use std::io::{self, Read, Write};

use super::{GatewayResponse, ProtocolAdapter, ReadWritePair, parse_http_response};

const VERSION: u8 = 1;
const TYPE_BEGIN_REQUEST: u8 = 1;
const TYPE_END_REQUEST: u8 = 3;
const TYPE_PARAMS: u8 = 4;
const TYPE_STDIN: u8 = 5;
const TYPE_STDOUT: u8 = 6;
const ROLE_RESPONDER: u16 = 1;
const FLAG_KEEP_CONN: u8 = 1;
const REQUEST_COMPLETE: u8 = 0;
const MAX_RECORD_CONTENT: usize = 65535;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub r#type: u8,
    pub request_id: u16,
    pub content: Vec<u8>,
}

impl Record {
    #[allow(clippy::cast_possible_truncation)]
    pub fn encode(&self, w: &mut dyn Write) -> io::Result<()> {
        let content_len = u16::try_from(self.content.len())
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        let pad = padding_for(self.content.len());
        let header = [
            VERSION,
            self.r#type,
            (self.request_id >> 8) as u8,
            self.request_id as u8,
            (content_len >> 8) as u8,
            content_len as u8,
            u8::try_from(pad).unwrap_or(u8::MAX),
            0,
        ];
        w.write_all(&header)?;
        w.write_all(&self.content)?;
        if pad > 0 {
            w.write_all(&vec![0u8; pad])?;
        }
        Ok(())
    }

    pub fn decode(r: &mut dyn Read) -> io::Result<Self> {
        let mut header = [0u8; 8];
        r.read_exact(&mut header)?;
        let version = header[0];
        if version != VERSION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unsupported FastCGI version: {version}"),
            ));
        }
        let r#type = header[1];
        let request_id = u16::from_be_bytes([header[2], header[3]]);
        let content_len = u16::from_be_bytes([header[4], header[5]]) as usize;
        let padding = header[6] as usize;
        let mut content = vec![0u8; content_len];
        r.read_exact(&mut content)?;
        if padding > 0 {
            let mut skip = vec![0u8; padding];
            r.read_exact(&mut skip)?;
        }
        Ok(Self {
            r#type,
            request_id,
            content,
        })
    }
}

pub const fn padding_for(content_len: usize) -> usize {
    (8 - (content_len % 8)) % 8
}

pub fn encode_params(params: &[(Vec<u8>, Vec<u8>)]) -> io::Result<Vec<u8>> {
    let mut buf = Vec::new();
    for (name, value) in params {
        encode_length(&mut buf, name.len());
        buf.extend_from_slice(name);
        encode_length(&mut buf, value.len());
        buf.extend_from_slice(value);
    }
    encode_length(&mut buf, 0);
    Ok(buf)
}

pub fn decode_params(data: &[u8]) -> io::Result<Vec<(Vec<u8>, Vec<u8>)>> {
    let mut params = Vec::new();
    let mut pos = 0;
    while pos < data.len() {
        let (name_len, consumed) = decode_length(data, pos)?;
        pos += consumed;
        if name_len == 0 {
            break;
        }
        if pos + name_len > data.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "truncated param name",
            ));
        }
        let name = data[pos..pos + name_len].to_vec();
        pos += name_len;
        let (value_len, consumed) = decode_length(data, pos)?;
        pos += consumed;
        if pos + value_len > data.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "truncated param value",
            ));
        }
        let value = data[pos..pos + value_len].to_vec();
        pos += value_len;
        params.push((name, value));
    }
    Ok(params)
}

pub fn encode_params_chunked(
    params: &[(Vec<u8>, Vec<u8>)],
    request_id: u16,
) -> io::Result<Vec<u8>> {
    let encoded = encode_params(params)?;
    let mut out = Vec::new();
    let mut pos = 0;
    while pos < encoded.len() {
        let end = (pos + MAX_RECORD_CONTENT).min(encoded.len());
        let record = Record {
            r#type: TYPE_PARAMS,
            request_id,
            content: encoded[pos..end].to_vec(),
        };
        record.encode(&mut out)?;
        pos = end;
    }
    let terminator = Record {
        r#type: TYPE_PARAMS,
        request_id,
        content: Vec::new(),
    };
    terminator.encode(&mut out)?;
    Ok(out)
}

pub fn encode_stdin(data: &[u8], request_id: u16) -> io::Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut pos = 0;
    while pos < data.len() {
        let end = (pos + MAX_RECORD_CONTENT).min(data.len());
        let record = Record {
            r#type: TYPE_STDIN,
            request_id,
            content: data[pos..end].to_vec(),
        };
        record.encode(&mut out)?;
        pos = end;
    }
    let terminator = Record {
        r#type: TYPE_STDIN,
        request_id,
        content: Vec::new(),
    };
    terminator.encode(&mut out)?;
    Ok(out)
}

