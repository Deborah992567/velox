//! The streaming reverse-proxy exchange.
//!
//! One blocking exchange per request: obtain an upstream connection (from the
//! Phase 10 [`UpstreamPool`], or a fresh one via the Phase 9 connect path — see
//! [`proxy_exchange_pooled`] and [`proxy_exchange`]), pin the read/write
//! timeouts, relay the rewritten request head and body, read the upstream's
//! response head with [`ResponseParser`], relay `1xx` interim heads, and stream
//! the body back — decoding upstream `chunked` and re-encoding it for the
//! client so a close-delimited HTTP/1.0 upstream still produces a properly
//! framed response.
//!
//! Retries are strictly bounded by the phase contract: only bodyless,
//! idempotent requests, only up to [`ProxyOptions::retries`] extra attempts,
//! and only before any byte (interim head, final head, or body) reaches the
//! client.
//!
//! The transport is the blocking [`crate::net`] layer; the generic
//! `Read + Write` bounds keep the core testable against in-memory peers and
//! reusable for a TLS-wrapped upstream later.

use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;

use crate::http::http1::chunked::{ChunkedDecoder, ChunkedError, DecodeResult};
use crate::http::http1::engine::{
    EncodeError, encode_chunk, encode_head, encode_last_chunk, encode_request_head,
};
use crate::http::http1::response::{FeedResult, ResponseParseError, ResponseParser};
use crate::http::{
    BodyFraming, HeaderName, Headers, Method, Request, Response, StatusCode, Version,
};
use crate::net::{Connection, SocketTimeoutSide, connect_with_timeout, set_socket_timeout};
use crate::proxy::config::{ProxyOptions, ProxyTarget, UpstreamScheme};
use crate::proxy::pool::UpstreamPool;
use crate::proxy::rewrite::{
    RewriteError, rewrite_request_headers, rewrite_target, rewrite_ws_request_headers,
};
use crate::proxy::websocket::ws_relay;
use crate::websocket::handshake::{contains_token, is_websocket_upgrade};

use super::pool::PooledConnection;
use super::rewrite::strip_hop_by_hop;

/// How the exchange ended, from the client connection's point of view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxyOutcome {
    /// The response body was fully delimited; the client connection can be
    /// kept alive.
    Complete,
    /// The response was close-delimited (an HTTP/1.0 client with a
    /// non-length-framed upstream); the client connection must be closed
    /// after the exchange.
    CloseDelimited,
}

/// Why a proxied exchange failed.
#[derive(Debug)]
pub enum ExchangeError {
    /// The `proxy_pass` scheme is `https`; an outbound TLS client has not
    /// landed yet, so the target is rejected.
    HttpsUpstreamUnsupported,
    /// Every server in the upstream group is down (or backups only and they
    /// are down too); no peer could be selected.
    NoHealthyUpstream,
    /// Connecting to the upstream failed.
    Connect(io::Error),
    /// Reading from or writing to the upstream failed.
    Upstream(io::Error),
    /// The upstream closed before the response head completed.
    UpstreamEof,
    /// The upstream sent a malformed or oversized response head.
    UpstreamHead(ResponseParseError),
    /// The upstream's chunked body was malformed.
    UpstreamBody(ChunkedError),
    /// Reading from or writing to the client failed.
    Client(io::Error),
    /// The client closed before the request body was complete.
    ClientEof,
    /// The client's chunked request body was malformed.
    ClientBody(ChunkedError),
    /// A rewritten head could not be encoded (an injection vector).
    RequestEncode(EncodeError),
    /// The request target could not be rewritten.
    Rewrite(RewriteError),
    /// The response head (or an interim `1xx`) was already relayed to the
    /// client when the wrapped error occurred; the exchange must not be
    /// retried.
    Relayed(Box<Self>),
}

/// Size of one upstream/client read.
const BUFFER: usize = 16 * 1024;
/// Chunked decode scratch. Four times the read size guarantees a single read
/// can never fill it, so the decoder's `NeedMore` stalls are always "partial
/// line absorbed into the internal carry" and the unconsumed tail is dropped.
const SCRATCH: usize = BUFFER * 4;

/// A connection an exchange borrows, with a terminal "keep it alive or close
/// it" decision. The pooled form returns a reusable connection to the pool;
/// the direct form is a plain [`Connection`] whose end state does not matter.
trait UpstreamConnection {
    /// The underlying stream for the relay.
    fn conn_mut(&mut self) -> &mut Connection;

    /// Consume the connection after the exchange. `keepalive` is true only
    /// when the response body was fully consumed to a message boundary.
    fn finish(self, keepalive: bool);
}

impl UpstreamConnection for Connection {
    fn conn_mut(&mut self) -> &mut Connection {
        self
    }

    fn finish(self, _keepalive: bool) {}
}

/// A pooled upstream handle: forwards I/O to the guard and returns it to the
/// pool (or closes it) when the exchange decides its fate.
struct PooledUpstream<'a> {
    guard: PooledConnection<'a>,
}

impl UpstreamConnection for PooledUpstream<'_> {
    fn conn_mut(&mut self) -> &mut Connection {
        self.guard.conn_mut()
    }

    fn finish(mut self, keepalive: bool) {
        if keepalive {
            self.guard.mark_reusable();
        }
    }
}

/// Proxy one request end to end.
///
/// `matched_prefix` is the location prefix the request matched (`None` for a
/// regex or named-location match), `client_ip` feeds the forwarded-* headers,
/// and `proto` is `"http"` or `"https"` for `X-Forwarded-Proto`. A fresh
/// upstream connection is opened for the exchange.
pub fn proxy_exchange<C: Read + Write>(
    client: &mut C,
    request: &Request,
    matched_prefix: Option<&str>,
    target: &ProxyTarget,
    options: &ProxyOptions,
    client_ip: &str,
    proto: &str,
) -> Result<ProxyOutcome, ExchangeError> {
    proxy_exchange_core(
        client,
        request,
        matched_prefix,
        target,
        options,
        client_ip,
        proto,
        || connect_upstream(target, options),
    )
}

