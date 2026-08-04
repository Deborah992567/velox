//! Incremental chunked transfer decoder (RFC 9112 §7.1).
//!
//! ```text
//! chunked-body = *chunk last-chunk trailer-part CRLF
//! chunk        = chunk-size [ chunk-ext ] CRLF chunk-data CRLF
//! last-chunk   = 1*("0") [ chunk-ext ] CRLF
//! trailer-part = *( header-field CRLF )
//! ```
//!
//! The decoder consumes exactly the bytes it is given: it copies decoded body
//! bytes into a caller-provided output buffer (so a streaming connection never
//! has to buffer a whole body) and reports how many wire bytes it consumed and
//! how many output bytes it produced. Line framing — chunk-size and trailers —
//! is buffered internally and bounded, and every structural byte is validated:
//! a bare LF, a missing CRLF, a non-hex digit, or a malformed trailer rejects
//! the message. Trailer fields are collected with the same strict parser used
//! by the head parser.

use super::{hex_digit, parse_header_field};
use crate::http::{Headers, is_tchar};

/// Hard caps applied while decoding, mirroring the head-parse limits.
const CHUNK_SIZE_LINE_MAX: usize = 1024;
const TRAILER_LINE_MAX: usize = 8 * 1024;
const TRAILER_COUNT_MAX: usize = 100;
const TRAILER_BYTES_MAX: usize = 64 * 1024;

/// Progress of a [`ChunkedDecoder::feed`] call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeResult {
    /// The message is not complete. `consumed` wire bytes were used and
    /// `produced` decoded bytes were written to the output buffer; a caller
    /// that fed all available input and/or filled the output buffer should
    /// call again with more input and/or a fresh buffer.
    NeedMore { consumed: usize, produced: usize },
    /// The final chunk and any trailers were decoded; `consumed` and
    /// `produced` are as for [`DecodeResult::NeedMore`], and remaining input
    /// belongs to the next message.
    Done { consumed: usize, produced: usize },
    /// The chunked body is malformed and the connection must be rejected.
    Error(ChunkedError),
}

/// Why a chunked body was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChunkedError {
    /// A CRLF was expected but a bare LF or other byte was seen.
    MissingCrlf,
    /// The chunk-size part was empty or contained a non-hex byte.
    InvalidChunkSize,
    /// The chunk-size overflowed `u64`.
    ChunkSizeTooLarge,
    /// A chunk extension violated `1*( ";" chunk-ext-name [ "=" chunk-ext-val ] )`.
    InvalidChunkExtension,
    /// The chunk-size line exceeded the internal cap.
    ChunkSizeLineTooLong,
    /// A trailer line exceeded the internal cap.
    TrailerLineTooLong,
    /// More trailer fields than the internal cap.
    TooManyTrailers,
    /// The trailer block exceeded the internal cap.
    TrailersTooLarge,
    /// A trailer field was malformed.
    InvalidTrailer,
}

impl DecodeResult {
    /// The number of wire bytes consumed by the call.
    pub const fn consumed(self) -> usize {
        match self {
            Self::NeedMore { consumed, .. } | Self::Done { consumed, .. } => consumed,
            Self::Error(_) => 0,
        }
    }

    /// The number of decoded bytes written to the output buffer.
    pub const fn produced(self) -> usize {
        match self {
            Self::NeedMore { produced, .. } | Self::Done { produced, .. } => produced,
            Self::Error(_) => 0,
        }
    }

    /// Whether the message is complete (or rejected).
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Done { .. } | Self::Error(_))
    }
}

/// Which part of the chunked grammar the decoder is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// Reading a `chunk-size [ chunk-ext ] CRLF` line.
    ChunkSize,
    /// Emitting `chunk-data` bytes.
    ChunkData,
    /// Expecting the CRLF after a chunk-data block.
    ChunkDataCrlf,
    /// Reading `trailer-part`.
    Trailer,
}

