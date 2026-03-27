#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Fuzz HTTP request parsing used during the XRPL peer handshake
    let _ = rxrpl_overlay::http::parse_http_request(data);

    // Fuzz HTTP response parsing
    let _ = rxrpl_overlay::http::parse_http_response(data);

    // Fuzz protobuf TMHello decoding (the P2P hello message)
    use prost::Message;
    let _ = rxrpl_p2p_proto::proto::TmHello::decode(data);

    // If valid UTF-8, try to extract handshake headers
    if let Ok(_text) = std::str::from_utf8(data) {
        // Build fake headers and test get_header with various patterns
        if let Ok(headers) = rxrpl_overlay::http::parse_http_request(data) {
            let _ = rxrpl_overlay::http::get_header(&headers, "Public-Key");
            let _ = rxrpl_overlay::http::get_header(&headers, "Session-Signature");
            let _ = rxrpl_overlay::http::get_header(&headers, "Network-ID");
            let _ = rxrpl_overlay::http::get_header(&headers, "Upgrade");
            let _ = rxrpl_overlay::http::get_header(&headers, "Closed-Ledger");
        }
        if let Ok((_status, headers)) = rxrpl_overlay::http::parse_http_response(data) {
            let _ = rxrpl_overlay::http::get_header(&headers, "Public-Key");
            let _ = rxrpl_overlay::http::get_header(&headers, "Session-Signature");
        }
    }
});