/// Proxy one request end to end, borrowing the upstream connection from `pool`.
///
/// Same exchange as [`proxy_exchange`], but the connection is drawn from and
/// returned to the keepalive pool: when the response body is fully consumed to
/// a message boundary the connection is reused by the next request to the
/// target, otherwise it is closed. Retry semantics are unchanged.
#[allow(clippy::too_many_arguments)]
pub fn proxy_exchange_pooled<C: Read + Write>(
    client: &mut C,
    request: &Request,
    matched_prefix: Option<&str>,
    target: &ProxyTarget,
    options: &ProxyOptions,
    pool: &UpstreamPool,
    client_ip: &str,
    proto: &str,
) -> Result<ProxyOutcome, ExchangeError> {
    proxy_exchange_core(
        client,
        request,
        matched_prefix,
        target,
        options,
        client_ip,
        proto,
        || {
            if target.scheme == UpstreamScheme::Https {
                return Err(ExchangeError::HttpsUpstreamUnsupported);
            }
            pool.borrow(target, options)
                .map(|guard| PooledUpstream { guard })
                .map_err(ExchangeError::Connect)
        },
    )
}

/// The retry loop shared by the direct and pooled exchanges: acquire an
/// upstream connection, relay the request, read the response, and report how
/// the exchange ended. A failed connection is dropped before the next attempt;
/// a fully-consumed pooled connection is returned to the pool.
#[allow(clippy::too_many_arguments)]
fn proxy_exchange_core<C, H, A>(
    client: &mut C,
    request: &Request,
    matched_prefix: Option<&str>,
    target: &ProxyTarget,
    options: &ProxyOptions,
    client_ip: &str,
    proto: &str,
    mut acquire: A,
) -> Result<ProxyOutcome, ExchangeError>
where
    C: Read + Write,
    H: UpstreamConnection,
    A: FnMut() -> Result<H, ExchangeError>,
{
    let retryable = request.framing == BodyFraming::None && request.method.is_idempotent();
    let mut attempts = options.retries.saturating_add(1);
    loop {
        let mut upstream = match acquire() {
            Ok(upstream) => upstream,
            Err(error) => {
                if retryable && attempts > 1 {
                    attempts -= 1;
                    continue;
                }
                return Err(error);
            }
        };
        if let Err(error) = relay_request(
            client,
            upstream.conn_mut(),
            request,
            matched_prefix,
            target,
            client_ip,
            proto,
        ) {
            if retryable && attempts > 1 {
                attempts -= 1;
                continue;
            }
            return Err(error);
        }
        let prepared = match prepare_response(client, upstream.conn_mut(), request) {
            Ok(prepared) => prepared,
            Err(error) => {
                if retryable && attempts > 1 && !matches!(error, ExchangeError::Relayed(_)) {
                    attempts -= 1;
                    continue;
                }
                return Err(error);
            }
        };
        if prepared.relay == BodyRelay::WsRelay {
            clear_ws_timeouts(client, upstream.conn_mut());
            ws_relay(client, upstream.conn_mut()).map_err(|e| {
                if e.kind() == io::ErrorKind::UnexpectedEof {
                    ExchangeError::UpstreamEof
                } else {
                    ExchangeError::Upstream(e)
                }
            })?;
            upstream.finish(false);
            return Ok(ProxyOutcome::Complete);
        }
        let outcome = relay_body(client, upstream.conn_mut(), prepared)?;
        upstream.finish(outcome == ProxyOutcome::Complete);
        return Ok(outcome);
    }
}

/// Connect to the upstream, applying the connect timeout and pinning the
/// blocking read/write timeouts for the exchange.
fn connect_upstream(
    target: &ProxyTarget,
    options: &ProxyOptions,
) -> Result<Connection, ExchangeError> {
    if target.scheme == UpstreamScheme::Https {
        return Err(ExchangeError::HttpsUpstreamUnsupported);
    }
    let conn = connect_with_timeout(&target.addr, options.connect_timeout)
        .map_err(ExchangeError::Connect)?;
    set_socket_timeout(
        conn.as_raw_fd(),
        SocketTimeoutSide::Read,
        Some(options.read_timeout),
    )
    .map_err(ExchangeError::Connect)?;
    set_socket_timeout(
        conn.as_raw_fd(),
        SocketTimeoutSide::Write,
        Some(options.send_timeout),
    )
    .map_err(ExchangeError::Connect)?;
    Ok(conn)
}

/// Rewrite the request, handle the client's `Expect: 100-continue`, and relay
/// the head and body upstream.
pub(crate) fn relay_request<C: Read + Write, U: Read + Write>(
    client: &mut C,
    upstream: &mut U,
    request: &Request,
    matched_prefix: Option<&str>,
    target: &ProxyTarget,
    client_ip: &str,
    proto: &str,
) -> Result<(), ExchangeError> {
    if request.expects_continue() && matches!(request.version, Version::Http11) {
        client
            .write_all(b"HTTP/1.1 100 Continue\r\n\r\n")
            .map_err(ExchangeError::Client)?;
    }
    let new_target =
        rewrite_target(request, matched_prefix, target).map_err(ExchangeError::Rewrite)?;
    let headers = if is_websocket_upgrade(request) {
        rewrite_ws_request_headers(request, target, client_ip, proto)
    } else {
        rewrite_request_headers(request, target, client_ip, proto)
    };
    let rewritten = Request::new(
        request.method.clone(),
        new_target,
        request.version,
        headers,
        request.framing,
    );
    let mut head = Vec::new();
    encode_request_head(&rewritten, &mut head).map_err(ExchangeError::RequestEncode)?;
    upstream.write_all(&head).map_err(ExchangeError::Upstream)?;
    relay_request_body(client, upstream, request.framing)
}

/// Relay the request body from the client to the upstream in its original
/// framing.
fn relay_request_body<C: Read + Write, U: Write>(
    client: &mut C,
    upstream: &mut U,
    framing: BodyFraming,
) -> Result<(), ExchangeError> {
    match framing {
        BodyFraming::None => Ok(()),
        BodyFraming::Length(len) => relay_client_fixed(client, upstream, len),
        BodyFraming::Chunked => relay_client_chunked(client, upstream),
    }
}

/// Copy exactly `remaining` bytes from the client to the upstream.
fn relay_client_fixed<C: Read, U: Write>(
    client: &mut C,
    upstream: &mut U,
    mut remaining: u64,
) -> Result<(), ExchangeError> {
    let mut buf = [0u8; BUFFER];
    while remaining > 0 {
        let want = usize::try_from(remaining)
            .expect("body length fits usize")
            .min(buf.len());
        let n = client
            .read(&mut buf[..want])
            .map_err(ExchangeError::Client)?;
        if n == 0 {
            return Err(ExchangeError::ClientEof);
        }
        upstream
            .write_all(&buf[..n])
            .map_err(ExchangeError::Upstream)?;
        remaining -= u64::try_from(n).expect("read count fits u64");
    }
    Ok(())
}

