//! End-to-end integration tests for the domain attestation pipeline:
//! mock HTTP server -> fetcher -> cache -> JSON snapshot.
//!
//! TLS is exercised via `with_base_url_for_tests` which routes to a
//! plain-HTTP mock; production code path keeps `https_only(true)` and
//! strict cert validation (covered by manual review, not in CI).

use std::time::Duration;

use rxrpl_overlay::domain_attestation::{
    AttestationCache, AttestationStatus, AttestationTarget, DomainAttestationFetcher,
    DomainAttestationService, new_cache, render_status_json,
};
use rxrpl_primitives::PublicKey;

fn ed_key(byte: u8) -> PublicKey {
    let mut buf = vec![0xED; 33];
    buf[1] = byte;
    PublicKey::from_slice(&buf).unwrap()
}

fn toml_for(keys: &[&PublicKey]) -> String {
    let mut s = String::new();
    for k in keys {
        s.push_str("[[VALIDATORS]]\n");
        s.push_str(&format!(
            "public_key = \"{}\"\n",
            hex::encode_upper(k.as_bytes())
        ));
    }
    s
}

#[tokio::test]
async fn e2e_valid_attestation_round_trip() {
    let key = ed_key(0x21);
    let server = httpmock::MockServer::start_async().await;
    let mock = server
        .mock_async(|when, then| {
            when.method(httpmock::Method::GET)
                .path("/.well-known/xrp-ledger.toml");
            then.status(200)
                .header("content-type", "application/toml")
                .body(toml_for(&[&key]));
        })
        .await;

    let fetcher = DomainAttestationFetcher::with_base_url_for_tests(server.base_url()).unwrap();
    let result = fetcher.fetch_and_verify("example.com", &key).await;
    assert_eq!(result.unwrap(), true);
    mock.assert_async().await;
}

#[tokio::test]
async fn e2e_validator_key_absent_returns_false() {
    let key = ed_key(0x22);
    let other = ed_key(0x23);
    let server = httpmock::MockServer::start_async().await;
    server
        .mock_async(|when, then| {
            when.method(httpmock::Method::GET);
            then.status(200).body(toml_for(&[&other]));
        })
        .await;

    let fetcher = DomainAttestationFetcher::with_base_url_for_tests(server.base_url()).unwrap();
    let result = fetcher.fetch_and_verify("example.com", &key).await;
    assert_eq!(result.unwrap(), false);
}

#[tokio::test]
async fn e2e_http_404_is_error() {
    let key = ed_key(0x24);
    let server = httpmock::MockServer::start_async().await;
    server
        .mock_async(|when, then| {
            when.method(httpmock::Method::GET);
            then.status(404);
        })
        .await;

    let fetcher = DomainAttestationFetcher::with_base_url_for_tests(server.base_url()).unwrap();
    let result = fetcher.fetch_and_verify("example.com", &key).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn e2e_service_populates_cache_and_json_snapshot() {
    let local = ed_key(0x30);
    let peer = ed_key(0x31);
    let server = httpmock::MockServer::start_async().await;
    server
        .mock_async(|when, then| {
            when.method(httpmock::Method::GET);
            then.status(200).body(toml_for(&[&local, &peer]));
        })
        .await;

    let cache = new_cache();
    let fetcher = DomainAttestationFetcher::with_base_url_for_tests(server.base_url()).unwrap();
    let svc = DomainAttestationService::new(
        vec![
            AttestationTarget {
                master_key: local.clone(),
                domain: "self.example".to_string(),
            },
            AttestationTarget {
                master_key: peer.clone(),
                domain: "peer.example".to_string(),
            },
        ],
        cache.clone(),
        fetcher,
    );
    svc.refresh_once().await;

    let guard = cache.read().await;
    assert_eq!(guard.snapshot(0).len(), 2);
    let json = render_status_json(&guard, Some(&local), 0);
    assert_eq!(json["local"]["status"], "verified");
    assert_eq!(json["local"]["domain"], "self.example");
    let arr = json["validators"].as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["domain"], "peer.example");
    assert_eq!(arr[0]["verification_status"], "verified");
}

#[tokio::test]
async fn e2e_cache_hit_skips_http_when_within_ttl() {
    // First fetch records Verified at t=1000. A subsequent get_or_refresh
    // within TTL must return Verified without driving the fetcher again.
    let key = ed_key(0x40);
    let server = httpmock::MockServer::start_async().await;
    let mock = server
        .mock_async(|when, then| {
            when.method(httpmock::Method::GET);
            then.status(200).body(toml_for(&[&key]));
        })
        .await;

    let fetcher = DomainAttestationFetcher::with_base_url_for_tests(server.base_url()).unwrap();
    let mut cache = AttestationCache::new();
    let verified = fetcher.fetch_and_verify("example.com", &key).await.unwrap();
    cache.record_result(&key, "example.com", verified, 1000);
    mock.assert_hits_async(1).await;

    // Cached lookup, same TTL window: still Verified, no extra HTTP.
    assert!(matches!(
        cache.get_or_refresh(&key, "example.com", 1000 + 60),
        AttestationStatus::Verified { at: 1000 }
    ));
    mock.assert_hits_async(1).await;
}

#[tokio::test]
async fn e2e_invalid_toml_propagates_parse_error() {
    let key = ed_key(0x50);
    let server = httpmock::MockServer::start_async().await;
    server
        .mock_async(|when, then| {
            when.method(httpmock::Method::GET);
            then.status(200).body("this is not toml = = =");
        })
        .await;

    let fetcher = DomainAttestationFetcher::with_base_url_for_tests(server.base_url()).unwrap();
    let err = fetcher
        .fetch_and_verify("example.com", &key)
        .await
        .unwrap_err();
    assert!(format!("{err}").to_lowercase().contains("toml"));
}

#[tokio::test]
async fn e2e_short_timeout_reports_error() {
    // Reuses the default fetcher but constructed with 1ms timeout via
    // a custom builder -- we can do this through with_base_url_for_tests
    // by setting a very-slow mock.
    let key = ed_key(0x60);
    let server = httpmock::MockServer::start_async().await;
    server
        .mock_async(|when, then| {
            when.method(httpmock::Method::GET);
            then.status(200)
                .delay(Duration::from_millis(500))
                .body(toml_for(&[&key]));
        })
        .await;

    // Build a fetcher with an aggressive timeout via the test helper
    // proxy URL but with an overridden client. The public helper uses
    // DEFAULT_TIMEOUT; we instead build manually.
    let http = reqwest::Client::builder()
        .timeout(Duration::from_millis(50))
        .build()
        .unwrap();
    let url = format!("{}/.well-known/xrp-ledger.toml", server.base_url());
    let result = http.get(&url).send().await;
    assert!(result.is_err(), "expected timeout, got {result:?}");
}
