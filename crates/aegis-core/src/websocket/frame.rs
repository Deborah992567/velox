//! RFC 6455 frame codec.
//!
//! The wire format of a WebSocket frame (§5.2): one header byte carrying the
//! `FIN` bit, the `RSV1..3` bits (kept `0` — no extensions are negotiated in
//! this phase), and the opcode; one payload-length byte with the mask bit and
//! a 7-bit length that expands to 16-bit or 64-bit extended forms; an optional
//! 4-byte masking key (client-to-server frames MUST be masked); then the
//! payload, XOR-unmasked with the key when present.
//!
//! This module owns the codec ([`FrameDecoder`] decodes incrementally with
//! [`FrameLimits`] caps, [`MessageDecoder`] additionally reassembles
//! fragmented text/binary messages while passing control frames through, and
//! [`Frame::encode`] serializes a frame). Validation follows §5.1–§5.5: RSV
//! bits must be clear, reserved opcodes are rejected, control frames must not
//! be fragmented and carry at most 125 bytes, a continuation requires an open
//! fragmented message, and a close frame's status code and reason are checked
//! (§7.4, §8.1).

use std::fmt;

/// A frame opcode (§5.2). Values 0x3–0x7 and 0xB–0xF are reserved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Opcode {
    /// A continuation frame of the fragmented message in progress.
    Continuation,
    /// A text data frame; the payload is UTF-8.
    Text,
    /// A binary data frame.
    Binary,
    /// A close control frame carrying an optional status code + reason.
    Close,
    /// A ping control frame; the peer must answer with a pong of the same
    /// payload.
    Ping,
    /// A pong control frame, usually echoing a ping.
    Pong,
    /// An opcode reserved for future definitions (§5.2).
    Reserved(u8),
}

impl Opcode {
    /// Classify a raw opcode nibble.
    pub const fn from_u8(value: u8) -> Self {
        match value {
            0x0 => Self::Continuation,
            0x1 => Self::Text,
            0x2 => Self::Binary,
            0x8 => Self::Close,
            0x9 => Self::Ping,
            0xA => Self::Pong,
            other => Self::Reserved(other),
        }
    }

    /// The raw opcode nibble.
    pub const fn to_u8(self) -> u8 {
        match self {
            Self::Continuation => 0x0,
            Self::Text => 0x1,
            Self::Binary => 0x2,
            Self::Close => 0x8,
            Self::Ping => 0x9,
            Self::Pong => 0xA,
            Self::Reserved(value) => value,
        }
    }

    /// Whether this is a control opcode (0x8–0xF). Control frames may be
    /// interleaved with fragments but must not themselves be fragmented.
    pub const fn is_control(self) -> bool {
        matches!(
            self,
            Self::Close | Self::Ping | Self::Pong | Self::Reserved(0xB..)
        )
    }

    /// Whether this opcode was defined by RFC 6455 (as opposed to reserved).
    pub const fn is_defined(self) -> bool {
        matches!(
            self,
            Self::Continuation | Self::Text | Self::Binary | Self::Close | Self::Ping | Self::Pong
        )
    }
}

/// Why a frame or message was rejected. Per §7.1 a protocol violation ends the
/// WebSocket connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameError {
    /// An `RSV1..3` bit was set; no extension has been negotiated.
    UnexpectedRsv(u8),
    /// A reserved opcode (0x3–0x7, 0xB–0xF) appeared on the wire.
    ReservedOpcode(u8),
    /// A control frame (close/ping/pong) had `FIN` clear.
    FragmentControlFrame,
    /// A control frame payload exceeded the mandatory 125-byte cap (§5.5).
    ControlPayloadTooLarge,
    /// A continuation frame arrived with no fragmented message in progress.
    OrphanContinuation,
    /// A new text/binary frame started while a fragmented message was open.
    MessageFragmented,
    /// A frame payload exceeded [`FrameLimits::max_frame_payload`].
    FrameTooLarge,
    /// The reassembled message exceeded [`FrameLimits::max_message_size`].
    MessageTooLarge,
    /// A close frame carried a partial status code (a 1-byte payload).
    PartialCloseCode,
    /// A close frame carried a status code that must not be sent (§7.4).
    InvalidCloseCode(u16),
    /// A close frame's reason was not valid UTF-8 (§8.1).
    InvalidCloseReason,
    /// A text message (whole or reassembled) was not valid UTF-8 (§8.1).
    InvalidText,
}

impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnexpectedRsv(rsv) => write!(f, "rsv bits set without an extension ({rsv:#06b})"),
            Self::ReservedOpcode(opcode) => write!(f, "reserved opcode 0x{opcode:x}"),
            Self::FragmentControlFrame => write!(f, "fragmented control frame"),
            Self::ControlPayloadTooLarge => write!(f, "control frame payload over 125 bytes"),
            Self::OrphanContinuation => write!(f, "continuation without an open message"),
            Self::MessageFragmented => write!(f, "data frame while a message is fragmented"),
            Self::FrameTooLarge => write!(f, "frame payload exceeds the configured limit"),
            Self::MessageTooLarge => write!(f, "message exceeds the configured limit"),
            Self::PartialCloseCode => write!(f, "close frame with a partial status code"),
            Self::InvalidCloseCode(code) => write!(f, "close code {code} is not sendable"),
            Self::InvalidCloseReason => write!(f, "close reason is not valid UTF-8"),
            Self::InvalidText => write!(f, "text message is not valid UTF-8"),
        }
    }
}

impl std::error::Error for FrameError {}

/// Caps applied while decoding frames and reassembling messages.
#[derive(Debug, Clone, Copy)]
pub struct FrameLimits {
    /// The largest single frame payload that will be buffered.
    pub max_frame_payload: usize,
    /// The largest reassembled (multi-fragment) message that will be kept.
    pub max_message_size: usize,
}

impl Default for FrameLimits {
    fn default() -> Self {
        Self {
            max_frame_payload: 1024 * 1024,
            max_message_size: 16 * 1024 * 1024,
        }
    }
}

/// Apply a 4-byte masking key to `data` (RFC 6455 §5.3), cycling the key
/// bytes.
pub fn apply_mask(data: &mut [u8], key: &[u8; 4]) {
    for (index, byte) in data.iter_mut().enumerate() {
        *byte ^= key[index % 4];
    }
}

/// Whether a status code may appear in a close frame on the wire (§7.4.1,
/// §7.4.2). Codes 1005 and 1006 are reserved for the stack (never sent), 1004
/// is unused, and 1016–2999 are unassigned.
pub const fn is_sendable_close_code(code: u16) -> bool {
    matches!(code, 1000..=1003 | 1007..=1011 | 3000..=4999)
}

/// One decoded — or ready-to-encode — WebSocket frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    /// The `FIN` bit: this is the last frame of the message.
    pub fin: bool,
    /// The `RSV1..3` bits packed as the low three bits (0–7). Must be `0`
    /// unless an extension is negotiated.
    pub rsv: u8,
    /// The frame opcode.
    pub opcode: Opcode,
    /// The masking key, present exactly for frames that were (or must be)
    /// masked. Client-to-server frames are always masked; server-to-client
    /// frames never are.
    pub mask: Option<[u8; 4]>,
    /// The payload, unmasked in memory.
    pub payload: Vec<u8>,
}

impl Frame {
    /// A single unmasked text frame.
    pub fn text(payload: impl Into<Vec<u8>>) -> Self {
        Self::message(Opcode::Text, true, payload)
    }

    /// A single unmasked binary frame.
    pub fn binary(payload: impl Into<Vec<u8>>) -> Self {
        Self::message(Opcode::Binary, true, payload)
    }