/// Decode the client's chunked body and re-encode it for the upstream.
fn relay_client_chunked<C: Read, U: Write>(
    client: &mut C,
    upstream: &mut U,
) -> Result<(), ExchangeError> {
    let mut decoder = ChunkedDecoder::new();
    let mut inbuf = [0u8; BUFFER];
    let mut outbuf = vec![0u8; SCRATCH];
    loop {
        let n = client.read(&mut inbuf).map_err(ExchangeError::Client)?;
        if n == 0 {
            return Err(ExchangeError::ClientEof);
        }
        match decoder.feed(&inbuf[..n], &mut outbuf) {
            DecodeResult::Done { produced, .. } => {
                if produced > 0 {
                    let mut frame = Vec::new();
                    encode_chunk(&mut frame, &outbuf[..produced]);
                    upstream
                        .write_all(&frame)
                        .map_err(ExchangeError::Upstream)?;
                }
                let trailers = decoder.take_trailers();
                let mut last = Vec::new();
                encode_last_chunk(&mut last, &trailers).map_err(ExchangeError::RequestEncode)?;
                upstream.write_all(&last).map_err(ExchangeError::Upstream)?;
                return Ok(());
            }
            DecodeResult::Error(error) => return Err(ExchangeError::ClientBody(error)),
            DecodeResult::NeedMore { produced, .. } => {
                if produced > 0 {
                    let mut frame = Vec::new();
                    encode_chunk(&mut frame, &outbuf[..produced]);
                    upstream
                        .write_all(&frame)
                        .map_err(ExchangeError::Upstream)?;
                }
            }
        }
    }
}

/// Read the upstream's response head, relaying interim `1xx` heads to the
/// client, and prepare the body relay.
pub(crate) fn prepare_response<C: Read + Write, U: Read + Write>(
    client: &mut C,
    upstream: &mut U,
    request: &Request,
) -> Result<PreparedResponse, ExchangeError> {
    let mut parser = ResponseParser::new();
    let mut pending = Vec::new();
    let mut inbuf = [0u8; BUFFER];
    let mut sent = false;
    let head = loop {
        if pending.is_empty() {
            match upstream.read(&mut inbuf) {
                Ok(0) => return Err(wrap_if(sent, ExchangeError::UpstreamEof)),
                Ok(n) => pending.extend_from_slice(&inbuf[..n]),
                Err(error) => return Err(wrap_if(sent, ExchangeError::Upstream(error))),
            }
        }
        match parser.feed(&pending) {
            FeedResult::Incomplete => pending.clear(),
            FeedResult::Complete(parsed) => {
                let rest = parser.drain_pending();
                if parsed.status.code() < 200 && parsed.status.code() != 101 {
                    if matches!(request.version, Version::Http11) {
                        let mut out = Vec::new();
                        encode_head(
                            &Response::new(Version::Http11, parsed.status),
                            BodyFraming::None,
                            &mut out,
                        )
                        .map_err(ExchangeError::RequestEncode)?;
                        client.write_all(&out).map_err(ExchangeError::Client)?;
                        sent = true;
                    }
                    parser.reset();
                    pending = rest;
                    continue;
                }
                pending = rest;
                break parsed;
            }
            FeedResult::Error(error) => {
                return Err(wrap_if(sent, ExchangeError::UpstreamHead(error)));
            }
        }
    };

    if is_ws_upgrade_response(&head) {
        let mut out = Vec::new();
        encode_head(
            &Response::new(request.version, head.status),
            BodyFraming::None,
            &mut out,
        )
        .map_err(ExchangeError::RequestEncode)?;
        client.write_all(&out).map_err(ExchangeError::Client)?;
        return Ok(PreparedResponse {
            relay: BodyRelay::WsRelay,
            pending: Vec::new(),
        });
    }

    let relay = relay_body_mode(request.version, &head, &request.method);
    let mut relayed = Response::new(request.version, head.status);
    relayed.headers = strip_hop_by_hop(&head.headers);
    if matches!(relay, BodyRelay::DecodeRaw | BodyRelay::RawToClose) {
        relayed.headers.push_value(HeaderName::Connection, "close");
    }
    if matches!(relay, BodyRelay::None) {
        // A HEAD/204/304 response carries no body, but the upstream's framing
        // headers describe the hypothetical body and must be preserved.
        match head.framing {
            BodyFraming::Length(len) => {
                relayed
                    .headers
                    .push_value(HeaderName::ContentLength, len.to_string());
            }
            BodyFraming::Chunked => {
                relayed
                    .headers
                    .push_value(HeaderName::TransferEncoding, "chunked");
            }
            BodyFraming::None => {}
        }
    }
    let framing = match relay {
        BodyRelay::Fixed(len) => BodyFraming::Length(len),
        BodyRelay::DecodeChunked | BodyRelay::EncodeRawChunked => BodyFraming::Chunked,
        BodyRelay::None | BodyRelay::DecodeRaw | BodyRelay::RawToClose | BodyRelay::WsRelay => {
            BodyFraming::None
        }
    };
    let mut out = Vec::new();
    encode_head(&relayed, framing, &mut out).map_err(ExchangeError::RequestEncode)?;
    client.write_all(&out).map_err(ExchangeError::Client)?;
    Ok(PreparedResponse { relay, pending })
}

/// Wrap an error as [`ExchangeError::Relayed`] when response bytes have
/// already reached the client.
fn wrap_if(sent: bool, error: ExchangeError) -> ExchangeError {
    if sent {
        ExchangeError::Relayed(Box::new(error))
    } else {
        error
    }
}

/// Clear socket timeouts on both sides of a WebSocket relay so idle
/// connections do not time out. Best-effort; ignores errors.
#[allow(clippy::needless_pass_by_ref_mut)]
pub(crate) fn clear_ws_timeouts<C: Read + Write, U: Read + Write + AsRawFd>(
    _client: &mut C,
    upstream: &mut U,
) {
    let _ = set_socket_timeout(upstream.as_raw_fd(), SocketTimeoutSide::Read, None);
    let _ = set_socket_timeout(upstream.as_raw_fd(), SocketTimeoutSide::Write, None);
}

/// Whether the upstream responded with `101 Switching Protocols` for a
/// WebSocket upgrade (`Upgrade: websocket` header present).
fn is_ws_upgrade_response(head: &crate::http::http1::response::ResponseHead) -> bool {
    head.status == StatusCode::SWITCHING_PROTOCOLS
        && head
            .headers
            .get(&HeaderName::Upgrade)
            .is_some_and(|value| contains_token(value, "websocket"))
}

