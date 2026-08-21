//! HTTP/2 frame types and framing layer (RFC 9113 §4).
//!
//! Each HTTP/2 frame starts with a 9-byte header: 3 bytes length,
//! 1 byte type, 1 byte flags, and 4 bytes stream identifier.

/// Maximum frame payload size (default per spec: 16,384 bytes).
pub const DEFAULT_MAX_FRAME_SIZE: u32 = 16_384;

/// Minimum allowed max frame size.
pub const MIN_FRAME_SIZE: u32 = 16_384;

/// Maximum allowed max frame size (2^24 - 1).
pub const MAX_FRAME_SIZE: u32 = (1 << 24) - 1;

/// Frame types defined by HTTP/2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameType {
    Data,
    Headers,
    Priority,
    RstStream,
    Settings,
    PushPromise,
    Ping,
    GoAway,
    WindowUpdate,
    Continuation,
}

impl FrameType {
    pub const fn from_u8(v: u8) -> Option<Self> {
        match v {
            0x0 => Some(Self::Data),
            0x1 => Some(Self::Headers),
            0x2 => Some(Self::Priority),
            0x3 => Some(Self::RstStream),
            0x4 => Some(Self::Settings),
            0x5 => Some(Self::PushPromise),
            0x6 => Some(Self::Ping),
            0x7 => Some(Self::GoAway),
            0x8 => Some(Self::WindowUpdate),
            0x9 => Some(Self::Continuation),
            _ => None,
        }
    }

    pub const fn to_u8(self) -> u8 {
        match self {
            Self::Data => 0x0,
            Self::Headers => 0x1,
            Self::Priority => 0x2,
            Self::RstStream => 0x3,
            Self::Settings => 0x4,
            Self::PushPromise => 0x5,
            Self::Ping => 0x6,
            Self::GoAway => 0x7,
            Self::WindowUpdate => 0x8,
            Self::Continuation => 0x9,
        }
    }
}

/// Bitmask flags for DATA frames.
pub mod data_flags {
    pub const END_STREAM: u8 = 0x01;
    pub const PADDED: u8 = 0x08;
}

/// Bitmask flags for HEADERS frames.
pub mod headers_flags {
    pub const END_STREAM: u8 = 0x01;
    pub const END_HEADERS: u8 = 0x04;
    pub const PADDED: u8 = 0x08;
    pub const PRIORITY: u8 = 0x20;
}

/// Bitmask flags for SETTINGS frames.
pub mod settings_flags {
    pub const ACK: u8 = 0x01;
}

/// Bitmask flags for PING frames.
pub mod ping_flags {
    pub const ACK: u8 = 0x01;
}

/// Settings identifiers (RFC 9113 §6.5.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingId {
    HeaderTableSize,
    EnablePush,
    MaxConcurrentStreams,
    InitialWindowSize,
    MaxFrameSize,
    MaxHeaderListSize,
}

impl SettingId {
    pub const fn from_u16(v: u16) -> Option<Self> {
        match v {
            0x1 => Some(Self::HeaderTableSize),
            0x2 => Some(Self::EnablePush),
            0x3 => Some(Self::MaxConcurrentStreams),
            0x4 => Some(Self::InitialWindowSize),
            0x5 => Some(Self::MaxFrameSize),
            0x6 => Some(Self::MaxHeaderListSize),
            _ => None,
        }
    }

    pub const fn to_u16(self) -> u16 {
        match self {
            Self::HeaderTableSize => 0x1,
            Self::EnablePush => 0x2,
            Self::MaxConcurrentStreams => 0x3,
            Self::InitialWindowSize => 0x4,
            Self::MaxFrameSize => 0x5,
            Self::MaxHeaderListSize => 0x6,
        }
    }
}

/// A parsed HTTP/2 frame header (9 bytes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameHeader {
    pub length: u32,
    pub frame_type: FrameType,
    pub flags: u8,
    pub stream_id: u32,
}

impl FrameHeader {
    /// Parse a 9-byte frame header.
    pub fn parse(buf: &[u8; 9]) -> Self {
        let length = (u32::from(buf[0]) << 16) | (u32::from(buf[1]) << 8) | u32::from(buf[2]);
        let frame_type = FrameType::from_u8(buf[3]).unwrap_or(FrameType::Data);
        let flags = buf[4];
        let stream_id = u32::from_be_bytes([buf[5], buf[6], buf[7], buf[8]]) & 0x7FFF_FFFF;
        Self {
            length,
            frame_type,
            flags,
            stream_id,
        }
    }