    /// A data or control frame with the given opcode and `FIN` bit.
    pub fn message(opcode: Opcode, fin: bool, payload: impl Into<Vec<u8>>) -> Self {
        Self {
            fin,
            rsv: 0,
            opcode,
            mask: None,
            payload: payload.into(),
        }
    }

    /// An unmasked ping control frame.
    pub fn ping(payload: impl Into<Vec<u8>>) -> Self {
        Self::message(Opcode::Ping, true, payload)
    }

    /// An unmasked pong control frame.
    pub fn pong(payload: impl Into<Vec<u8>>) -> Self {
        Self::message(Opcode::Pong, true, payload)
    }

    /// An unmasked close frame with an optional status code and reason.
    pub fn close(code: Option<u16>, reason: impl Into<Vec<u8>>) -> Self {
        let mut payload = Vec::new();
        if let Some(code) = code {
            payload.extend_from_slice(&code.to_be_bytes());
        }
        payload.extend_from_slice(reason.into().as_slice());
        Self::message(Opcode::Close, true, payload)
    }

    /// Apply a masking key so the encoded frame is client-to-server.
    #[must_use]
    pub const fn with_mask(mut self, key: [u8; 4]) -> Self {
        self.mask = Some(key);
        self
    }

    /// Serialize the frame to the wire, applying the mask if present.
    pub fn encode(&self, out: &mut Vec<u8>) {
        let len = self.payload.len();
        let first = u8::from(self.fin) << 7 | (self.rsv & 0x07) << 4 | self.opcode.to_u8();
        out.push(first);
        if let Some(key) = self.mask {
            out.push(0x80 | length_code(len));
            push_extended_length(out, len);
            out.extend_from_slice(&key);
            let start = out.len();
            out.extend_from_slice(&self.payload);
            apply_mask(&mut out[start..], &key);
        } else {
            out.push(length_code(len));
            push_extended_length(out, len);
            out.extend_from_slice(&self.payload);
        }
    }

    /// The decoded payload.
    pub fn payload(&self) -> &[u8] {
        &self.payload
    }

    /// Whether this is the final frame of its message.
    pub const fn is_final(&self) -> bool {
        self.fin
    }

    /// The frame's opcode.
    pub const fn opcode(&self) -> Opcode {
        self.opcode
    }
}

/// The 7-bit length code: the length itself for ≤125, else the 126/127
/// escape markers that select the extended forms.
#[allow(clippy::cast_possible_truncation)]
const fn length_code(len: usize) -> u8 {
    match len {
        0..=125 => len as u8,
        126..=0xFFFF => 126,
        _ => 127,
    }
}

/// Append the extended length bytes selected by [`length_code`].
#[allow(clippy::cast_possible_truncation)]
fn push_extended_length(out: &mut Vec<u8>, len: usize) {
    match len {
        0..=125 => {}
        126..=0xFFFF => out.extend_from_slice(&(len as u16).to_be_bytes()),
        _ => out.extend_from_slice(&(len as u64).to_be_bytes()),
    }
}

/// An incremental RFC 6455 frame decoder.
///
/// Feed bytes with [`FrameDecoder::push`]; each call returns the next complete
/// frame when one has arrived, or `Ok(None)` while more bytes are needed.
/// Fragmentation rules are enforced across calls, so a single decoder must
/// own one WebSocket connection's inbound stream.
#[derive(Debug)]
pub struct FrameDecoder {
    limits: FrameLimits,
    buf: Vec<u8>,
    message_open: bool,
}

impl Default for FrameDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameDecoder {
    /// A decoder with the default [`FrameLimits`].
    pub fn new() -> Self {
        Self::new_limited(FrameLimits::default())
    }

    /// A decoder with explicit caps.
    pub const fn new_limited(limits: FrameLimits) -> Self {
        Self {
            limits,
            buf: Vec::new(),
            message_open: false,
        }
    }