/// The response body relay plan, chosen from the client's HTTP version and the
/// upstream's framing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BodyRelay {
    /// No body (HEAD, 204, 304, 1xx-final).
    None,
    /// Upstream `Content-Length: n`; relay exactly `n` bytes.
    Fixed(u64),
    /// HTTP/1.1 client, upstream chunked; decode and re-encode chunked.
    DecodeChunked,
    /// HTTP/1.1 client, upstream close-delimited; chunk-encode the raw body.
    EncodeRawChunked,
    /// HTTP/1.0 client, upstream chunked; decode and relay raw, then close.
    DecodeRaw,
    /// HTTP/1.0 client, upstream close-delimited; relay raw, then close.
    RawToClose,
    /// WebSocket upgrade: bidirectional relay after a 101 response.
    WsRelay,
}

/// How to relay the body for a (client version, upstream framing) pair.
const fn relay_body_mode(
    version: Version,
    head: &crate::http::http1::response::ResponseHead,
    method: &Method,
) -> BodyRelay {
    if !head.has_body(method) {
        return BodyRelay::None;
    }
    match (version, head.framing) {
        (_, BodyFraming::Length(len)) => BodyRelay::Fixed(len),
        (Version::Http11, BodyFraming::Chunked) => BodyRelay::DecodeChunked,
        (Version::Http11, BodyFraming::None) => BodyRelay::EncodeRawChunked,
        (Version::Http10, BodyFraming::Chunked) => BodyRelay::DecodeRaw,
        (Version::Http10, BodyFraming::None) => BodyRelay::RawToClose,
    }
}

/// A read source that yields the response head's unconsumed tail before the
/// upstream socket.
struct Feed<'a, U> {
    pending: Vec<u8>,
    pos: usize,
    upstream: &'a mut U,
}

impl<U: Read> Read for Feed<'_, U> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.pos < self.pending.len() {
            let n = (self.pending.len() - self.pos).min(buf.len());
            buf[..n].copy_from_slice(&self.pending[self.pos..self.pos + n]);
            self.pos += n;
            return Ok(n);
        }
        self.upstream.read(buf)
    }
}

/// The response body relay in progress.
pub(crate) struct PreparedResponse {
    pub(crate) relay: BodyRelay,
    pending: Vec<u8>,
}

/// Relay the response body and report how the exchange must end.
pub(crate) fn relay_body<C: Read + Write, U: Read + Write>(
    client: &mut C,
    upstream: &mut U,
    prepared: PreparedResponse,
) -> Result<ProxyOutcome, ExchangeError> {
    let mut feed = Feed {
        pending: prepared.pending,
        pos: 0,
        upstream,
    };
    match prepared.relay {
        BodyRelay::None => Ok(ProxyOutcome::Complete),
        BodyRelay::WsRelay => unreachable!("handled by caller"),
        BodyRelay::Fixed(len) => {
            relay_upstream_fixed(&mut feed, client, len)?;
            Ok(ProxyOutcome::Complete)
        }
        BodyRelay::DecodeChunked => {
            relay_upstream_chunked(&mut feed, client)?;
            Ok(ProxyOutcome::Complete)
        }
        BodyRelay::EncodeRawChunked => {
            relay_upstream_raw_chunked(&mut feed, client)?;
            Ok(ProxyOutcome::Complete)
        }
        BodyRelay::DecodeRaw => {
            relay_upstream_decoded_raw(&mut feed, client)?;
            Ok(ProxyOutcome::CloseDelimited)
        }
        BodyRelay::RawToClose => {
            relay_upstream_raw(&mut feed, client)?;
            Ok(ProxyOutcome::CloseDelimited)
        }
    }
}

/// Copy exactly `remaining` bytes from the upstream to the client.
fn relay_upstream_fixed<S: Read, C: Write>(
    source: &mut S,
    client: &mut C,
    mut remaining: u64,
) -> Result<(), ExchangeError> {
    let mut buf = [0u8; BUFFER];
    while remaining > 0 {
        let want = usize::try_from(remaining)
            .expect("body length fits usize")
            .min(buf.len());
        let n = source
            .read(&mut buf[..want])
            .map_err(ExchangeError::Upstream)?;
        if n == 0 {
            return Err(ExchangeError::UpstreamEof);
        }
        client.write_all(&buf[..n]).map_err(ExchangeError::Client)?;
        remaining -= u64::try_from(n).expect("read count fits u64");
    }
    Ok(())
}

/// Decode the upstream's chunked body and re-encode it for the client,
/// relaying any trailers in the terminating chunk.
fn relay_upstream_chunked<S: Read, C: Write>(
    source: &mut S,
    client: &mut C,
) -> Result<(), ExchangeError> {
    let mut decoder = ChunkedDecoder::new();
    let mut inbuf = [0u8; BUFFER];
    let mut outbuf = vec![0u8; SCRATCH];
    loop {
        let n = source.read(&mut inbuf).map_err(ExchangeError::Upstream)?;
        if n == 0 {
            return Err(ExchangeError::UpstreamEof);
        }
        match decoder.feed(&inbuf[..n], &mut outbuf) {
            DecodeResult::Done { produced, .. } => {
                if produced > 0 {
                    let mut frame = Vec::new();
                    encode_chunk(&mut frame, &outbuf[..produced]);
                    client.write_all(&frame).map_err(ExchangeError::Client)?;
                }
                let trailers = decoder.take_trailers();
                let mut last = Vec::new();
                encode_last_chunk(&mut last, &trailers).map_err(ExchangeError::RequestEncode)?;
                client.write_all(&last).map_err(ExchangeError::Client)?;
                return Ok(());
            }
            DecodeResult::Error(error) => return Err(ExchangeError::UpstreamBody(error)),
            DecodeResult::NeedMore { produced, .. } => {
                if produced > 0 {
                    let mut frame = Vec::new();
                    encode_chunk(&mut frame, &outbuf[..produced]);
                    client.write_all(&frame).map_err(ExchangeError::Client)?;
                }
            }
        }
    }
}

