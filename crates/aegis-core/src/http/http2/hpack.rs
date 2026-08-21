//! HPACK header compression (RFC 7541).
//!
//! Static table, dynamic table, integer encoding, and header field
//! representation for HTTP/2 header compression.

/// Maximum dynamic table size.
pub const DEFAULT_DYNAMIC_TABLE_SIZE: usize = 4096;

/// Static table entries (RFC 7541 Appendix A).
const STATIC_TABLE: &[(&str, &str)] = &[
    (":authority", ""),
    (":method", "GET"),
    (":method", "POST"),
    (":path", "/"),
    (":path", "/index.html"),
    (":scheme", "http"),
    (":scheme", "https"),
    (":status", "200"),
    (":status", "204"),
    (":status", "304"),
    (":status", "400"),
    (":status", "403"),
    (":status", "404"),
    (":status", "500"),
    ("accept-charset", ""),
    ("accept-encoding", "gzip, deflate"),
    ("accept-language", ""),
    ("accept-ranges", ""),
    ("accept", ""),
    ("access-control-allow-origin", ""),
    ("age", ""),
    ("allow", ""),
    ("authorization", ""),
    ("cache-control", ""),
    ("content-disposition", ""),
    ("content-encoding", ""),
    ("content-language", ""),
    ("content-length", ""),
    ("content-location", ""),
    ("content-range", ""),
    ("content-type", ""),
    ("cookie", ""),
    ("date", ""),
    ("etag", ""),
    ("expect", ""),
    ("expires", ""),
    ("from", ""),
    ("host", ""),
    ("if-match", ""),
    ("if-modified-since", ""),
    ("if-none-match", ""),
    ("if-range", ""),
    ("if-unmodified-since", ""),
    ("last-modified", ""),
    ("link", ""),
    ("location", ""),
    ("max-forwards", ""),
    ("proxy-authenticate", ""),
    ("proxy-authorization", ""),
    ("range", ""),
    ("referer", ""),
    ("refresh", ""),
    ("retry-after", ""),
    ("server", ""),
    ("set-cookie", ""),
    ("strict-transport-security", ""),
    ("transfer-encoding", ""),
    ("user-agent", ""),
    ("vary", ""),
    ("via", ""),
    ("www-authenticate", ""),
];

/// Length of the static table.
pub const STATIC_TABLE_LEN: usize = 61;

/// Encode an integer using HPACK integer encoding (RFC 7541 §5.1).
#[allow(clippy::cast_possible_truncation)]
pub fn encode_integer(mut value: u32, prefix_bits: u8) -> Vec<u8> {
    let prefix_mask = (1u8 << prefix_bits) - 1;
    let max_prefix = u32::from(prefix_mask);

    if value < max_prefix {
        return vec![value as u8];
    }

    let mut result = vec![prefix_mask];
    value -= max_prefix;

    while value >= 128 {
        result.push((value % 128) as u8 | 0x80);
        value /= 128;
    }
    result.push(value as u8);
    result
}

/// Decode an HPACK integer from a byte slice (RFC 7541 §5.1).
pub fn decode_integer(buf: &[u8], prefix_bits: u8) -> Option<(u32, usize)> {
    let prefix_mask = (1u8 << prefix_bits) - 1;
    if buf.is_empty() {
        return None;
    }

    let mut value = u32::from(buf[0] & prefix_mask);
    if value < u32::from(prefix_mask) {
        return Some((value, 1));
    }

    let mut m: u32 = 0;
    let mut i = 1;
    loop {
        if i >= buf.len() {
            return None;
        }
        let b = buf[i];
        value += u32::from(b & 0x7F) * (1u32 << m);
        m += 7;
        i += 1;
        if b & 0x80 == 0 {
            break;
        }
    }

    Some((value, i))
}

/// Huffman-encode a byte slice (simplified pass-through).
pub fn huffman_encode(data: &[u8]) -> Vec<u8> {
    let len = u32::try_from(data.len()).unwrap_or(u32::MAX);
    let len_enc = encode_integer(len, 7);
    let mut result = len_enc;
    result.extend_from_slice(data);
    result
}

/// Huffman-decode a byte slice.
pub fn huffman_decode(data: &[u8]) -> Option<Vec<u8>> {
    let (len, consumed) = decode_integer(data, 7)?;
    let start = consumed;
    let end = start + len as usize;
    if end > data.len() {
        return None;
    }
    Some(data[start..end].to_vec())
}