    /// Whether a fragmented message is currently open (a continuation frame
    /// is expected next).
    pub const fn message_open(&self) -> bool {
        self.message_open
    }

    /// Feed bytes and decode the next frame, if complete.
    pub fn push(&mut self, bytes: &[u8]) -> Result<Option<Frame>, FrameError> {
        self.buf.extend_from_slice(bytes);
        self.try_parse()
    }

    /// Decode the next complete frame already buffered, without adding bytes.
    /// Returns `None` while more bytes are needed.
    pub fn next_frame(&mut self) -> Result<Option<Frame>, FrameError> {
        self.try_parse()
    }

    fn try_parse(&mut self) -> Result<Option<Frame>, FrameError> {
        let Some(header) = self.parse_header()? else {
            return Ok(None);
        };
        let FrameHeader {
            fin,
            rsv,
            opcode,
            mask_key,
            header_len,
            payload_len,
        } = header;
        if opcode.is_control() && !fin {
            return Err(FrameError::FragmentControlFrame);
        }
        if opcode.is_control() && payload_len > 125 {
            return Err(FrameError::ControlPayloadTooLarge);
        }
        if payload_len > u64::try_from(self.limits.max_frame_payload).unwrap_or(u64::MAX) {
            return Err(FrameError::FrameTooLarge);
        }
        let payload_len = usize::try_from(payload_len).expect("fits usize after the cap check");
        if self.buf.len() < header_len + payload_len {
            return Ok(None);
        }
        match opcode {
            Opcode::Continuation => {
                if !self.message_open {
                    return Err(FrameError::OrphanContinuation);
                }
                if fin {
                    self.message_open = false;
                }
            }
            Opcode::Text | Opcode::Binary => {
                if self.message_open {
                    return Err(FrameError::MessageFragmented);
                }
                self.message_open = !fin;
            }
            _ => {}
        }
        let mut payload = self.buf[header_len..header_len + payload_len].to_vec();
        if let Some(key) = mask_key {
            apply_mask(&mut payload, &key);
        }
        if opcode == Opcode::Close {
            validate_close_payload(&payload)?;
        }
        if opcode == Opcode::Text && fin && std::str::from_utf8(&payload).is_err() {
            return Err(FrameError::InvalidText);
        }
        self.buf.drain(..header_len + payload_len);
        Ok(Some(Frame {
            fin,
            rsv,
            opcode,
            mask: mask_key,
            payload,
        }))
    }

    /// Parse the frame header, returning `None` until the header bytes are
    /// available. The total header length includes the mask key.
    fn parse_header(&self) -> Result<Option<FrameHeader>, FrameError> {
        if self.buf.len() < 2 {
            return Ok(None);
        }
        let first = self.buf[0];
        let fin = first & 0x80 != 0;
        let rsv = (first >> 4) & 0x07;
        if rsv != 0 {
            return Err(FrameError::UnexpectedRsv(rsv));
        }
        let opcode = Opcode::from_u8(first & 0x0F);
        if !opcode.is_defined() {
            return Err(FrameError::ReservedOpcode(opcode.to_u8()));
        }
        let masked = self.buf[1] & 0x80 != 0;
        let code = self.buf[1] & 0x7F;
        let mut header_len = 2usize;
        let payload_len = match code {
            126 => {
                if self.buf.len() < 4 {
                    return Ok(None);
                }
                header_len = 4;
                u64::from(u16::from_be_bytes([self.buf[2], self.buf[3]]))
            }
            127 => {
                if self.buf.len() < 10 {
                    return Ok(None);
                }
                header_len = 10;
                let mut bytes = [0u8; 8];
                bytes.copy_from_slice(&self.buf[2..10]);
                u64::from_be_bytes(bytes)
            }
            other => u64::from(other),
        };
        let mask_key = if masked {
            if self.buf.len() < header_len + 4 {
                return Ok(None);
            }
            let key = [
                self.buf[header_len],
                self.buf[header_len + 1],
                self.buf[header_len + 2],
                self.buf[header_len + 3],
            ];
            header_len += 4;
            Some(key)
        } else {
            None
        };
        Ok(Some(FrameHeader {
            fin,
            rsv,
            opcode,
            mask_key,
            header_len,
            payload_len,
        }))
    }
}