/// Chunk-encode a close-delimited upstream body for an HTTP/1.1 client.
fn relay_upstream_raw_chunked<S: Read, C: Write>(
    source: &mut S,
    client: &mut C,
) -> Result<(), ExchangeError> {
    let mut buf = [0u8; BUFFER];
    loop {
        let n = source.read(&mut buf).map_err(ExchangeError::Upstream)?;
        if n == 0 {
            let mut last = Vec::new();
            encode_last_chunk(&mut last, &Headers::new()).map_err(ExchangeError::RequestEncode)?;
            client.write_all(&last).map_err(ExchangeError::Client)?;
            return Ok(());
        }
        let mut frame = Vec::new();
        encode_chunk(&mut frame, &buf[..n]);
        client.write_all(&frame).map_err(ExchangeError::Client)?;
    }
}

/// Decode the upstream's chunked body and relay the raw bytes to an HTTP/1.0
/// client, which the caller closes afterwards.
fn relay_upstream_decoded_raw<S: Read, C: Write>(
    source: &mut S,
    client: &mut C,
) -> Result<(), ExchangeError> {
    let mut decoder = ChunkedDecoder::new();
    let mut inbuf = [0u8; BUFFER];
    let mut outbuf = vec![0u8; SCRATCH];
    loop {
        let n = source.read(&mut inbuf).map_err(ExchangeError::Upstream)?;
        if n == 0 {
            return Err(ExchangeError::UpstreamEof);
        }
        match decoder.feed(&inbuf[..n], &mut outbuf) {
            DecodeResult::Done { produced, .. } => {
                if produced > 0 {
                    client
                        .write_all(&outbuf[..produced])
                        .map_err(ExchangeError::Client)?;
                }
                return Ok(());
            }
            DecodeResult::Error(error) => return Err(ExchangeError::UpstreamBody(error)),
            DecodeResult::NeedMore { produced, .. } => {
                if produced > 0 {
                    client
                        .write_all(&outbuf[..produced])
                        .map_err(ExchangeError::Client)?;
                }
            }
        }
    }
}

/// Relay a close-delimited upstream body to an HTTP/1.0 client.
fn relay_upstream_raw<S: Read, C: Write>(
    source: &mut S,
    client: &mut C,
) -> Result<(), ExchangeError> {
    let mut buf = [0u8; BUFFER];
    loop {
        let n = source.read(&mut buf).map_err(ExchangeError::Upstream)?;
        if n == 0 {
            return Ok(());
        }
        client.write_all(&buf[..n]).map_err(ExchangeError::Client)?;
    }
}

#[cfg(test)]
mod tests {
    use super::{BodyRelay, ProxyOutcome, prepare_response, relay_body_mode, relay_request};
    use crate::http::{BodyFraming, HeaderName, Headers, Method, Request, StatusCode, Version};
    use crate::net::InetAddr;
    use crate::proxy::config::{ProxyOptions, ProxyTarget};
    use std::io::{self, Read, Write};

    /// An in-memory peer: serves `input` on reads and records `output`.
    #[derive(Debug)]
    struct TestIo {
        input: Vec<u8>,
        pos: usize,
        output: Vec<u8>,
    }

    impl TestIo {
        fn new(input: &[u8]) -> Self {
            Self {
                input: input.to_vec(),
                pos: 0,
                output: Vec::new(),
            }
        }
    }