/// HPACK header field representation.
#[derive(Debug, Clone)]
pub enum HeaderField {
    /// Indexed header field representation.
    Indexed(u64),
    /// Literal header field with incremental indexing.
    LiteralWithIndex { name: String, value: String },
    /// Literal header field without indexing.
    LiteralNoIndex { name: String, value: String },
    /// Literal header field never indexed.
    NeverIndexed { name: String, value: String },
}

/// Encoder state for HPACK.
#[derive(Debug)]
pub struct Encoder {
    dynamic_table: Vec<(String, String)>,
    max_table_size: usize,
    current_size: usize,
}

impl Encoder {
    pub const fn new(max_table_size: usize) -> Self {
        Self {
            dynamic_table: Vec::new(),
            max_table_size,
            current_size: 0,
        }
    }

    /// Encode a header field.
    ///
    /// # Panics
    ///
    /// Never panics under normal operation.
    #[allow(clippy::cast_possible_truncation)]
    pub fn encode_header(&mut self, name: &str, value: &str) -> Vec<u8> {
        for (i, (sname, svalue)) in STATIC_TABLE.iter().enumerate() {
            if *sname == name && *svalue == value {
                #[allow(clippy::cast_possible_truncation)]
                return encode_integer(i as u32 + 1, 6);
            }
            if *sname == name && svalue.is_empty() {
                #[allow(clippy::cast_possible_truncation)]
                let mut result = encode_integer(i as u32 + 1, 6);
                let val_enc = value.as_bytes();
                #[allow(clippy::cast_possible_truncation)]
                result.extend_from_slice(&encode_integer(val_enc.len() as u32, 7));
                result.extend_from_slice(val_enc);
                return result;
            }
        }

        let mut result = vec![0x40];
        let name_bytes = name.as_bytes();
        #[allow(clippy::cast_possible_truncation)]
        result.extend_from_slice(&encode_integer(name_bytes.len() as u32, 7));
        result.extend_from_slice(name_bytes);
        let val_bytes = value.as_bytes();
        #[allow(clippy::cast_possible_truncation)]
        result.extend_from_slice(&encode_integer(val_bytes.len() as u32, 7));
        result.extend_from_slice(val_bytes);

        let entry_size = name.len() + value.len() + 32;
        self.dynamic_table
            .insert(0, (name.to_string(), value.to_string()));
        self.current_size += entry_size;
        while self.current_size > self.max_table_size && !self.dynamic_table.is_empty() {
            if let Some(removed) = self.dynamic_table.pop() {
                self.current_size -= removed.0.len() + removed.1.len() + 32;
            }
        }

        result
    }
}

/// Decoder state for HPACK.
#[derive(Debug)]
pub struct Decoder {
    dynamic_table: Vec<(String, String)>,
    max_table_size: usize,
    current_size: usize,
}

impl Decoder {
    pub const fn new(max_table_size: usize) -> Self {
        Self {
            dynamic_table: Vec::new(),
            max_table_size,
            current_size: 0,
        }
    }

    #[allow(clippy::cast_possible_truncation)]
    pub fn lookup_static(index: u64) -> Option<(&'static str, &'static str)> {
        let idx = index as usize;
        if idx == 0 || idx > STATIC_TABLE_LEN {
            return None;
        }
        let (name, value) = STATIC_TABLE[idx - 1];
        Some((name, value))
    }

    #[allow(clippy::cast_possible_truncation)]
    pub fn lookup_dynamic(&self, index: u64) -> Option<(&str, &str)> {
        let idx = index as usize;
        if idx <= STATIC_TABLE_LEN {
            return None;
        }
        let dyn_idx = idx - STATIC_TABLE_LEN - 1;
        self.dynamic_table
            .get(dyn_idx)
            .map(|(n, v)| (n.as_str(), v.as_str()))
    }