/// The decoded fields of a frame header (§5.2), including the running header
/// length so the caller can locate the payload.
struct FrameHeader {
    fin: bool,
    rsv: u8,
    opcode: Opcode,
    mask_key: Option<[u8; 4]>,
    header_len: usize,
    payload_len: u64,
}

/// Validate a close frame's payload: either empty, or a 2-byte sendable
/// status code plus a UTF-8 reason (§5.5.1, §7.4).
fn validate_close_payload(payload: &[u8]) -> Result<(), FrameError> {
    match payload.len() {
        0 => Ok(()),
        1 => Err(FrameError::PartialCloseCode),
        _ => {
            let code = u16::from_be_bytes([payload[0], payload[1]]);
            if !is_sendable_close_code(code) {
                return Err(FrameError::InvalidCloseCode(code));
            }
            if std::str::from_utf8(&payload[2..]).is_err() {
                return Err(FrameError::InvalidCloseReason);
            }
            Ok(())
        }
    }
}

/// A fully decoded WebSocket message (RFC 6455 §5.4): a text/binary data
/// message with all its fragments concatenated, or a single control frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    /// A complete text message; guaranteed valid UTF-8.
    Text(Vec<u8>),
    /// A complete binary message.
    Binary(Vec<u8>),
    /// A close control frame: optional status code and reason.
    Close { code: Option<u16>, reason: Vec<u8> },
    /// A ping control frame.
    Ping(Vec<u8>),
    /// A pong control frame.
    Pong(Vec<u8>),
}

impl Message {
    /// The opcode the message was sent under.
    pub const fn opcode(&self) -> Opcode {
        match self {
            Self::Text(_) => Opcode::Text,
            Self::Binary(_) => Opcode::Binary,
            Self::Close { .. } => Opcode::Close,
            Self::Ping(_) => Opcode::Ping,
            Self::Pong(_) => Opcode::Pong,
        }
    }
}

/// A decoder that yields whole [`Message`]s: fragments are accumulated and
/// validated (size cap, UTF-8), while ping/pong/close control frames are
/// surfaced as they arrive, interleaved between fragments.
#[derive(Debug)]
pub struct MessageDecoder {
    frames: FrameDecoder,
    open: Option<OpenMessage>,
}

#[derive(Debug)]
struct OpenMessage {
    opcode: Opcode,
    buffer: Vec<u8>,
}

impl Default for MessageDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl MessageDecoder {
    /// A message decoder with the default [`FrameLimits`].
    pub fn new() -> Self {
        Self::new_limited(FrameLimits::default())
    }

    /// A message decoder with explicit caps.
    pub const fn new_limited(limits: FrameLimits) -> Self {
        Self {
            frames: FrameDecoder::new_limited(limits),
            open: None,
        }
    }

    /// Feed bytes and decode the next complete message, if one has arrived.
    pub fn push(&mut self, bytes: &[u8]) -> Result<Option<Message>, FrameError> {
        if let Some(frame) = self.frames.push(bytes)?
            && let Some(message) = self.consume(frame)?
        {
            return Ok(Some(message));
        }
        loop {
            let Some(frame) = self.frames.next_frame()? else {
                return Ok(None);
            };
            if let Some(message) = self.consume(frame)? {
                return Ok(Some(message));
            }
        }
    }

