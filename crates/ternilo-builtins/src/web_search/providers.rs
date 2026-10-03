use reqwest::{Client, RequestBuilder};
use serde::Deserialize;
use serde_json::json;
use ternilo_protocol::HarnessError;

use super::{BRAVE_KIND, KIND, SearchResult, TAVILY_KIND};

const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum SearchProvider {
    Searxng,
    Brave,
    Tavily,
}

impl SearchProvider {
    pub(super) const fn kind(self) -> &'static str {
        match self {
            Self::Searxng => KIND,
            Self::Brave => BRAVE_KIND,
            Self::Tavily => TAVILY_KIND,
        }
    }

    pub(super) const fn name(self) -> &'static str {
        match self {
            Self::Searxng => "SearXNG",
            Self::Brave => "Brave",
            Self::Tavily => "Tavily",
        }
    }

    pub(super) const fn default_base_url(self) -> Option<&'static str> {
        match self {
            Self::Searxng => None,
            Self::Brave => Some("https://api.search.brave.com/res/v1"),
            Self::Tavily => Some("https://api.tavily.com"),
        }
    }

    pub(super) const fn max_results(self) -> usize {
        match self {
            Self::Searxng => 50,
            Self::Brave | Self::Tavily => 20,
        }
    }

    pub(super) fn request(
        self,
        client: &Client,
        base_url: &str,
        key: Option<&str>,
        query: &str,
        limit: usize,
        language: Option<&str>,
    ) -> RequestBuilder {
        let base = base_url.trim_end_matches('/');
        let language = language.map(str::trim).filter(|value| !value.is_empty());
        let request = match self {
            Self::Searxng => {
                let request = client.get(format!("{base}/search")).query(&[
                    ("q", query),
                    ("format", "json"),
                    ("language", language.unwrap_or("all")),
                ]);
                if let Some(key) = key {
                    request.bearer_auth(key)
                } else {
                    request
                }
            }
            Self::Brave => {
                let mut request = client
                    .get(format!("{base}/web/search"))
                    .header("X-Subscription-Token", key.unwrap_or_default())
                    .query(&[("q", query), ("count", &limit.to_string())]);
                if let Some(language) = language {
                    request = request.query(&[("search_lang", language)]);
                }
                request
            }
            Self::Tavily => {
                let mut body = json!({
                    "query": query, "max_results": limit, "topic": "general",
                    "search_depth": "basic", "auto_parameters": false,
                    "include_answer": false, "include_raw_content": false, "include_images": false,
                });
                if let Some(language) = language {
                    body["language"] = json!(language);
                }
                client
                    .post(format!("{base}/search"))
                    .bearer_auth(key.unwrap_or_default())
                    .json(&body)
            }
        };
        request.header(reqwest::header::ACCEPT, "application/json")
    }

    pub(super) async fn search(
        self,
        request: RequestBuilder,
        limit: usize,
    ) -> Result<Vec<SearchResult>, HarnessError> {
        let mut response = request.send().await.map_err(|error| {
            let reason = if error.is_timeout() {
                "timed out"
            } else if error.is_connect() {
                "could not connect"
            } else {
                "request failed"
            };
            HarnessError::execution(format!("{} search {reason}", self.name()))
        })?;
        if !response.status().is_success() {
            return Err(HarnessError::execution(format!(
                "{} search returned HTTP {}",
                self.name(),
                response.status().as_u16()
            )));
        }
        if response
            .content_length()
            .is_some_and(|size| size > MAX_RESPONSE_BYTES as u64)
        {
            return Err(self.response_too_large());
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| {
            HarnessError::execution(format!("{} search response could not be read", self.name()))
        })? {
            if chunk.len() > MAX_RESPONSE_BYTES.saturating_sub(bytes.len()) {
                return Err(self.response_too_large());
            }
            bytes.extend_from_slice(&chunk);
        }
        self.parse(&bytes, limit).map_err(|_| {
            HarnessError::execution(format!(
                "{} returned an invalid search response",
                self.name()
            ))
        })
    }

    fn response_too_large(self) -> HarnessError {
        HarnessError::execution(format!(
            "{} search response exceeds {MAX_RESPONSE_BYTES} bytes",
            self.name()
        ))
    }

    fn parse(self, bytes: &[u8], limit: usize) -> Result<Vec<SearchResult>, serde_json::Error> {
        let results = match self {
            Self::Brave => {
                serde_json::from_slice::<BraveResponse>(bytes)?
                    .web
                    .unwrap_or_default()
                    .results
            }
            Self::Searxng | Self::Tavily => {
                serde_json::from_slice::<SearchResponse>(bytes)?.results
            }
        };
        Ok(results
            .into_iter()
            .take(limit)
            .map(|result| SearchResult {
                title: result.title,
                url: result.url,
                snippet: match self {
                    Self::Brave => result.description,
                    _ => result.content,
                },
                engine: if self == Self::Searxng {
                    result.engine
                } else {
                    self.name().to_owned()
                },
            })
            .collect())
    }
}

#[derive(Deserialize)]
struct BraveResponse {
    web: Option<SearchResponse>,
    #[serde(rename = "type")]
    _response_type: String,
}

#[derive(Default, Deserialize)]
struct SearchResponse {
    results: Vec<ProviderResult>,
}

#[derive(Deserialize)]
struct ProviderResult {
    #[serde(default)]
    title: String,
    url: String,
    #[serde(default)]
    content: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    engine: String,
}