    /// Serialize to 9 bytes.
    pub const fn serialize(&self) -> [u8; 9] {
        let len_bytes = self.length.to_be_bytes();
        let stream_bytes = self.stream_id.to_be_bytes();
        [
            len_bytes[1],
            len_bytes[2],
            len_bytes[3],
            self.frame_type.to_u8(),
            self.flags,
            stream_bytes[0],
            stream_bytes[1],
            stream_bytes[2],
            stream_bytes[3],
        ]
    }
}

/// A parsed SETTINGS parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Setting {
    pub id: SettingId,
    pub value: u32,
}

impl Setting {
    /// Parse from a 6-byte buffer.
    pub fn parse(buf: &[u8; 6]) -> Option<Self> {
        let id_val = u16::from_be_bytes([buf[0], buf[1]]);
        let value = u32::from_be_bytes([buf[2], buf[3], buf[4], buf[5]]);
        Some(Self {
            id: SettingId::from_u16(id_val)?,
            value,
        })
    }

    /// Serialize to 6 bytes.
    pub const fn serialize(&self) -> [u8; 6] {
        let id = self.id.to_u16().to_be_bytes();
        let val = self.value.to_be_bytes();
        [id[0], id[1], val[0], val[1], val[2], val[3]]
    }
}

/// Parse a SETTINGS frame payload into individual settings.
pub fn parse_settings(payload: &[u8]) -> Vec<Setting> {
    payload
        .chunks_exact(6)
        .filter_map(|chunk| {
            let mut buf = [0u8; 6];
            buf.copy_from_slice(chunk);
            Setting::parse(&buf)
        })
        .collect()
}

/// Default connection-level settings.
pub const DEFAULT_SETTINGS: &[Setting] = &[
    Setting {
        id: SettingId::HeaderTableSize,
        value: 4096,
    },
    Setting {
        id: SettingId::EnablePush,
        value: 1,
    },
    Setting {
        id: SettingId::InitialWindowSize,
        value: 65_535,
    },
    Setting {
        id: SettingId::MaxFrameSize,
        value: 16_384,
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_type_roundtrip() {
        for i in 0u8..=9 {
            let ft = FrameType::from_u8(i).unwrap();
            assert_eq!(ft.to_u8(), i);
        }
    }

    #[test]
    fn setting_id_roundtrip() {
        for i in 1u16..=6 {
            let sid = SettingId::from_u16(i).unwrap();
            assert_eq!(sid.to_u16(), i);
        }
    }

    #[test]
    fn setting_id_unknown() {
        assert!(SettingId::from_u16(99).is_none());
    }

    #[test]
    fn frame_header_roundtrip() {
        let header = FrameHeader {
            length: 1024,
            frame_type: FrameType::Data,
            flags: data_flags::END_STREAM,
            stream_id: 1,
        };
        let buf = header.serialize();
        let parsed = FrameHeader::parse(&buf);
        assert_eq!(parsed, header);
    }

    #[test]
    fn frame_header_stream_id_masks_reserved_bit() {
        let mut buf = [0u8; 9];
        buf[5] = 0x80;
        buf[8] = 0x01;
        let parsed = FrameHeader::parse(&buf);
        assert_eq!(parsed.stream_id, 1);
    }

    #[test]
    fn setting_roundtrip() {
        let setting = Setting {
            id: SettingId::MaxFrameSize,
            value: 16_384,
        };
        let buf = setting.serialize();
        let parsed = Setting::parse(&buf).unwrap();
        assert_eq!(parsed, setting);
    }

    #[test]
    fn parse_settings_payload() {
        let mut payload = Vec::new();
        for s in DEFAULT_SETTINGS {
            payload.extend_from_slice(&s.serialize());
        }
        let settings = parse_settings(&payload);
        assert_eq!(settings.len(), DEFAULT_SETTINGS.len());
        assert_eq!(settings[0].id, SettingId::HeaderTableSize);
        assert_eq!(settings[0].value, 4096);
    }

    #[test]
    fn frame_header_zero_length() {
        let header = FrameHeader {
            length: 0,
            frame_type: FrameType::Ping,
            flags: 0,
            stream_id: 0,
        };
        let buf = header.serialize();
        let parsed = FrameHeader::parse(&buf);
        assert_eq!(parsed, header);
    }

    #[test]
    fn setting_parse_skips_partial_chunks() {
        let payload = [0x00, 0x01, 0x00, 0x00, 0x10, 0x00, 0xFF];
        let settings = parse_settings(&payload);
        assert_eq!(settings.len(), 1);
    }

    #[test]
    fn frame_header_max_stream_id() {
        let header = FrameHeader {
            length: 0,
            frame_type: FrameType::WindowUpdate,
            flags: 0,
            stream_id: u32::MAX >> 1,
        };
        let buf = header.serialize();
        let parsed = FrameHeader::parse(&buf);
        assert_eq!(parsed.stream_id, header.stream_id);
    }
}
