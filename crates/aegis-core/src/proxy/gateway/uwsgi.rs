use std::io;

use super::{GatewayResponse, ProtocolAdapter, ReadWritePair, parse_http_response};

const MODIFIER1: u8 = 0;
const MODIFIER2: u8 = 17;

#[derive(Debug)]
pub struct UwsgiAdapter;

impl ProtocolAdapter for UwsgiAdapter {
    fn exchange(
        &self,
        upstream: &mut dyn ReadWritePair,
        request_head: &[u8],
        request_body: &[u8],
    ) -> io::Result<GatewayResponse> {
        let params = parse_head_to_params(request_head);
        let wire = encode_uwsgi_request(&params, request_body);
        upstream.write_all(&wire)?;
        let mut response = Vec::new();
        upstream.read_to_end(&mut response)?;
        Ok(parse_http_response(&response))
    }
}

#[allow(clippy::cast_possible_truncation)]
pub fn encode_uwsgi_request(headers: &[(Vec<u8>, Vec<u8>)], body: &[u8]) -> Vec<u8> {
    let mut kv_buf = Vec::new();
    for (key, value) in headers {
        kv_buf.extend_from_slice(key);
        kv_buf.push(0);
        kv_buf.extend_from_slice(value);
        kv_buf.push(0);
    }
    let datasize = kv_buf.len() + body.len();
    let mut out = Vec::with_capacity(5 + datasize);
    out.push(MODIFIER1);
    out.push(((datasize >> 16) & 0xFF) as u8);
    out.push(((datasize >> 8) & 0xFF) as u8);
    out.push((datasize & 0xFF) as u8);
    out.push(MODIFIER2);
    out.extend_from_slice(&kv_buf);
    out.extend_from_slice(body);
    out
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
            let uwsgi_name = format!("HTTP_{}", key.to_uppercase().replace('-', "_"));
            params.push((uwsgi_name.into_bytes(), value.as_bytes().to_vec()));
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
    use std::io::{Read, Write};

    #[test]
    fn encode_uwsgi_request_format() {
        let headers = vec![
            (b"REQUEST_METHOD".to_vec(), b"GET".to_vec()),
            (b"SCRIPT_NAME".to_vec(), b"/test".to_vec()),
        ];
        let body = b"";
        let wire = encode_uwsgi_request(&headers, body);
        assert_eq!(wire[0], MODIFIER1);
        let datasize = ((wire[1] as usize) << 16) | ((wire[2] as usize) << 8) | (wire[3] as usize);
        assert_eq!(wire[4], MODIFIER2);
        let kv_and_body = &wire[5..5 + datasize];
        let body_offset = kv_and_body.len() - body.len();
        let kv = &kv_and_body[..body_offset];
        let needle = b"REQUEST_METHOD\0GET\0";
        assert!(kv.windows(needle.len()).any(|w| w == needle));
    }

    #[test]
    fn header_encoding() {
        let headers = vec![
            (b"SERVER_NAME".to_vec(), b"myhost".to_vec()),
            (b"SERVER_PORT".to_vec(), b"9000".to_vec()),
        ];
        let body = b"payload";
        let wire = encode_uwsgi_request(&headers, body);
        let kv_len = b"SERVER_NAME".len()
            + 1
            + b"myhost".len()
            + 1
            + b"SERVER_PORT".len()
            + 1
            + b"9000".len()
            + 1;
        assert_eq!(wire.len(), 5 + kv_len + body.len());
        assert_eq!(wire[0], 0);
        assert_eq!(wire[4], 17);
        let datasize = ((wire[1] as usize) << 16) | ((wire[2] as usize) << 8) | (wire[3] as usize);
        assert_eq!(datasize, kv_len + body.len());
    }

    #[test]
    fn datasize_is_3_bytes_big_endian() {
        let headers = vec![(b"K".to_vec(), b"V".to_vec())];
        let body = b"";
        let wire = encode_uwsgi_request(&headers, body);
        let datasize = ((wire[1] as usize) << 16) | ((wire[2] as usize) << 8) | (wire[3] as usize);
        assert_eq!(datasize, 4);
    }

    #[test]
    fn body_follows_kv_pairs() {
        let headers = vec![(b"KEY".to_vec(), b"VAL".to_vec())];
        let body = b"test body";
        let wire = encode_uwsgi_request(&headers, body);
        let datasize = ((wire[1] as usize) << 16) | ((wire[2] as usize) << 8) | (wire[3] as usize);
        let payload = &wire[5..5 + datasize];
        assert_eq!(&payload[payload.len() - body.len()..], body);
    }

    #[test]
    fn parse_head_to_params_extracts_common_fields() {
        let head =
            b"GET /path?k=v HTTP/1.1\r\nHost: example.com:8080\r\nContent-Type: text/html\r\n\r\n";
        let params = parse_head_to_params(head);
        let find = |key: &[u8]| {
            params
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.clone())
        };
        assert_eq!(find(b"REQUEST_METHOD"), Some(b"GET".to_vec()));
        assert_eq!(find(b"SCRIPT_NAME"), Some(b"/path".to_vec()));
        assert_eq!(find(b"QUERY_STRING"), Some(b"k=v".to_vec()));
        assert_eq!(find(b"SERVER_NAME"), Some(b"example.com".to_vec()));
        assert_eq!(find(b"SERVER_PORT"), Some(b"8080".to_vec()));
        assert_eq!(find(b"CONTENT_TYPE"), Some(b"text/html".to_vec()));
    }

    #[test]
    fn adapter_round_trip() {
        let params = vec![
            (b"REQUEST_METHOD".to_vec(), b"GET".to_vec()),
            (b"SCRIPT_NAME".to_vec(), b"/app".to_vec()),
        ];
        let body = b"";
        let wire = encode_uwsgi_request(&params, body);
        let http_response = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok";
        let mut mock_io = MockIo::new(&wire, http_response);
        let adapter = UwsgiAdapter;
        let head = b"GET /app HTTP/1.1\r\nHost: localhost\r\n\r\n";
        let resp = adapter.exchange(&mut mock_io, head, b"").unwrap();
        assert_eq!(resp.status, 200);
        assert_eq!(resp.body, b"ok");
    }

    struct MockIo {
        written: Vec<u8>,
        recv_buf: Vec<u8>,
        recv_pos: usize,
    }

    impl MockIo {
        fn new(_expect_send: &[u8], return_recv: &[u8]) -> Self {
            Self {
                written: Vec::new(),
                recv_buf: return_recv.to_vec(),
                recv_pos: 0,
            }
        }
    }

    impl Read for MockIo {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let remaining = &self.recv_buf[self.recv_pos..];
            let n = remaining.len().min(buf.len());
            buf[..n].copy_from_slice(&remaining[..n]);
            self.recv_pos += n;
            Ok(n)
        }
    }

    impl Write for MockIo {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.written.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
}