/// An incremental chunked transfer decoder.
#[derive(Debug)]
pub struct ChunkedDecoder {
    carry: Vec<u8>,
    state: State,
    remaining: u64,
    crlf_expect: u8,
    trailers: Headers,
    trailer_bytes: usize,
    done: bool,
}

impl Default for ChunkedDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl ChunkedDecoder {
    /// A decoder ready for a fresh chunked body.
    pub const fn new() -> Self {
        Self {
            carry: Vec::new(),
            state: State::ChunkSize,
            remaining: 0,
            crlf_expect: 0,
            trailers: Headers::new(),
            trailer_bytes: 0,
            done: false,
        }
    }

    /// Feed wire bytes and decode as much as `out` can hold.
    pub fn feed(&mut self, input: &[u8], out: &mut [u8]) -> DecodeResult {
        if self.done {
            return DecodeResult::Done {
                consumed: 0,
                produced: 0,
            };
        }
        let mut consumed = 0;
        let mut produced = 0;
        loop {
            match self.state {
                State::ChunkSize => match self.scan_line(input, consumed, CHUNK_SIZE_LINE_MAX) {
                    Ok(Some(after_lf)) => {
                        consumed = after_lf;
                        let line = match self.take_line() {
                            Ok(line) => line,
                            Err(error) => return self.fail(error),
                        };
                        if let Err(error) = self.process_size_line(&line) {
                            return self.fail(error);
                        }
                    }
                    Ok(None) => return DecodeResult::NeedMore { consumed, produced },
                    Err(()) => return self.fail(ChunkedError::ChunkSizeLineTooLong),
                },
                State::ChunkData => {
                    if self.remaining == 0 {
                        self.state = State::ChunkDataCrlf;
                        continue;
                    }
                    if out.len() - produced == 0 {
                        return DecodeResult::NeedMore { consumed, produced };
                    }
                    let room = (out.len() - produced).min(input.len() - consumed);
                    let take = usize::try_from(self.remaining)
                        .unwrap_or(usize::MAX)
                        .min(room);
                    if take == 0 {
                        return DecodeResult::NeedMore { consumed, produced };
                    }
                    out[produced..produced + take]
                        .copy_from_slice(&input[consumed..consumed + take]);
                    self.remaining -= take as u64;
                    produced += take;
                    consumed += take;
                    if self.remaining == 0 {
                        self.state = State::ChunkDataCrlf;
                    } else {
                        return DecodeResult::NeedMore { consumed, produced };
                    }
                }
                State::ChunkDataCrlf => {
                    while self.crlf_expect < 2 && consumed < input.len() {
                        let expected = if self.crlf_expect == 0 { b'\r' } else { b'\n' };
                        if input[consumed] != expected {
                            return self.fail(ChunkedError::MissingCrlf);
                        }
                        consumed += 1;
                        self.crlf_expect += 1;
                    }
                    if self.crlf_expect == 2 {
                        self.crlf_expect = 0;
                        self.state = State::ChunkSize;
                    } else {
                        return DecodeResult::NeedMore { consumed, produced };
                    }
                }
                State::Trailer => match self.scan_line(input, consumed, TRAILER_LINE_MAX) {
                    Ok(Some(after_lf)) => {
                        consumed = after_lf;
                        let line = match self.take_line() {
                            Ok(line) => line,
                            Err(error) => return self.fail(error),
                        };
                        if line.is_empty() {
                            self.done = true;
                            return DecodeResult::Done { consumed, produced };
                        }
                        let Ok(header) = parse_header_field(&line) else {
                            return self.fail(ChunkedError::InvalidTrailer);
                        };
                        self.trailers.push(header);
                        if self.trailers.len() > TRAILER_COUNT_MAX {
                            return self.fail(ChunkedError::TooManyTrailers);
                        }
                        self.trailer_bytes += line.len();
                        if self.trailer_bytes > TRAILER_BYTES_MAX {
                            return self.fail(ChunkedError::TrailersTooLarge);
                        }
                    }
                    Ok(None) => return DecodeResult::NeedMore { consumed, produced },
                    Err(()) => return self.fail(ChunkedError::TrailerLineTooLong),
                },
            }
        }
    }

