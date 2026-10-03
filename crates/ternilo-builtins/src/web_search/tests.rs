use super::*;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

async fn endpoint(reply: String) -> (String, tokio::task::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut bytes = Vec::new();
        let mut chunk = [0; 4096];
        loop {
            let n = stream.read(&mut chunk).await.unwrap();
            assert!(n > 0);
            bytes.extend_from_slice(&chunk[..n]);
            if let Some(header_end) = bytes.windows(4).position(|value| value == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&bytes[..header_end]).to_ascii_lowercase();
                let length = headers
                    .lines()
                    .find_map(|line| line.strip_prefix("content-length: "))
                    .map_or(0, |value| value.parse::<usize>().unwrap());
                if bytes.len() >= header_end + 4 + length {
                    break;
                }
            }
        }
        stream.write_all(reply.as_bytes()).await.unwrap();
        String::from_utf8(bytes).unwrap()
    });
    (base, task)
}

fn ok(body: &Value) -> String {
    let body = body.to_string();
    format!(
        "HTTP/1.1 200 OK\r\ncontent-length: {}\r\ncontent-type: application/json\r\nconnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap()
}

#[tokio::test]
async fn services_use_native_auth_and_normalize_bounded_results() {
    for provider in [
        SearchProvider::Searxng,
        SearchProvider::Brave,
        SearchProvider::Tavily,
    ] {
        let results = json!([
            {"title":"Ternilo", "url":"https://example.com/one", "content":"Content", "description":"Brave snippet", "engine":"fixture"},
            {"title":"Two", "url":"https://example.com/two"}
        ]);
        let body = if provider == SearchProvider::Brave {
            json!({"type":"search", "web":{"results":results}})
        } else {
            json!({"results":results})
        };
        let (base, incoming) = endpoint(ok(&body)).await;
        let results = provider
            .search(
                provider.request(
                    &client(),
                    &base,
                    Some("fixture-key"),
                    "a & b",
                    1,
                    Some("en"),
                ),
                1,
            )
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].url, "https://example.com/one");
        let request = incoming.await.unwrap();
        let (headers, body) = request.split_once("\r\n\r\n").unwrap();
        let headers = headers.to_ascii_lowercase();
        assert!(!headers.lines().next().unwrap().contains("fixture-key"));
        match provider {
            SearchProvider::Searxng => {
                assert!(headers.starts_with("get /search?q=a+%26+b&format=json&language=en "));
                assert!(headers.contains("authorization: bearer fixture-key"));
                assert_eq!(results[0].snippet, "Content");
                assert_eq!(results[0].engine, "fixture");
            }
            SearchProvider::Brave => {
                assert!(headers.starts_with("get /web/search?q=a+%26+b&count=1&search_lang=en "));
                assert!(headers.contains("x-subscription-token: fixture-key"));
                assert!(!headers.contains("authorization:"));
                assert_eq!(results[0].snippet, "Brave snippet");
            }
            SearchProvider::Tavily => {
                assert!(headers.starts_with("post /search "));
                assert!(headers.contains("authorization: bearer fixture-key"));
                let body: Value = serde_json::from_str(body).unwrap();
                assert_eq!(body["query"], "a & b");
                assert_eq!(body["max_results"], 1);
                assert_eq!(body["search_depth"], "basic");
                assert_eq!(body["auto_parameters"], false);
                assert_eq!(body["include_raw_content"], false);
                assert_eq!(body["language"], "en");
                assert!(body.get("api_key").is_none());
            }
        }
    }
}

#[tokio::test]
async fn failures_do_not_echo_upstream_secrets_or_query_and_redirects_are_not_followed() {
    for status in [302, 401, 429, 500] {
        let body = "fixture-secret confidential-query";
        let reply = format!(
            "HTTP/1.1 {status} Failed\r\nlocation: https://example.invalid/secret\r\ncontent-length: {}\r\n\r\n{body}",
            body.len()
        );
        let (base, incoming) = endpoint(reply).await;
        let provider = SearchProvider::Brave;
        let error = provider
            .search(
                provider.request(
                    &client(),
                    &base,
                    Some("fixture-secret"),
                    "confidential-query",
                    1,
                    None,
                ),
                1,
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains(&format!("HTTP {status}")));
        assert!(!error.to_string().contains("fixture-secret"));
        assert!(!error.to_string().contains("confidential-query"));
        incoming.await.unwrap();
    }
}

#[tokio::test]
async fn bounded_responses_reject_bad_json_and_oversized_chunked_bodies() {
    let large = "x".repeat(2 * 1024 * 1024 + 1);
    for (reply, expected) in [
        (
            ok(&json!({"private-secret":"not a search response"})),
            "invalid search response",
        ),
        (
            format!(
                "HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\n\r\n{:x}\r\n{large}\r\n0\r\n\r\n",
                large.len()
            ),
            "exceeds",
        ),
    ] {
        let (base, incoming) = endpoint(reply).await;
        let provider = SearchProvider::Tavily;
        let error = provider
            .search(
                provider.request(&client(), &base, Some("key"), "query", 1, None),
                1,
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains(expected));
        assert!(!error.to_string().contains("private-secret"));
        incoming.await.unwrap();
    }
}

#[test]
fn configuration_requires_credentials_and_provider_limits() {
    for provider in [SearchProvider::Brave, SearchProvider::Tavily] {
        let factory = provider_factory(provider);
        assert!(factory.build(json!({"api_key_env":"SEARCH_KEY"})).is_ok());
        for value in [
            json!({}),
            json!({"api_key_env":""}),
            json!({"api_key_env":"KEY", "max_results":21}),
            json!({"api_key_env":"KEY", "base_url":"https://user:secret@example.com"}),
        ] {
            assert!(factory.build(value).is_err());
        }
    }
    assert!(
        factory()
            .build(json!({"base_url":"http://localhost:8888", "max_results":50}))
            .is_ok()
    );
}