pub fn read_response(r: &mut dyn Read) -> io::Result<GatewayResponse> {
    let mut stdout = Vec::new();
    loop {
        let record = Record::decode(r)?;
        match record.r#type {
            TYPE_STDOUT => {
                stdout.extend_from_slice(&record.content);
            }
            TYPE_END_REQUEST => {
                if record.content.is_empty() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "empty END_REQUEST body",
                    ));
                }
                let protocol_status = record.content[0];
                if protocol_status != REQUEST_COMPLETE {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("unexpected protocol_status: {protocol_status}"),
                    ));
                }
                return Ok(parse_http_response(&stdout));
            }
            _ => {}
        }
    }
}

#[derive(Debug, Default)]
pub struct FastcgiAdapter {
    pub keep_conn: bool,
}

impl ProtocolAdapter for FastcgiAdapter {
    fn exchange(
        &self,
        upstream: &mut dyn ReadWritePair,
        request_head: &[u8],
        request_body: &[u8],
    ) -> io::Result<GatewayResponse> {
        let request_id: u16 = 1;
        let params = parse_head_to_params(request_head);
        let flags = if self.keep_conn { FLAG_KEEP_CONN } else { 0 };
        let mut buf = Vec::new();
        let begin = Record {
            r#type: TYPE_BEGIN_REQUEST,
            request_id,
            content: {
                let mut c = Vec::with_capacity(8);
                c.extend_from_slice(&ROLE_RESPONDER.to_be_bytes());
                c.push(flags);
                c.extend_from_slice(&[0; 3]);
                c
            },
        };
        begin.encode(&mut buf)?;
        let params_wire = encode_params_chunked(&params, request_id)?;
        buf.extend_from_slice(&params_wire);
        let stdin_wire = encode_stdin(request_body, request_id)?;
        buf.extend_from_slice(&stdin_wire);
        upstream.write_all(&buf)?;
        read_response(upstream)
    }
}

#[allow(clippy::cast_possible_truncation)]
fn encode_length(buf: &mut Vec<u8>, len: usize) {
    if len > 127 {
        buf.push(0x80 | ((len >> 24) & 0x7F) as u8);
        buf.push((len >> 16) as u8);
        buf.push((len >> 8) as u8);
    }
    buf.push(len as u8);
}

fn decode_length(data: &[u8], pos: usize) -> io::Result<(usize, usize)> {
    if pos >= data.len() {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "unexpected end in param length",
        ));
    }
    let b = data[pos];
    if b & 0x80 == 0 {
        Ok((b as usize, 1))
    } else if pos + 4 > data.len() {
        Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "truncated long param length",
        ))
    } else {
        let len = (((b & 0x7F) as usize) << 24)
            | ((data[pos + 1] as usize) << 16)
            | ((data[pos + 2] as usize) << 8)
            | (data[pos + 3] as usize);
        Ok((len, 4))
    }
}