    impl Read for TestIo {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let n = (self.input.len() - self.pos).min(buf.len());
            buf[..n].copy_from_slice(&self.input[self.pos..self.pos + n]);
            self.pos += n;
            Ok(n)
        }
    }

    impl Write for TestIo {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.output.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn get_request() -> Request {
        Request::new(
            Method::Get,
            b"/api/foo?x=1".to_vec(),
            Version::Http11,
            Headers::new(),
            BodyFraming::None,
        )
    }

    fn unix_target() -> ProxyTarget {
        ProxyTarget::http(InetAddr::Unix("/tmp/up.sock".into())).with_uri_prefix(b"/v1")
    }

    #[test]
    fn relay_request_rewrites_target_and_headers() {
        let mut headers = Headers::new();
        headers.push_value(HeaderName::Host, "example.com");
        headers.push_value(HeaderName::Connection, "keep-alive");
        headers.push_value(HeaderName::UserAgent, "curl");
        let request = Request::new(
            Method::Get,
            b"/api/foo?x=1".to_vec(),
            Version::Http11,
            headers,
            BodyFraming::None,
        );
        let mut client = TestIo::new(b"");
        let mut upstream = TestIo::new(b"");
        relay_request(
            &mut client,
            &mut upstream,
            &request,
            Some("/api"),
            &unix_target(),
            "10.0.0.1",
            "http",
        )
        .unwrap();
        let out = String::from_utf8(upstream.output).unwrap();
        assert!(out.starts_with("GET /v1/foo?x=1 HTTP/1.1\r\n"), "{out}");
        assert!(out.contains("host: localhost\r\n"), "{out}");
        assert!(out.contains("user-agent: curl\r\n"), "{out}");
        assert!(out.contains("x-forwarded-for: 10.0.0.1\r\n"), "{out}");
        assert!(out.contains("x-real-ip: 10.0.0.1\r\n"), "{out}");
        assert!(out.contains("x-forwarded-proto: http\r\n"), "{out}");
        assert!(!out.contains("connection:"), "{out}");
        assert!(!out.contains("expect:"), "{out}");
    }

    #[test]
    fn relay_request_sends_100_continue_and_fixed_body() {
        let mut headers = Headers::new();
        headers.push_value(HeaderName::Expect, "100-continue");
        let request = Request::new(
            Method::Post,
            b"/api/up".to_vec(),
            Version::Http11,
            headers,
            BodyFraming::Length(5),
        );
        let mut client = TestIo::new(b"hello");
        let mut upstream = TestIo::new(b"");
        relay_request(
            &mut client,
            &mut upstream,
            &request,
            Some("/api"),
            &unix_target(),
            "10.0.0.1",
            "http",
        )
        .unwrap();
        assert_eq!(client.output, b"HTTP/1.1 100 Continue\r\n\r\n");
        let out = upstream.output;
        assert!(out.windows(4).any(|w| w == b"\r\n\r\n"), "{out:?}");
        let head_end = out.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
        assert!(out[..head_end].ends_with(b"content-length: 5\r\n\r\n"));
        assert_eq!(&out[head_end..], b"hello");
        assert!(!String::from_utf8_lossy(&out).contains("expect:"));
    }

    #[test]
    fn relay_request_reencodes_chunked_body() {
        let request = Request::new(
            Method::Post,
            b"/api/up".to_vec(),
            Version::Http11,
            Headers::new(),
            BodyFraming::Chunked,
        );
        let mut client = TestIo::new(b"5\r\nhello\r\n0\r\n\r\n");
        let mut upstream = TestIo::new(b"");
        relay_request(
            &mut client,
            &mut upstream,
            &request,
            Some("/api"),
            &unix_target(),
            "10.0.0.1",
            "http",
        )
        .unwrap();
        assert!(upstream.output.ends_with(b"5\r\nhello\r\n0\r\n\r\n"));
    }

    #[test]
    fn relays_length_framed_response() {
        let mut upstream = TestIo::new(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello");
        let mut client = TestIo::new(b"");
        let prepared = prepare_response(&mut client, &mut upstream, &get_request()).unwrap();
        assert_eq!(prepared.relay, BodyRelay::Fixed(5));
        assert_eq!(
            client.output,
            b"HTTP/1.1 200 OK\r\ncontent-length: 5\r\n\r\n"
        );
        let outcome = super::relay_body(&mut client, &mut upstream, prepared).unwrap();
        assert_eq!(outcome, ProxyOutcome::Complete);
        assert_eq!(
            client.output,
            b"HTTP/1.1 200 OK\r\ncontent-length: 5\r\n\r\nhello"
        );
    }

    #[test]
    fn relays_chunked_response_pending_and_trailers() {
        let wire = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n0\r\nX-Foo: bar\r\n\r\n";
        let mut upstream = TestIo::new(wire);
        let mut client = TestIo::new(b"");
        let prepared = prepare_response(&mut client, &mut upstream, &get_request()).unwrap();
        assert_eq!(prepared.relay, BodyRelay::DecodeChunked);
        super::relay_body(&mut client, &mut upstream, prepared).unwrap();
        assert_eq!(
            client.output,
            b"HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\n\r\n5\r\nhello\r\n0\r\nx-foo: bar\r\n\r\n"
        );
    }

    #[test]
    fn relays_interim_100_before_final_head() {
        let wire = b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nhi";
        let mut upstream = TestIo::new(wire);
        let mut client = TestIo::new(b"");
        let prepared = prepare_response(&mut client, &mut upstream, &get_request()).unwrap();
        assert_eq!(prepared.relay, BodyRelay::Fixed(2));
        super::relay_body(&mut client, &mut upstream, prepared).unwrap();
        assert_eq!(
            client.output,
            b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nhi"
        );
    }

    #[test]
    fn head_response_relays_framing_but_no_body() {
        let mut headers = Headers::new();
        headers.push_value(HeaderName::Host, "example.com");
        let request = Request::new(
            Method::Head,
            b"/".to_vec(),
            Version::Http11,
            headers,
            BodyFraming::None,
        );
        let wire = b"HTTP/1.1 200 OK\r\nContent-Length: 9\r\n\r\nnot-a-body";
        let mut upstream = TestIo::new(wire);
        let mut client = TestIo::new(b"");
        let prepared = prepare_response(&mut client, &mut upstream, &request).unwrap();
        assert_eq!(prepared.relay, BodyRelay::None);
        let outcome = super::relay_body(&mut client, &mut upstream, prepared).unwrap();
        assert_eq!(outcome, ProxyOutcome::Complete);
        assert_eq!(
            client.output,
            b"HTTP/1.1 200 OK\r\ncontent-length: 9\r\n\r\n"
        );
    }

    #[test]
    fn http10_client_gets_close_delimited_chunked_body() {
        let mut headers = Headers::new();
        headers.push_value(HeaderName::Host, "example.com");
        let request = Request::new(
            Method::Get,
            b"/".to_vec(),
            Version::Http10,
            headers,
            BodyFraming::None,
        );
        let wire = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n0\r\n\r\n";
        let mut upstream = TestIo::new(wire);
        let mut client = TestIo::new(b"");
        let prepared = prepare_response(&mut client, &mut upstream, &request).unwrap();
        assert_eq!(prepared.relay, BodyRelay::DecodeRaw);
        let outcome = super::relay_body(&mut client, &mut upstream, prepared).unwrap();
        assert_eq!(outcome, ProxyOutcome::CloseDelimited);
        assert_eq!(
            client.output,
            b"HTTP/1.0 200 OK\r\nconnection: close\r\n\r\nhello"
        );
    }

    #[test]
    fn close_delimited_upstream_is_chunk_encoded_for_http11() {
        let wire = b"HTTP/1.1 200 OK\r\n\r\nhello";
        let mut upstream = TestIo::new(wire);
        let mut client = TestIo::new(b"");
        let prepared = prepare_response(&mut client, &mut upstream, &get_request()).unwrap();
        assert_eq!(prepared.relay, BodyRelay::EncodeRawChunked);
        let outcome = super::relay_body(&mut client, &mut upstream, prepared).unwrap();
        assert_eq!(outcome, ProxyOutcome::Complete);
        assert_eq!(
            client.output,
            b"HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\n\r\n5\r\nhello\r\n0\r\n\r\n"
        );
    }

    #[test]
    fn rejects_https_upstreams() {
        let target = crate::proxy::config::parse_proxy_pass("https://93.184.216.34").unwrap();
        let mut client = TestIo::new(b"");
        let error = super::proxy_exchange(
            &mut client,
            &get_request(),
            Some("/api"),
            &target,
            &ProxyOptions::default(),
            "10.0.0.1",
            "http",
        )
        .unwrap_err();
        assert!(matches!(
            error,
            super::ExchangeError::HttpsUpstreamUnsupported
        ));
    }

    #[test]
    fn bodyless_retryable_methods() {
        let target = unix_target();
        let _ = &target;
        assert!(Method::Get.is_idempotent());
        assert!(Method::Head.is_idempotent());
        assert!(Method::Put.is_idempotent());
        assert!(!Method::Post.is_idempotent());
        assert!(!Method::Patch.is_idempotent());
    }

    #[test]
    fn relay_mode_never_body_for_head() {
        let head = crate::http::http1::response::ResponseHead {
            version: Version::Http11,
            status: StatusCode::OK,
            headers: Headers::new(),
            framing: BodyFraming::Length(5),
        };
        assert_eq!(
            relay_body_mode(Version::Http11, &head, &Method::Head),
            BodyRelay::None
        );
        assert_eq!(
            relay_body_mode(Version::Http11, &head, &Method::Get),
            BodyRelay::Fixed(5)
        );
    }

    /// Serve one request head (until the blank line) and respond, returning
    /// the bytes received. Used to drive real Unix-socket upstreams.
    fn serve_once(listener: &crate::net::Listener, response: &[u8]) -> Vec<u8> {
        let mut conn = listener.accept().expect("accept");
        let mut buf = Vec::new();
        let mut tmp = [0u8; 1024];
        while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
            let n = conn.read(&mut tmp).expect("read request");
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&tmp[..n]);
        }
        conn.write_all(response).expect("write response");
        buf
    }

    #[test]
    fn proxies_end_to_end_over_a_unix_socket() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("upstream.sock");
        let listener = crate::net::Listener::bind(
            &InetAddr::Unix(path.clone()),
            crate::net::SocketOptions::new(),
        )
        .unwrap();
        let received = std::thread::spawn(move || {
            serve_once(
                &listener,
                b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello",
            )
        });

        let target = ProxyTarget::http(InetAddr::Unix(path)).with_uri_prefix(b"/v1");
        let mut client = TestIo::new(b"");
        let request = Request::new(
            Method::Get,
            b"/api/foo?x=1".to_vec(),
            Version::Http11,
            Headers::new(),
            BodyFraming::None,
        );
        let outcome = super::proxy_exchange(
            &mut client,
            &request,
            Some("/api"),
            &target,
            &ProxyOptions::default(),
            "10.0.0.1",
            "http",
        )
        .unwrap();
        assert_eq!(outcome, ProxyOutcome::Complete);
        assert_eq!(
            client.output,
            b"HTTP/1.1 200 OK\r\ncontent-length: 5\r\n\r\nhello"
        );
        let upstream_request = received.join().unwrap();
        assert!(upstream_request.starts_with(b"GET /v1/foo?x=1 HTTP/1.1\r\n"));
        assert!(upstream_request.windows(4).any(|w| w == b"\r\n\r\n"));
    }

    #[test]
    fn retries_bodyless_get_when_first_upstream_closes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("retry.sock");
        let listener = crate::net::Listener::bind(
            &InetAddr::Unix(path.clone()),
            crate::net::SocketOptions::new(),
        )
        .unwrap();
        let responses = vec![
            b"".to_vec(), // first attempt: accept then close immediately
            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok".to_vec(),
        ];
        let received = std::thread::spawn(move || {
            for response in responses {
                if response.is_empty() {
                    let conn = listener.accept().expect("accept");
                    drop(conn);
                } else {
                    serve_once(&listener, &response);
                }
            }
        });

        let target = ProxyTarget::http(InetAddr::Unix(path));
        let mut client = TestIo::new(b"");
        let request = Request::new(
            Method::Get,
            b"/".to_vec(),
            Version::Http11,
            Headers::new(),
            BodyFraming::None,
        );
        let options = ProxyOptions {
            retries: 1,
            ..ProxyOptions::default()
        };
        let outcome = super::proxy_exchange(
            &mut client,
            &request,
            None,
            &target,
            &options,
            "10.0.0.1",
            "http",
        )
        .unwrap();
        assert_eq!(outcome, ProxyOutcome::Complete);
        assert_eq!(
            client.output,
            b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nok"
        );
        received.join().unwrap();
    }

    #[test]
    fn does_not_retry_bodyless_post() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("noretry.sock");
        let listener = crate::net::Listener::bind(
            &InetAddr::Unix(path.clone()),
            crate::net::SocketOptions::new(),
        )
        .unwrap();
        let received = std::thread::spawn(move || {
            let mut conn = listener.accept().expect("accept");
            let mut buf = Vec::new();
            let mut tmp = [0u8; 1024];
            while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                let n = conn.read(&mut tmp).expect("read");
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&tmp[..n]);
            }
            drop(conn);
            buf
        });

        let target = ProxyTarget::http(InetAddr::Unix(path));
        let mut client = TestIo::new(b"");
        let request = Request::new(
            Method::Post,
            b"/".to_vec(),
            Version::Http11,
            Headers::new(),
            BodyFraming::None,
        );
        let options = ProxyOptions {
            retries: 1,
            ..ProxyOptions::default()
        };
        let error = super::proxy_exchange(
            &mut client,
            &request,
            None,
            &target,
            &options,
            "10.0.0.1",
            "http",
        )
        .unwrap_err();
        assert!(matches!(error, super::ExchangeError::UpstreamEof));
        received.join().unwrap();
    }

    /// Serve `count` requests on a single keep-alive connection, then close
    /// it. Returns the number of connections the server had to accept.
    fn serve_keepalive(listener: &crate::net::Listener, count: usize) {
        let mut conn = listener.accept().expect("accept");
        for _ in 0..count {
            let mut buf = Vec::new();
            let mut tmp = [0u8; 1024];
            while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                let n = conn.read(&mut tmp).expect("read request");
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&tmp[..n]);
            }
            conn.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                .expect("write response");
        }
    }

    #[test]
    fn pooled_exchange_reuses_one_upstream_connection() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pooled.sock");
        let listener = crate::net::Listener::bind(
            &InetAddr::Unix(path.clone()),
            crate::net::SocketOptions::new(),
        )
        .unwrap();
        let server = std::thread::spawn(move || serve_keepalive(&listener, 3));

        let target = ProxyTarget::http(InetAddr::Unix(path));
        let pool = crate::proxy::UpstreamPool::default();
        let options = ProxyOptions::default();
        for _ in 0..3 {
            let mut client = TestIo::new(b"");
            let request = Request::new(
                Method::Get,
                b"/".to_vec(),
                Version::Http11,
                Headers::new(),
                BodyFraming::None,
            );
            let outcome = super::proxy_exchange_pooled(
                &mut client,
                &request,
                None,
                &target,
                &options,
                &pool,
                "10.0.0.1",
                "http",
            )
            .unwrap();
            assert_eq!(outcome, ProxyOutcome::Complete);
            assert_eq!(
                client.output,
                b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nok"
            );
        }
        // Three requests, one keepalive connection: the pool kept it and the
        // server never saw a second accept.
        assert_eq!(pool.total(), 1);
        assert_eq!(pool.idle_len(), 1);
        drop(pool);
        server.join().unwrap();
    }

    #[test]
    fn pooled_exchange_closes_connection_after_close_delimited_body() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pooled-close.sock");
        let listener = crate::net::Listener::bind(
            &InetAddr::Unix(path.clone()),
            crate::net::SocketOptions::new(),
        )
        .unwrap();
        let received = std::thread::spawn(move || {
            let mut conn = listener.accept().expect("accept");
            let mut tmp = [0u8; 1024];
            let mut buf = Vec::new();
            while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                let n = conn.read(&mut tmp).expect("read");
                buf.extend_from_slice(&tmp[..n]);
            }
            conn.write_all(b"HTTP/1.1 200 OK\r\n\r\nhello")
                .expect("write");
            drop(conn);
        });

        let target = ProxyTarget::http(InetAddr::Unix(path));
        let pool = crate::proxy::UpstreamPool::default();
        let mut client = TestIo::new(b"");
        let mut headers = Headers::new();
        headers.push_value(HeaderName::Host, "example.com");
        let request = Request::new(
            Method::Get,
            b"/".to_vec(),
            Version::Http10,
            headers,
            BodyFraming::None,
        );
        let outcome = super::proxy_exchange_pooled(
            &mut client,
            &request,
            None,
            &target,
            &ProxyOptions::default(),
            &pool,
            "10.0.0.1",
            "http",
        )
        .unwrap();
        assert_eq!(outcome, ProxyOutcome::CloseDelimited);
        assert_eq!(
            client.output,
            b"HTTP/1.0 200 OK\r\nconnection: close\r\n\r\nhello"
        );
        // A close-delimited response consumed the connection; nothing is kept.
        assert_eq!(pool.idle_len(), 0);
        assert_eq!(pool.total(), 0);
        received.join().unwrap();
    }

    #[test]
    fn pooled_exchange_drops_broken_connection_and_retries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pooled-retry.sock");
        let listener = crate::net::Listener::bind(
            &InetAddr::Unix(path.clone()),
            crate::net::SocketOptions::new(),
        )
        .unwrap();
        let responses = vec![
            b"".to_vec(),
            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok".to_vec(),
        ];
        let received = std::thread::spawn(move || {
            for response in responses {
                if response.is_empty() {
                    let conn = listener.accept().expect("accept");
                    drop(conn);
                } else {
                    serve_once(&listener, &response);
                }
            }
        });

        let target = ProxyTarget::http(InetAddr::Unix(path));
        let pool = crate::proxy::UpstreamPool::default();
        let mut client = TestIo::new(b"");
        let request = Request::new(
            Method::Get,
            b"/".to_vec(),
            Version::Http11,
            Headers::new(),
            BodyFraming::None,
        );
        let options = ProxyOptions {
            retries: 1,
            ..ProxyOptions::default()
        };
        let outcome = super::proxy_exchange_pooled(
            &mut client,
            &request,
            None,
            &target,
            &options,
            &pool,
            "10.0.0.1",
            "http",
        )
        .unwrap();
        assert_eq!(outcome, ProxyOutcome::Complete);
        assert_eq!(
            client.output,
            b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nok"
        );
        assert_eq!(pool.idle_len(), 1);
        received.join().unwrap();
    }

    fn ws_upgrade_request() -> Request {
        let mut headers = Headers::new();
        headers.push_value(HeaderName::Host, "example.com");
        headers.push_value(HeaderName::Connection, "Upgrade");
        headers.push_value(HeaderName::Upgrade, "websocket");
        headers.push_value(HeaderName::Custom("sec-websocket-version".into()), "13");
        headers.push_value(
            HeaderName::Custom("sec-websocket-key".into()),
            "dGhlIHNhbXBsZSBub25jZQ==",
        );
        Request::new(
            Method::Get,
            b"/ws".to_vec(),
            Version::Http11,
            headers,
            BodyFraming::None,
        )
    }

    #[test]
    fn ws_upgrade_relay_preserves_upgrade_headers_upstream() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ws-upgrade.sock");
        let listener = crate::net::Listener::bind(
            &InetAddr::Unix(path.clone()),
            crate::net::SocketOptions::new(),
        )
        .unwrap();
        let server = std::thread::spawn(move || {
            let mut conn = listener.accept().expect("accept");
            let mut buf = Vec::new();
            let mut tmp = [0u8; 1024];
            while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                let n = conn.read(&mut tmp).expect("read");
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&tmp[..n]);
            }
            let head = String::from_utf8_lossy(&buf);
            assert!(
                head.contains("upgrade: websocket\r\n"),
                "must forward upgrade header: {head}"
            );
            assert!(
                head.contains("connection: Upgrade\r\n"),
                "must forward connection upgrade: {head}"
            );
            assert!(
                head.contains("sec-websocket-version: 13\r\n"),
                "must forward version: {head}"
            );
            assert!(
                head.contains("sec-websocket-key: dGhlIHNhbXBsZSBub25jZQ==\r\n"),
                "must forward key unchanged: {head}"
            );
            conn.write_all(
                b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\r\n",
            )
            .unwrap();
            // Echo back raw bytes
            let mut buf = [0u8; 64];
            if let Ok(n) = conn.read(&mut buf) {
                let _ = conn.write_all(&buf[..n]);
            }
        });

        let target = ProxyTarget::http(InetAddr::Unix(path));
        let mut client = TestIo::new(b"");
        let request = ws_upgrade_request();
        let outcome = super::proxy_exchange(
            &mut client,
            &request,
            None,
            &target,
            &ProxyOptions::default(),
            "10.0.0.1",
            "http",
        )
        .unwrap();
        assert_eq!(outcome, ProxyOutcome::Complete);
        let response = String::from_utf8_lossy(&client.output);
        assert!(
            response.contains("HTTP/1.1 101"),
            "client must see 101: {response}"
        );
        server.join().unwrap();
    }

    #[test]
    fn ws_upgrade_relay_does_not_try_when_upstream_rejects() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ws-reject.sock");
        let listener = crate::net::Listener::bind(
            &InetAddr::Unix(path.clone()),
            crate::net::SocketOptions::new(),
        )
        .unwrap();
        let server = std::thread::spawn(move || {
            let mut conn = listener.accept().expect("accept");
            let mut buf = Vec::new();
            let mut tmp = [0u8; 1024];
            while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                let n = conn.read(&mut tmp).expect("read");
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&tmp[..n]);
            }
            conn.write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n")
                .unwrap();
        });

        let target = ProxyTarget::http(InetAddr::Unix(path));
        let mut client = TestIo::new(b"");
        let request = ws_upgrade_request();
        let outcome = super::proxy_exchange(
            &mut client,
            &request,
            None,
            &target,
            &ProxyOptions::default(),
            "10.0.0.1",
            "http",
        )
        .unwrap();
        assert_eq!(outcome, ProxyOutcome::Complete);
        let response = String::from_utf8_lossy(&client.output);
        assert!(response.contains("403"), "must relay 403: {response}");
        server.join().unwrap();
    }
}