    /// The trailer fields collected after the final chunk.
    pub fn take_trailers(&mut self) -> Headers {
        std::mem::take(&mut self.trailers)
    }

    /// Whether the message has reached its final chunk and trailers.
    pub const fn is_done(&self) -> bool {
        self.done
    }

    /// Reset the decoder for a fresh chunked body.
    pub fn reset(&mut self) {
        self.carry.clear();
        self.state = State::ChunkSize;
        self.remaining = 0;
        self.crlf_expect = 0;
        self.trailers = Headers::new();
        self.trailer_bytes = 0;
        self.done = false;
    }

    /// Copy input bytes into the line buffer until a LF arrives or the line
    /// cap is hit. `Ok(Some(after_lf))` when a line terminated; `Ok(None)`
    /// when more input is needed; `Err(())` when the buffer filled first.
    fn scan_line(&mut self, input: &[u8], from: usize, cap: usize) -> Result<Option<usize>, ()> {
        let limit = cap + 2;
        let mut i = from;
        while i < input.len() && self.carry.len() < limit {
            let byte = input[i];
            self.carry.push(byte);
            i += 1;
            if byte == b'\n' {
                return Ok(Some(i));
            }
        }
        if self.carry.len() >= limit {
            self.carry.clear();
            return Err(());
        }
        Ok(None)
    }

    /// Extract the just-terminated line from the carry buffer, enforcing that
    /// it ended in CRLF.
    fn take_line(&mut self) -> Result<Vec<u8>, ChunkedError> {
        let n = self.carry.len();
        if n < 2 || self.carry[n - 2] != b'\r' {
            self.carry.clear();
            return Err(ChunkedError::MissingCrlf);
        }
        let line = self.carry[..n - 2].to_vec();
        self.carry.clear();
        Ok(line)
    }

    /// Parse `chunk-size [ chunk-ext ]` and move to the next state.
    fn process_size_line(&mut self, line: &[u8]) -> Result<(), ChunkedError> {
        let extension = line
            .iter()
            .position(|&b| b == b';')
            .map_or(&[][..], |i| &line[i..]);
        let size_part = line
            .iter()
            .position(|&b| b == b';')
            .map_or(line, |i| &line[..i]);
        if size_part.is_empty() {
            return Err(ChunkedError::InvalidChunkSize);
        }
        let mut size: u64 = 0;
        for &byte in size_part {
            let Some(digit) = hex_digit(byte) else {
                return Err(ChunkedError::InvalidChunkSize);
            };
            size = size
                .checked_mul(16)
                .and_then(|s| s.checked_add(u64::from(digit)))
                .ok_or(ChunkedError::ChunkSizeTooLarge)?;
        }
        validate_extensions(extension)?;
        if size == 0 {
            self.state = State::Trailer;
        } else {
            self.remaining = size;
            self.state = State::ChunkData;
        }
        Ok(())
    }

    const fn fail(&mut self, error: ChunkedError) -> DecodeResult {
        self.done = true;
        DecodeResult::Error(error)
    }
}

