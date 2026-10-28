use super::*;
use crate::web_search::tests::{client, endpoint, ok};

#[test]
fn fetch_configuration_and_urls_follow_the_service_contract() {
    assert!(factory().build(json!({"api_key_env":"DS_KEY"})).is_ok());
    for config in [
        json!({}),
        json!({"api_key_env":""}),
        json!({"api_key_env":"KEY","max_bytes":0}),
        json!({"api_key_env":"KEY","base_url":"https://user:secret@example.com"}),
    ] {
        assert!(factory().build(config).is_err());
    }
    assert!(validate_target("https://example.com/article?q=web#section").is_ok());
    for url in [
        "file:///tmp/page",
        "/relative",
        "https://user:secret@example.com",
        "ftp://example.com",
    ] {
        assert!(validate_target(url).is_err(), "{url}");
    }
    assert!(validate_target(&format!("https://example.com/{}", "a".repeat(8192))).is_err());
}

#[tokio::test]
async fn fetch_reads_markdown_and_errors_without_disclosing_response_secrets() {
    let (base, incoming) = endpoint(ok(
        &json!({"title":"Page","content":"# Content\n\nArticle"}),
    ))
    .await;
    let bytes = read_response(
        client()
            .post(format!("{base}/v1/tools/fetch"))
            .bearer_auth("fixture-secret")
            .json(&json!({"url":"https://example.com/article"})),
        "fetch",
        1024,
    )
    .await
    .unwrap();
    let page: FetchResponse = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(page.title, "Page");
    assert_eq!(page.content, "# Content\n\nArticle");
    let request = incoming.await.unwrap();
    assert!(request.starts_with("POST /v1/tools/fetch "));
    assert!(request.contains("authorization: Bearer fixture-secret"));

    for (status, advice) in [
        (302, "rejected"),
        (401, "credential"),
        (402, "top up"),
        (403, "safety policy"),
        (404, "readable"),
        (429, "frequency"),
        (504, "timed out"),
    ] {
        let body = "fixture-secret upstream-private-detail";
        let (base, incoming) = endpoint(format!("HTTP/1.1 {status} Failed\r\nLocation: https://example.invalid\r\nContent-Length: {}\r\n\r\n{body}",body.len())).await;
        let error = read_response(client().post(base), "fetch", 1024)
            .await
            .unwrap_err()
            .to_string();
        assert!(
            error.contains(&format!("HTTP {status}")) && error.contains(advice),
            "{error}"
        );
        assert!(!error.contains("fixture-secret") && !error.contains("upstream-private-detail"));
        incoming.await.unwrap();
    }
}

#[tokio::test]
async fn fetch_and_search_enforce_the_body_limit_without_content_length() {
    let body = "x".repeat(1025);
    let (base, incoming) = endpoint(format!(
        "HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n{body}"
    ))
    .await;
    let error = read_response(client().post(base), "fetch", 1024)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("exceeds 1024 bytes"));
    incoming.await.unwrap();
}