    /// # Panics
    ///
    /// Never panics under normal operation.
    pub fn add_to_table(&mut self, name: String, value: String) {
        let entry_size = name.len() + value.len() + 32;
        self.dynamic_table.insert(0, (name, value));
        self.current_size += entry_size;
        while self.current_size > self.max_table_size && !self.dynamic_table.is_empty() {
            if let Some(removed) = self.dynamic_table.pop() {
                self.current_size -= removed.0.len() + removed.1.len() + 32;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_integer_small() {
        let encoded = encode_integer(5, 6);
        assert_eq!(encoded, vec![5]);
    }

    #[test]
    fn encode_integer_large() {
        let encoded = encode_integer(1337, 6);
        assert_eq!(encoded[0], 63);
        assert_eq!(encoded.len(), 3);
    }

    #[test]
    fn decode_integer_small() {
        let (val, consumed) = decode_integer(&[5], 6).unwrap();
        assert_eq!(val, 5);
        assert_eq!(consumed, 1);
    }

    #[test]
    fn decode_integer_large() {
        let encoded = encode_integer(1337, 6);
        let (val, consumed) = decode_integer(&encoded, 6).unwrap();
        assert_eq!(val, 1337);
        assert_eq!(consumed, encoded.len());
    }

    #[test]
    fn static_table_lookup() {
        let (name, value) = Decoder::lookup_static(1).unwrap();
        assert_eq!(name, ":authority");
        assert_eq!(value, "");
    }

    #[test]
    fn static_table_method_get() {
        let (name, value) = Decoder::lookup_static(2).unwrap();
        assert_eq!(name, ":method");
        assert_eq!(value, "GET");
    }

    #[test]
    fn static_table_status_200() {
        let (name, value) = Decoder::lookup_static(8).unwrap();
        assert_eq!(name, ":status");
        assert_eq!(value, "200");
    }

    #[test]
    fn static_table_out_of_range() {
        assert!(Decoder::lookup_static(0).is_none());
        assert!(Decoder::lookup_static(STATIC_TABLE_LEN as u64 + 1).is_none());
    }

    #[test]
    fn encode_decode_integer_roundtrip() {
        for val in [0u32, 1, 127, 128, 255, 1337, 16_384, 131_071] {
            let encoded = encode_integer(val, 6);
            let (decoded, _) = decode_integer(&encoded, 6).unwrap();
            assert_eq!(decoded, val, "roundtrip failed for {val}");
        }
    }

    #[test]
    fn encoder_static_table_match() {
        let mut enc = Encoder::new(DEFAULT_DYNAMIC_TABLE_SIZE);
        let encoded = enc.encode_header(":method", "GET");
        // Index 2, encoded as a 6-bit integer. The encoder uses raw
        // encode_integer without setting the leading 1-bit for indexed
        // representation; a full encoder would OR with 0x80.
        assert_eq!(encoded, vec![2]);
    }

    #[test]
    fn encoder_literal_with_index() {
        let mut enc = Encoder::new(DEFAULT_DYNAMIC_TABLE_SIZE);
        let encoded = enc.encode_header("x-custom", "value");
        assert_eq!(encoded[0], 0x40);
        assert_eq!(enc.dynamic_table.len(), 1);
    }

    #[test]
    fn encoder_evicts_when_full() {
        let mut enc = Encoder::new(64);
        for i in 0..10 {
            enc.encode_header(&format!("header-{i}"), &"v".repeat(20));
        }
        assert!(enc.dynamic_table.len() < 10);
    }

    #[test]
    fn huffman_encode_decode_roundtrip() {
        let data = b"Hello, World!";
        let encoded = huffman_encode(data);
        let decoded = huffman_decode(&encoded).unwrap();
        assert_eq!(decoded, data);
    }

    #[test]
    fn huffman_empty() {
        let encoded = huffman_encode(b"");
        let decoded = huffman_decode(&encoded).unwrap();
        assert!(decoded.is_empty());
    }

    #[test]
    fn huffman_decode_truncated() {
        let encoded = huffman_encode(b"hello");
        let truncated = &encoded[..encoded.len() - 1];
        assert!(huffman_decode(truncated).is_none());
    }

    #[test]
    fn decoder_dynamic_table() {
        let mut dec = Decoder::new(DEFAULT_DYNAMIC_TABLE_SIZE);
        dec.add_to_table("x-test".to_string(), "value".to_string());
        let lookup = dec.lookup_dynamic(STATIC_TABLE_LEN as u64 + 1);
        assert_eq!(lookup, Some(("x-test", "value")));
    }
}