/// Validate the `chunk-ext` part (everything from the first `;` on): zero or
/// more `; name [ = value ]` segments whose names are tokens and whose values
/// are free of control bytes.
fn validate_extensions(extension: &[u8]) -> Result<(), ChunkedError> {
    if extension.is_empty() {
        return Ok(());
    }
    for segment in extension[1..].split(|&b| b == b';') {
        if segment.is_empty() {
            return Err(ChunkedError::InvalidChunkExtension);
        }
        let mut parts = segment.splitn(2, |&b| b == b'=');
        let name = parts.next().unwrap_or_default();
        let value = parts.next();
        if name.is_empty() || !name.iter().all(|&b| is_tchar(b)) {
            return Err(ChunkedError::InvalidChunkExtension);
        }
        if let Some(value) = value
            && (value.is_empty() || value.iter().any(|&b| (b < 0x20 && b != b'\t') || b == 0x7f))
        {
            return Err(ChunkedError::InvalidChunkExtension);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{ChunkedDecoder, ChunkedError, DecodeResult};
    use crate::http::HeaderName;

    fn decode_once(input: &[u8], out_len: usize) -> (DecodeResult, Vec<u8>) {
        let mut decoder = ChunkedDecoder::new();
        let mut out = vec![0u8; out_len];
        let result = decoder.feed(input, &mut out);
        out.truncate(result.produced());
        (result, out)
    }

    fn assert_decoded(input: &[u8], expected: &[u8]) {
        let (result, out) = decode_once(input, expected.len().max(1));
        assert_eq!(out, expected, "decoded bytes for {input:?}");
        assert_eq!(
            result,
            DecodeResult::Done {
                consumed: input.len(),
                produced: expected.len()
            }
        );
    }

    fn assert_error(input: &[u8], error: ChunkedError) {
        let mut decoder = ChunkedDecoder::new();
        let mut out = vec![0u8; 1024];
        let result = decoder.feed(input, &mut out);
        assert_eq!(result, DecodeResult::Error(error), "for {input:?}");
    }

    #[test]
    fn decodes_two_chunks() {
        assert_decoded(b"4\r\nWiki\r\n5\r\npedia\r\n0\r\n\r\n", b"Wikipedia");
    }

    #[test]
    fn decodes_one_chunk_exactly() {
        assert_decoded(b"5\r\nhello\r\n0\r\n\r\n", b"hello");
    }

    #[test]
    fn decodes_empty_body() {
        let (result, out) = decode_once(b"0\r\n\r\n", 4);
        assert_eq!(
            result,
            DecodeResult::Done {
                consumed: 5,
                produced: 0
            }
        );
        assert!(out.is_empty());
    }

    #[test]
    fn ignores_chunk_extensions() {
        assert_decoded(b"5;name=value\r\nhello\r\n0\r\n\r\n", b"hello");
    }

    #[test]
    fn collects_trailers() {
        let mut decoder = ChunkedDecoder::new();
        let mut out = vec![0u8; 64];
        let input = b"3\r\nfoo\r\n0\r\nX-Foo: bar\r\nX-Baz: qux\r\n\r\n";
        let result = decoder.feed(input, &mut out);
        assert_eq!(
            result,
            DecodeResult::Done {
                consumed: input.len(),
                produced: 3
            }
        );
        let trailers = decoder.take_trailers();
        assert_eq!(
            trailers.get(&HeaderName::Custom("x-foo".into())),
            Some(&b"bar"[..])
        );
        assert_eq!(
            trailers.get(&HeaderName::Custom("x-baz".into())),
            Some(&b"qux"[..])
        );
    }

    #[test]
    fn streams_through_a_small_output_buffer() {
        let mut decoder = ChunkedDecoder::new();
        let mut out = vec![0u8; 3];
        let input = b"5\r\nhello\r\n0\r\n\r\n";
        let result = decoder.feed(input, &mut out);
        assert_eq!(
            result,
            DecodeResult::NeedMore {
                consumed: 6,
                produced: 3
            }
        );
        assert_eq!(&out, b"hel");
        let mut out = vec![0u8; 3];
        let result = decoder.feed(&input[result.consumed()..], &mut out);
        assert_eq!(
            result,
            DecodeResult::Done {
                consumed: 9,
                produced: 2
            }
        );
        assert_eq!(&out[..2], b"lo");
    }

    #[test]
    fn feeds_byte_by_byte() {
        let input = b"4\r\nWiki\r\n0\r\n\r\n";
        let mut decoder = ChunkedDecoder::new();
        let mut out = Vec::new();
        let mut last = None;
        for &byte in input {
            let mut out_chunk = [0u8; 4];
            let result = decoder.feed(&[byte], &mut out_chunk);
            out.extend_from_slice(&out_chunk[..result.produced()]);
            if result.is_terminal() {
                last = Some(result);
            }
        }
        assert_eq!(
            last,
            Some(DecodeResult::Done {
                consumed: 1,
                produced: 0
            })
        );
        assert_eq!(out, b"Wiki");
    }

    #[test]
    fn rejects_bad_hex_digits() {
        assert_error(b"Z\r\nfoo\r\n0\r\n\r\n", ChunkedError::InvalidChunkSize);
        assert_error(b"\r\nfoo\r\n0\r\n\r\n", ChunkedError::InvalidChunkSize);
    }

    #[test]
    fn rejects_size_overflow() {
        assert_error(
            b"FFFFFFFFFFFFFFFFF\r\nfoo\r\n0\r\n\r\n",
            ChunkedError::ChunkSizeTooLarge,
        );
    }

    #[test]
    fn rejects_missing_or_bare_crlf() {
        assert_error(b"4\r\nWikiX\r\n0\r\n\r\n", ChunkedError::MissingCrlf);
        assert_error(b"4\nWiki\r\n0\r\n\r\n", ChunkedError::MissingCrlf);
        assert_error(b"4\rWiki\r\n0\r\n\r\n", ChunkedError::InvalidChunkSize);
    }

    #[test]
    fn rejects_malformed_extensions() {
        assert_error(
            b"5; bad\r\nhello\r\n0\r\n\r\n",
            ChunkedError::InvalidChunkExtension,
        );
        assert_error(
            b"5;=v\r\nhello\r\n0\r\n\r\n",
            ChunkedError::InvalidChunkExtension,
        );
        assert_error(
            b"5;\r\nhello\r\n0\r\n\r\n",
            ChunkedError::InvalidChunkExtension,
        );
        assert_error(
            b"5;name=\r\nhello\r\n0\r\n\r\n",
            ChunkedError::InvalidChunkExtension,
        );
        assert_error(
            b"5;na me=v\r\nhello\r\n0\r\n\r\n",
            ChunkedError::InvalidChunkExtension,
        );
    }

    #[test]
    fn rejects_malformed_trailers() {
        assert_error(b"0\r\nBad Header: x\r\n\r\n", ChunkedError::InvalidTrailer);
        assert_error(b"0\r\n: x\r\n\r\n", ChunkedError::InvalidTrailer);
    }

    #[test]
    fn rejects_too_many_trailers() {
        let mut decoder = ChunkedDecoder::new();
        let mut input = b"0\r\n".to_vec();
        for i in 0..101 {
            input.extend_from_slice(format!("X-H{i}: v\r\n").as_bytes());
        }
        input.extend_from_slice(b"\r\n");
        let mut out = vec![0u8; 8];
        assert_eq!(
            decoder.feed(&input, &mut out),
            DecodeResult::Error(ChunkedError::TooManyTrailers)
        );
    }

    #[test]
    fn reset_reuses_the_decoder() {
        let mut decoder = ChunkedDecoder::new();
        let mut out = vec![0u8; 16];
        assert_eq!(
            decoder.feed(b"3\r\nfoo\r\n0\r\n\r\n", &mut out),
            DecodeResult::Done {
                consumed: 13,
                produced: 3
            }
        );
        decoder.reset();
        assert!(!decoder.is_done());
        assert_eq!(
            decoder.feed(b"3\r\nbar\r\n0\r\n\r\n", &mut out),
            DecodeResult::Done {
                consumed: 13,
                produced: 3
            }
        );
        assert_eq!(&out[..3], b"bar");
    }
}