fn parse_head_to_params(head: &[u8]) -> Vec<(Vec<u8>, Vec<u8>)> {
    let mut params = Vec::new();
    let head_str = String::from_utf8_lossy(head);
    let mut lines = head_str.split("\r\n");
    let request_line = lines.next().unwrap_or("");
    let mut parts = request_line.splitn(3, ' ');
    let method = parts.next().unwrap_or("");
    let uri = parts.next().unwrap_or("");
    let (script_name, query_string) = match uri.split_once('?') {
        Some((path, qs)) => (path.as_bytes().to_vec(), qs.as_bytes().to_vec()),
        None => (uri.as_bytes().to_vec(), Vec::new()),
    };
    params.push((b"REQUEST_METHOD".to_vec(), method.as_bytes().to_vec()));
    params.push((b"SCRIPT_NAME".to_vec(), script_name));
    params.push((b"QUERY_STRING".to_vec(), query_string));
    let mut server_name = Vec::new();
    let mut server_port = Vec::new();
    let mut content_length = Vec::new();
    let mut content_type = Vec::new();
    for line in lines {
        if line.is_empty() {
            break;
        }
        if let Some((key, value)) = line.split_once(':') {
            let value = value.trim_start();
            match key.to_ascii_lowercase().as_str() {
                "host" => {
                    if let Some((h, p)) = value.split_once(':') {
                        server_name.extend_from_slice(h.as_bytes());
                        server_port.extend_from_slice(p.as_bytes());
                    } else {
                        server_name.extend_from_slice(value.as_bytes());
                        server_port.extend_from_slice(b"80");
                    }
                }
                "content-length" => {
                    content_length.extend_from_slice(value.as_bytes());
                }
                "content-type" => {
                    content_type.extend_from_slice(value.as_bytes());
                }
                _ => {}
            }
            let fcgi_name = format!("HTTP_{}", key.to_uppercase().replace('-', "_"));
            params.push((fcgi_name.into_bytes(), value.as_bytes().to_vec()));
        }
    }
    if !server_name.is_empty() {
        params.push((b"SERVER_NAME".to_vec(), server_name));
    }
    if !server_port.is_empty() {
        params.push((b"SERVER_PORT".to_vec(), server_port));
    }
    if !content_length.is_empty() {
        params.push((b"CONTENT_LENGTH".to_vec(), content_length));
    }
    if !content_type.is_empty() {
        params.push((b"CONTENT_TYPE".to_vec(), content_type));
    }
    params
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_round_trip() {
        let record = Record {
            r#type: TYPE_PARAMS,
            request_id: 42,
            content: b"hello".to_vec(),
        };
        let mut buf = Vec::new();
        record.encode(&mut buf).unwrap();
        let decoded = Record::decode(&mut &buf[..]).unwrap();
        assert_eq!(record, decoded);
    }

    #[test]
    fn encode_decode_params() {
        let params = vec![
            (b"REQUEST_METHOD".to_vec(), b"GET".to_vec()),
            (b"SCRIPT_NAME".to_vec(), b"/index.php".to_vec()),
        ];
        let encoded = encode_params(&params).unwrap();
        let decoded = decode_params(&encoded).unwrap();
        assert_eq!(params, decoded);
    }

    #[test]
    fn padding_is_multiple_of_8() {
        assert_eq!(padding_for(0), 0);
        assert_eq!(padding_for(1), 7);
        assert_eq!(padding_for(7), 1);
        assert_eq!(padding_for(8), 0);
        assert_eq!(padding_for(9), 7);
    }

    #[test]
    fn encode_params_terminates_with_empty() {
        let params = vec![(b"FOO".to_vec(), b"bar".to_vec())];
        let wire = encode_params_chunked(&params, 1).unwrap();
        let mut cursor = &wire[..];
        let mut saw_empty = false;
        while !cursor.is_empty() {
            let record = Record::decode(&mut cursor).unwrap();
            if record.r#type == TYPE_PARAMS && record.content.is_empty() {
                saw_empty = true;
            }
        }
        assert!(saw_empty);
    }

    #[test]
    fn decode_params_round_trip() {
        let params = vec![
            (b"REQUEST_METHOD".to_vec(), b"POST".to_vec()),
            (b"CONTENT_TYPE".to_vec(), b"application/json".to_vec()),
            (b"QUERY_STRING".to_vec(), b"a=1&b=2".to_vec()),
        ];
        let encoded = encode_params(&params).unwrap();
        let decoded = decode_params(&encoded).unwrap();
        assert_eq!(params, decoded);
    }

    #[test]
    fn encode_stdin_round_trip() {
        let data = b"request body content";
        let wire = encode_stdin(data, 1).unwrap();
        let mut cursor = &wire[..];
        let mut collected = Vec::new();
        while !cursor.is_empty() {
            let record = Record::decode(&mut cursor).unwrap();
            if record.r#type == TYPE_STDIN {
                collected.extend_from_slice(&record.content);
            }
        }
        assert_eq!(collected, data);
    }

    #[test]
    fn parse_head_to_params_extracts_method_and_uri() {
        let head = b"GET /app/test?x=1 HTTP/1.1\r\nHost: example.com:8080\r\n\r\n";
        let params = parse_head_to_params(head);
        let find = |key: &[u8]| {
            params
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.clone())
        };
        assert_eq!(find(b"REQUEST_METHOD"), Some(b"GET".to_vec()));
        assert_eq!(find(b"SCRIPT_NAME"), Some(b"/app/test".to_vec()));
        assert_eq!(find(b"QUERY_STRING"), Some(b"x=1".to_vec()));
        assert_eq!(find(b"SERVER_NAME"), Some(b"example.com".to_vec()));
        assert_eq!(find(b"SERVER_PORT"), Some(b"8080".to_vec()));
    }

    #[test]
    fn length_encode_round_trip() {
        let mut buf = Vec::new();
        encode_length(&mut buf, 127);
        assert_eq!(buf, vec![127]);
        buf.clear();
        encode_length(&mut buf, 128);
        assert_eq!(buf.len(), 4);
        assert_eq!(buf[0], 0x80);
        let (val, _) = decode_length(&buf, 0).unwrap();
        assert_eq!(val, 128);
    }

    #[test]
    fn record_encode_writes_correct_header() {
        let record = Record {
            r#type: TYPE_STDIN,
            request_id: 1,
            content: b"test".to_vec(),
        };
        let mut buf = Vec::new();
        record.encode(&mut buf).unwrap();
        assert_eq!(buf[0], VERSION);
        assert_eq!(buf[1], TYPE_STDIN);
        assert_eq!(buf[2], 0);
        assert_eq!(buf[3], 1);
        let content_len = u16::from_be_bytes([buf[4], buf[5]]);
        assert_eq!(content_len, 4);
        assert_eq!(buf[6], u8::try_from(padding_for(4)).unwrap());
        assert_eq!(buf[7], 0);
        assert_eq!(&buf[8..12], b"test");
    }

    #[test]
    fn begin_request_record_format() {
        let record = Record {
            r#type: TYPE_BEGIN_REQUEST,
            request_id: 1,
            content: {
                let mut c = Vec::with_capacity(8);
                c.extend_from_slice(&ROLE_RESPONDER.to_be_bytes());
                c.push(FLAG_KEEP_CONN);
                c.extend_from_slice(&[0; 3]);
                c
            },
        };
        let mut buf = Vec::new();
        record.encode(&mut buf).unwrap();
        assert_eq!(buf[8], 0);
        assert_eq!(buf[9], 1);
        assert_eq!(buf[10], FLAG_KEEP_CONN);
    }
}
