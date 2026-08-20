use std::io;

use super::{GatewayResponse, ProtocolAdapter, ReadWritePair, parse_http_response};

#[derive(Debug)]
pub struct ScgiAdapter;

impl ProtocolAdapter for ScgiAdapter {
    fn exchange(
        &self,
        upstream: &mut dyn ReadWritePair,
        request_head: &[u8],
        request_body: &[u8],
    ) -> io::Result<GatewayResponse> {
        let params = parse_head_to_params(request_head);
        let wire = encode_scgi_request(&params, request_body);
        upstream.write_all(&wire)?;
        let mut response = Vec::new();
        upstream.read_to_end(&mut response)?;
        Ok(parse_http_response(&response))
    }
}

pub fn encode_scgi_request(headers: &[(Vec<u8>, Vec<u8>)], body: &[u8]) -> Vec<u8> {
    let mut header_buf = Vec::new();
    for (key, value) in headers {
        header_buf.extend_from_slice(key);
        header_buf.push(0);
        header_buf.extend_from_slice(value);
        header_buf.push(0);
    }
    let header_len = header_buf.len();
    let mut out = Vec::with_capacity(header_len + 32 + body.len());
    out.extend_from_slice(header_len.to_string().as_bytes());
    out.push(b':');
    out.extend_from_slice(&header_buf);
    out.push(b',');
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
            let scgi_name = format!("HTTP_{}", key.to_uppercase().replace('-', "_"));
            params.push((scgi_name.into_bytes(), value.as_bytes().to_vec()));
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
    fn encode_scgi_request_format() {
        let headers = vec![
            (b"REQUEST_METHOD".to_vec(), b"GET".to_vec()),
            (b"SCRIPT_NAME".to_vec(), b"/test".to_vec()),
        ];
        let body = b"";
        let wire = encode_scgi_request(&headers, body);
        let header_str = String::from_utf8_lossy(&wire);
        let colon_pos = header_str.find(':').unwrap();
        let header_len: usize = header_str[..colon_pos].parse().unwrap();
        assert_eq!(wire[colon_pos + 1 + header_len], b',');
        let inner = &wire[colon_pos + 1..colon_pos + 1 + header_len];
        let needle = b"REQUEST_METHOD\0GET\0";
        assert!(inner.windows(needle.len()).any(|w| w == needle));
    }

    #[test]
    fn adapter_round_trip() {
        let params = vec![
            (b"REQUEST_METHOD".to_vec(), b"GET".to_vec()),
            (b"SCRIPT_NAME".to_vec(), b"/app".to_vec()),
        ];
        let body = b"hello";
        let wire = encode_scgi_request(&params, body);
        let http_response = b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nworld";
        let mut mock_io = MockIo::new(&wire, http_response);
        let adapter = ScgiAdapter;
        let head = b"GET /app HTTP/1.1\r\nHost: localhost\r\n\r\n";
        let resp = adapter.exchange(&mut mock_io, head, b"").unwrap();
        assert_eq!(resp.status, 200);
        assert_eq!(resp.body, b"world");
    }

    #[test]
    fn params_are_null_separated() {
        let headers = vec![(b"FOO".to_vec(), b"bar".to_vec())];
        let wire = encode_scgi_request(&headers, b"");
        let header_start = wire.iter().position(|&b| b == b':').unwrap() + 1;
        let header_end = wire.iter().position(|&b| b == b',').unwrap();
        let inner = &wire[header_start..header_end];
        assert!(inner.contains(&0));
        assert_eq!(inner, b"FOO\0bar\0");
    }

    #[test]
    fn body_is_appended_after_header_comma() {
        let headers = vec![(b"KEY".to_vec(), b"VAL".to_vec())];
        let body = b"payload";
        let wire = encode_scgi_request(&headers, body);
        let comma_pos = wire.iter().position(|&b| b == b',').unwrap();
        assert_eq!(&wire[comma_pos + 1..], body);
    }

    #[test]
    fn parse_head_to_params_extracts_common_fields() {
        let head =
            b"POST /submit HTTP/1.1\r\nHost: myhost:9000\r\nContent-Type: text/plain\r\n\r\n";
        let params = parse_head_to_params(head);
        let find = |key: &[u8]| {
            params
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.clone())
        };
        assert_eq!(find(b"REQUEST_METHOD"), Some(b"POST".to_vec()));
        assert_eq!(find(b"SCRIPT_NAME"), Some(b"/submit".to_vec()));
        assert_eq!(find(b"QUERY_STRING"), Some(b"".to_vec()));
        assert_eq!(find(b"SERVER_NAME"), Some(b"myhost".to_vec()));
        assert_eq!(find(b"SERVER_PORT"), Some(b"9000".to_vec()));
        assert_eq!(find(b"CONTENT_TYPE"), Some(b"text/plain".to_vec()));
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
