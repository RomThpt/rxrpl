/// Minimal HTTP/1.1 parser for the XRPL peer protocol upgrade handshake.
///
/// Handles formatting and parsing of HTTP upgrade requests/responses
/// used during the rippled-compatible peer handshake.

/// Format an HTTP/1.1 upgrade request with the given headers.
pub fn format_http_request(headers: &[(String, String)]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(512);
    buf.extend_from_slice(b"GET / HTTP/1.1\r\n");
    for (name, value) in headers {
        buf.extend_from_slice(name.as_bytes());
        buf.extend_from_slice(b": ");
        buf.extend_from_slice(value.as_bytes());
        buf.extend_from_slice(b"\r\n");
    }
    buf.extend_from_slice(b"\r\n");
    buf
}

/// Format an HTTP/1.1 response with the given status, reason, and headers.
pub fn format_http_response(status: u16, reason: &str, headers: &[(String, String)]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(512);
    buf.extend_from_slice(format!("HTTP/1.1 {status} {reason}\r\n").as_bytes());
    for (name, value) in headers {
        buf.extend_from_slice(name.as_bytes());
        buf.extend_from_slice(b": ");
        buf.extend_from_slice(value.as_bytes());
        buf.extend_from_slice(b"\r\n");
    }
    buf.extend_from_slice(b"\r\n");
    buf
}

/// Parse an HTTP/1.1 response, returning (status_code, headers).
///
/// Expects the full response up to and including the `\r\n\r\n` terminator.
pub fn parse_http_response(buf: &[u8]) -> Result<(u16, Vec<(String, String)>), String> {
    let text = std::str::from_utf8(buf).map_err(|e| format!("invalid UTF-8: {e}"))?;

    let mut lines = text.split("\r\n");

    let status_line = lines.next().ok_or("empty response")?;
    // "HTTP/1.1 101 Switching Protocols"
    let mut parts = status_line.splitn(3, ' ');
    let _version = parts.next().ok_or("missing HTTP version")?;
    let status_str = parts.next().ok_or("missing status code")?;
    let status: u16 = status_str
        .parse()
        .map_err(|_| format!("invalid status code: {status_str}"))?;

    let mut headers = Vec::new();
    for line in lines {
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.push((name.trim().to_string(), value.trim().to_string()));
        }
    }

    Ok((status, headers))
}

/// Parse an HTTP/1.1 request, returning headers.
///
/// Expects the full request up to and including the `\r\n\r\n` terminator.
pub fn parse_http_request(buf: &[u8]) -> Result<Vec<(String, String)>, String> {
    let text = std::str::from_utf8(buf).map_err(|e| format!("invalid UTF-8: {e}"))?;

    let mut lines = text.split("\r\n");

    let request_line = lines.next().ok_or("empty request")?;
    // Verify it looks like "GET / HTTP/1.1"
    if !request_line.starts_with("GET ") {
        return Err(format!("unexpected request line: {request_line}"));
    }

    let mut headers = Vec::new();
    for line in lines {
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.push((name.trim().to_string(), value.trim().to_string()));
        }
    }

    Ok(headers)
}

/// Look up a header value by name (case-insensitive).
pub fn get_header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    let name_lower = name.to_lowercase();
    headers
        .iter()
        .find(|(n, _)| n.to_lowercase() == name_lower)
        .map(|(_, v)| v.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_and_parse_request() {
        let headers = vec![
            ("Upgrade".to_string(), "XRPL/2.2".to_string()),
            ("Connection".to_string(), "Upgrade".to_string()),
            ("Public-Key".to_string(), "ED0000".to_string()),
        ];
        let raw = format_http_request(&headers);
        let text = std::str::from_utf8(&raw).unwrap();
        assert!(text.starts_with("GET / HTTP/1.1\r\n"));
        assert!(text.contains("Upgrade: XRPL/2.2\r\n"));
        assert!(text.ends_with("\r\n\r\n"));

        let parsed = parse_http_request(&raw).unwrap();
        assert_eq!(parsed.len(), 3);
        assert_eq!(get_header(&parsed, "upgrade"), Some("XRPL/2.2"));
        assert_eq!(get_header(&parsed, "Public-Key"), Some("ED0000"));
    }

    #[test]
    fn format_and_parse_response() {
        let headers = vec![
            ("Upgrade".to_string(), "XRPL/2.2".to_string()),
            ("Connection".to_string(), "Upgrade".to_string()),
        ];
        let raw = format_http_response(101, "Switching Protocols", &headers);
        let text = std::str::from_utf8(&raw).unwrap();
        assert!(text.starts_with("HTTP/1.1 101 Switching Protocols\r\n"));
        assert!(text.ends_with("\r\n\r\n"));

        let (status, parsed) = parse_http_response(&raw).unwrap();
        assert_eq!(status, 101);
        assert_eq!(parsed.len(), 2);
        assert_eq!(get_header(&parsed, "Upgrade"), Some("XRPL/2.2"));
    }

    #[test]
    fn parse_response_error_cases() {
        assert!(parse_http_response(b"").is_err());
        assert!(parse_http_response(b"HTTP/1.1 ABC\r\n\r\n").is_err());
    }

    #[test]
    fn parse_request_error_cases() {
        assert!(parse_http_request(b"POST / HTTP/1.1\r\n\r\n").is_err());
    }

    #[test]
    fn get_header_case_insensitive() {
        let headers = vec![("Content-Type".to_string(), "text/html".to_string())];
        assert_eq!(get_header(&headers, "content-type"), Some("text/html"));
        assert_eq!(get_header(&headers, "CONTENT-TYPE"), Some("text/html"));
        assert_eq!(get_header(&headers, "X-Missing"), None);
    }
}