    fn consume(&mut self, frame: Frame) -> Result<Option<Message>, FrameError> {
        match frame.opcode {
            Opcode::Ping => Ok(Some(Message::Ping(frame.payload))),
            Opcode::Pong => Ok(Some(Message::Pong(frame.payload))),
            Opcode::Close => {
                let (code, reason) = split_close_payload(&frame.payload);
                Ok(Some(Message::Close {
                    code,
                    reason: reason.to_vec(),
                }))
            }
            Opcode::Text | Opcode::Binary | Opcode::Continuation => {
                if matches!(frame.opcode, Opcode::Text | Opcode::Binary) && self.open.is_some() {
                    return Err(FrameError::MessageFragmented);
                }
                if matches!(frame.opcode, Opcode::Continuation) && self.open.is_none() {
                    return Err(FrameError::OrphanContinuation);
                }
                let capacity = self.frames.limits.max_message_size;
                let open = self.open.get_or_insert(OpenMessage {
                    opcode: frame.opcode,
                    buffer: Vec::new(),
                });
                if open.buffer.len() + frame.payload.len() > capacity {
                    return Err(FrameError::MessageTooLarge);
                }
                open.buffer.extend_from_slice(&frame.payload);
                if frame.fin {
                    let open = self.open.take().expect("message was open");
                    let payload = open.buffer;
                    match open.opcode {
                        Opcode::Text => {
                            if std::str::from_utf8(&payload).is_err() {
                                return Err(FrameError::InvalidText);
                            }
                            Ok(Some(Message::Text(payload)))
                        }
                        Opcode::Binary => Ok(Some(Message::Binary(payload))),
                        _ => unreachable!("continuation opcodes never open a message"),
                    }
                } else {
                    Ok(None)
                }
            }
            Opcode::Reserved(_) => unreachable!("the frame decoder rejects reserved opcodes"),
        }
    }
}

/// Split a close payload into `(status code, reason)`. The status code is
/// present only when the payload is at least two bytes; it was already
/// validated by the frame decoder.
fn split_close_payload(payload: &[u8]) -> (Option<u16>, &[u8]) {
    if payload.len() < 2 {
        (None, payload)
    } else {
        (
            Some(u16::from_be_bytes([payload[0], payload[1]])),
            &payload[2..],
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Frame, FrameDecoder, FrameError, Message, MessageDecoder, Opcode, apply_mask,
        is_sendable_close_code,
    };

    #[test]
    fn decodes_the_rfc_masked_hello_vector() {
        // RFC 6455 §5.7: single-frame masked text "Hello".
        let wire = [
            0x81, 0x85, 0x37, 0xfa, 0x21, 0x3d, 0x7f, 0x9f, 0x4d, 0x51, 0x58,
        ];
        let mut decoder = FrameDecoder::new();
        let frame = decoder.push(&wire).unwrap().unwrap();
        assert_eq!(frame.opcode, Opcode::Text);
        assert!(frame.fin);
        assert_eq!(frame.payload, b"Hello");
        assert_eq!(frame.mask, Some([0x37, 0xfa, 0x21, 0x3d]));
    }

    #[test]
    fn encodes_the_rfc_unmasked_hello_vector() {
        let frame = Frame::text("Hello");
        let mut out = Vec::new();
        frame.encode(&mut out);
        assert_eq!(out, b"\x81\x05Hello");
    }

    #[test]
    fn masked_round_trip() {
        let wire = Frame::text("Hello").with_mask([0x37, 0xfa, 0x21, 0x3d]);
        let mut encoded = Vec::new();
        wire.encode(&mut encoded);
        let mut decoder = FrameDecoder::new();
        let frame = decoder.push(&encoded).unwrap().unwrap();
        assert_eq!(frame.payload, b"Hello");
        assert_eq!(frame.mask, Some([0x37, 0xfa, 0x21, 0x3d]));
    }

    #[test]
    fn apply_mask_cycles_the_key() {
        let mut data = b"Hello".to_vec();
        apply_mask(&mut data, &[0x37, 0xfa, 0x21, 0x3d]);
        assert_eq!(data, [0x7f, 0x9f, 0x4d, 0x51, 0x58]);
    }

    #[test]
    fn reassembles_the_rfc_fragmented_hello() {
        // RFC 6455 §5.7: "Hello" as three fragments.
        let first = [0x01, 0x03, b'H', b'e', b'l'];
        let second = [0x80, 0x02, b'l', b'o'];
        let mut decoder = MessageDecoder::new();
        assert_eq!(decoder.push(&first).unwrap(), None);
        let message = decoder.push(&second).unwrap().unwrap();
        assert_eq!(message, Message::Text(b"Hello".to_vec()));
    }

    #[test]
    fn control_frame_interleaves_with_fragments() {
        let mut decoder = MessageDecoder::new();
        assert_eq!(decoder.push(&[0x01, 0x01, b'a']).unwrap(), None);
        let ping = decoder.push(&[0x89, 0x02, 0x03, 0x04]).unwrap().unwrap();
        assert_eq!(ping, Message::Ping(vec![0x03, 0x04]));
        let done = decoder.push(&[0x80, 0x01, b'b']).unwrap().unwrap();
        assert_eq!(done, Message::Text(b"ab".to_vec()));
    }

    #[test]
    fn rejects_orphan_continuation() {
        let mut decoder = FrameDecoder::new();
        assert_eq!(
            decoder.push(&[0x80, 0x00]).unwrap_err(),
            FrameError::OrphanContinuation
        );
    }

    #[test]
    fn rejects_data_frame_inside_fragment() {
        let mut decoder = FrameDecoder::new();
        decoder.push(&[0x01, 0x01, b'a']).unwrap();
        assert_eq!(
            decoder.push(&[0x81, 0x01, b'b']).unwrap_err(),
            FrameError::MessageFragmented
        );
    }

    #[test]
    fn rejects_fragmented_control_frame() {
        let mut decoder = FrameDecoder::new();
        assert_eq!(
            decoder.push(&[0x09, 0x00]).unwrap_err(),
            FrameError::FragmentControlFrame
        );
    }

    #[test]
    fn rejects_oversized_control_payload() {
        let mut decoder = FrameDecoder::new();
        // Ping with a 126-byte payload (length byte 0xFE, 126).
        let mut wire = vec![0x89, 126, 0x00, 126];
        wire.resize(2 + 126, 0);
        assert_eq!(
            decoder.push(&wire).unwrap_err(),
            FrameError::ControlPayloadTooLarge
        );
    }

    #[test]
    fn rejects_reserved_opcodes_and_rsv_bits() {
        let mut decoder = FrameDecoder::new();
        assert_eq!(
            decoder.push(&[0x83, 0x00]).unwrap_err(),
            FrameError::ReservedOpcode(0x3)
        );
        let mut decoder = FrameDecoder::new();
        assert_eq!(
            decoder.push(&[0xC1, 0x00]).unwrap_err(),
            FrameError::UnexpectedRsv(0x4)
        );
    }

    #[test]
    fn decodes_extended_length_forms() {
        // A 256-byte binary frame (§5.7 vector): length 0x0100 in 16-bit form.
        let mut wire = vec![0x82, 126, 0x01, 0x00];
        wire.resize(4 + 256, 0xAB);
        let mut decoder = FrameDecoder::new();
        let frame = decoder.push(&wire).unwrap().unwrap();
        assert_eq!(frame.opcode, Opcode::Binary);
        assert_eq!(frame.payload.len(), 256);
        assert!(frame.payload.iter().all(|&b| b == 0xAB));

        // A 64 KiB text frame in 64-bit form.
        let mut wire = vec![0x81, 127, 0, 0, 0, 0, 0, 1, 0, 0];
        wire.resize(10 + 65536, b'x');
        let mut decoder = FrameDecoder::new();
        let frame = decoder.push(&wire).unwrap().unwrap();
        assert_eq!(frame.payload.len(), 65536);
        assert_eq!(frame.payload[0], b'x');
    }

    #[test]
    fn feeds_incrementally() {
        let wire = [0x81, 0x05, b'H', b'e', b'l', b'l', b'o'];
        let mut decoder = FrameDecoder::new();
        for chunk in wire.chunks(2) {
            if let Some(frame) = decoder.push(chunk).unwrap() {
                assert_eq!(frame.payload, b"Hello");
                return;
            }
        }
        panic!("incremental feed never completed a frame");
    }

    #[test]
    fn enforces_frame_size_cap() {
        let mut decoder = FrameDecoder::new_limited(super::FrameLimits {
            max_frame_payload: 4,
            max_message_size: 1024,
        });
        assert_eq!(
            decoder.push(&[0x81, 0x05, 0, 0, 0, 0, 0]).unwrap_err(),
            FrameError::FrameTooLarge
        );
    }

    #[test]
    fn validates_close_payloads() {
        // Empty close is legal.
        let mut decoder = FrameDecoder::new();
        let close = decoder.push(&[0x88, 0x00]).unwrap().unwrap();
        assert_eq!(close.opcode, Opcode::Close);

        // A 1-byte payload is a partial status code.
        let mut decoder = FrameDecoder::new();
        assert_eq!(
            decoder.push(&[0x88, 0x01, 0x03]).unwrap_err(),
            FrameError::PartialCloseCode
        );

        // 1005 must never be sent.
        let mut decoder = FrameDecoder::new();
        assert_eq!(
            decoder.push(&[0x88, 0x02, 0x03, 0xED]).unwrap_err(),
            FrameError::InvalidCloseCode(1005)
        );

        // 1000 with a non-UTF-8 reason is rejected.
        let mut decoder = FrameDecoder::new();
        assert_eq!(
            decoder
                .push(&[0x88, 0x04, 0x03, 0xE8, 0xFF, 0xFE])
                .unwrap_err(),
            FrameError::InvalidCloseReason
        );
    }

    #[test]
    fn close_message_surfaces_code_and_reason() {
        let mut decoder = MessageDecoder::new();
        let message = decoder
            .push(&[0x88, 0x04, 0x03, 0xE8, b'o', b'k'])
            .unwrap()
            .unwrap();
        assert_eq!(
            message,
            Message::Close {
                code: Some(1000),
                reason: b"ok".to_vec(),
            }
        );
    }

    #[test]
    fn sendable_close_codes() {
        assert!(is_sendable_close_code(1000));
        assert!(is_sendable_close_code(1001));
        assert!(is_sendable_close_code(1011));
        assert!(is_sendable_close_code(3000));
        assert!(is_sendable_close_code(4999));
        assert!(!is_sendable_close_code(999));
        assert!(!is_sendable_close_code(1004));
        assert!(!is_sendable_close_code(1005));
        assert!(!is_sendable_close_code(1006));
        assert!(!is_sendable_close_code(1016));
        assert!(!is_sendable_close_code(2000));
        assert!(!is_sendable_close_code(5000));
    }

    #[test]
    fn rejects_non_utf8_text() {
        let mut decoder = FrameDecoder::new();
        assert_eq!(
            decoder.push(&[0x81, 0x02, 0xFF, 0xFE]).unwrap_err(),
            FrameError::InvalidText
        );
    }

    #[test]
    fn reassembled_text_must_be_utf8() {
        let mut decoder = MessageDecoder::new();
        decoder.push(&[0x01, 0x01, 0xFF]).unwrap();
        assert_eq!(
            decoder.push(&[0x80, 0x00]).unwrap_err(),
            FrameError::InvalidText
        );
    }

    #[test]
    fn single_frame_non_final_is_open_but_continues() {
        let mut decoder = FrameDecoder::new();
        let frame = decoder.push(&[0x01, 0x01, b'a']).unwrap().unwrap();
        assert!(!frame.fin);
        assert!(decoder.message_open());
    }
}
