use std::{error::Error as _, future::Future, io, net::IpAddr, pin::Pin, sync::Arc};

use linorun_core::{Activation, CleanupError, ComponentContext, ComponentDescriptor, effect};
use linorun_macros::component_descriptor;
use serde::Deserialize;
use serde_json::{Value, json};
use ternilo_kernel::{
    HarnessPlugin, PluginFactory, PluginManifest, ToolExecutionContext, ToolHandler,
    ToolRegistration, Tools,
};
use ternilo_protocol::{HarnessError, ToolOutput, ToolSpec};

use crate::{factory as make_factory, parse_config};

mod dns;

use dns::WebDnsResolver;

pub const KIND: &str = "ternilo.tool.web_fetch";

component_descriptor! {
    static DESCRIPTOR: () {
        id: "ternilo/builtin-web-fetch@1",
        requires: [Tools],
        provides: [],
    }
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct WebFetchConfig {
    #[serde(default = "default_timeout")]
    timeout_ms: u64,
    #[serde(default = "default_max_bytes")]
    max_bytes: u64,
    #[serde(default)]
    allow_private_network: bool,
    /// Optional HTTPS endpoint supporting the Google/Cloudflare DNS JSON API. Empty uses system DNS.
    #[serde(default)]
    dns_json_url: Option<String>,
}

const fn default_timeout() -> u64 {
    30_000
}

const fn default_max_bytes() -> u64 {
    2 * 1024 * 1024
}

pub fn factory() -> PluginFactory {
    make_factory(
        PluginManifest {
            kind: KIND,
            requires: &["ternilo/tools@1"],
            provides: &[],
        },
        |value| {
            let config: WebFetchConfig = parse_config(value)?;
            if config.timeout_ms == 0 || config.max_bytes == 0 {
                return Err(HarnessError::composition(
                    "web fetch timeout_ms and max_bytes must be positive",
                ));
            }
            let timeout = std::time::Duration::from_millis(config.timeout_ms);
            let client = reqwest::Client::builder()
                .timeout(timeout)
                .user_agent("Ternilo/0.1")
                .no_proxy()
                .redirect(redirect_policy(config.allow_private_network))
                .dns_resolver(WebDnsResolver::new(
                    config.dns_json_url.as_deref(),
                    timeout,
                    config.allow_private_network,
                )?);
            let client = client.build().map_err(|error| {
                HarnessError::composition(format!("build web fetch client: {error}"))
            })?;
            Ok(Arc::new(WebFetchPlugin { config, client }))
        },
    )
    .with_config_schema::<WebFetchConfig>()
}

struct WebFetchPlugin {
    config: WebFetchConfig,
    client: reqwest::Client,
}

impl HarnessPlugin for WebFetchPlugin {
    fn descriptor(&self) -> &'static ComponentDescriptor {
        &DESCRIPTOR
    }

    fn activate(&self, context: ComponentContext) -> Activation {
        let tools = context
            .context()
            .service::<Tools>()
            .expect("web fetch declares Tools");
        let handler = Arc::new(WebFetchTool {
            client: self.client.clone(),
            max_bytes: self.config.max_bytes,
            allow_private_network: self.config.allow_private_network,
        });
        Activation::Once(Box::pin(async move {
            let registration = tools
                .register_tool(ToolRegistration {
                    spec: ToolSpec {
                        name: "web_fetch".to_owned(),
                        description: "Fetch a public HTTP(S) URL and return its status, content type, and text body.".to_owned(),
                        input_schema: json!({
                            "type": "object",
                            "properties": { "url": { "type": "string" } },
                            "required": ["url"],
                            "additionalProperties": false
                        }),
                    },
                    effect: ternilo_kernel::ToolEffect::ReadOnly,
                    handler,
                })
                .await
                .map_err(|error| linorun_core::ActivationFailure::user(error.to_string()))?;
            Ok(Some(effect::inverse(move || async move {
                tools
                    .unregister_tool(registration)
                    .await
                    .map_err(|error| CleanupError::user(error.to_string()))
            })))
        }))
    }
}

struct WebFetchTool {
    client: reqwest::Client,
    max_bytes: u64,
    allow_private_network: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FetchArguments {
    url: String,
}

impl ToolHandler for WebFetchTool {
    fn execute<'a>(
        &'a self,
        _: ToolExecutionContext,
        arguments: Value,
    ) -> Pin<Box<dyn Future<Output = Result<ToolOutput, HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            let arguments: FetchArguments = serde_json::from_value(arguments).map_err(|error| {
                HarnessError::invalid(format!("invalid web_fetch arguments: {error}"))
            })?;
            let url = reqwest::Url::parse(&arguments.url)
                .map_err(|error| HarnessError::invalid(format!("invalid URL: {error}")))?;
            if !matches!(url.scheme(), "http" | "https") {
                return Err(HarnessError::policy("web_fetch only permits HTTP(S) URLs"));
            }
            if !self.allow_private_network && is_private_host(&url) {
                return Err(HarnessError::policy(
                    "web_fetch private-network targets are disabled by this plugin config",
                ));
            }
            let mut response = self.client.get(url).send().await.map_err(|error| {
                HarnessError::execution(format!(
                    "web fetch request failed: {}",
                    request_error(error)
                ))
            })?;
            let status = response.status();
            let content_type = response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .unwrap_or("application/octet-stream")
                .to_owned();
            if response
                .content_length()
                .is_some_and(|length| length > self.max_bytes)
            {
                return Err(HarnessError::policy(format!(
                    "web response exceeds {} bytes",
                    self.max_bytes
                )));
            }
            let bytes = read_limited_body(&mut response, self.max_bytes).await?;
            Ok(ToolOutput {
                content: format!(
                    "HTTP {}\nContent-Type: {}\n\n{}",
                    status.as_u16(),
                    content_type,
                    String::from_utf8_lossy(&bytes)
                ),
                is_error: !status.is_success(),
            })
        })
    }
}

fn request_error(error: reqwest::Error) -> String {
    let error = error.without_url();
    let mut message = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        message.push_str(": ");
        message.push_str(&cause.to_string());
        source = cause.source();
    }
    message
}

fn is_private_host(url: &reqwest::Url) -> bool {
    let Some(host) = url.host_str() else {
        return true;
    };
    let host = host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(host);
    if host.eq_ignore_ascii_case("localhost") || host.ends_with(".localhost") {
        return true;
    }
    host.parse::<IpAddr>()
        .is_ok_and(|address| !is_public_ip(address))
}

fn redirect_policy(allow_private_network: bool) -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(move |attempt| {
        if attempt.previous().len() > 5 {
            return attempt.error(io::Error::other("web_fetch exceeded five redirects"));
        }
        if !matches!(attempt.url().scheme(), "http" | "https") {
            return attempt.error(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "web_fetch redirect changed to a non-HTTP(S) URL",
            ));
        }
        if !allow_private_network && is_private_host(attempt.url()) {
            return attempt.error(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "web_fetch redirect targets a private network",
            ));
        }
        attempt.follow()
    })
}

fn is_public_ip(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => {
            let [first, second, third, _] = address.octets();
            !(first == 0
                || address.is_private()
                || address.is_loopback()
                || address.is_link_local()
                || address.is_broadcast()
                || address.is_unspecified()
                || address.is_documentation()
                || address.is_multicast()
                || (first == 100 && (64..=127).contains(&second))
                || (first == 192 && second == 0 && third == 0)
                || (first == 192 && second == 88 && third == 99)
                || (first == 198 && matches!(second, 18 | 19))
                || first >= 240)
        }
        IpAddr::V6(address) => {
            let [first, second, third, fourth, fifth, _, _, _] = address.segments();
            !(address.is_loopback()
                || address.is_unspecified()
                || address.is_unique_local()
                || address.is_unicast_link_local()
                || address.is_multicast()
                || (first == 0 && second == 0 && third == 0 && fourth == 0 && fifth == 0)
                || (first == 0x64 && second == 0xff9b && matches!(third, 0 | 1))
                || (first == 0x100 && second == 0 && third == 0 && fourth == 0)
                || (first == 0x2001 && second < 0x200)
                || (first == 0x2001 && second == 0xdb8)
                || first == 0x2002
                || (first == 0x3fff && second < 0x1000)
                || first == 0x5f00
                || first & 0xffc0 == 0xfec0)
        }
    }
}

async fn read_limited_body(
    response: &mut reqwest::Response,
    max_bytes: u64,
) -> Result<Vec<u8>, HarnessError> {
    let max_bytes = usize::try_from(max_bytes).unwrap_or(usize::MAX);
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| HarnessError::execution(format!("read web response: {error}")))?
    {
        if chunk.len() > max_bytes.saturating_sub(body.len()) {
            return Err(HarnessError::policy(format!(
                "web response exceeds {max_bytes} bytes"
            )));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::dns::{Name, Resolve, Resolving};

    #[test]
    fn private_network_filter_covers_literal_and_special_ranges() {
        for url in [
            "http://localhost/",
            "http://service.localhost/",
            "http://127.0.0.1/",
            "http://10.0.0.1/",
            "http://100.64.0.1/",
            "http://169.254.169.254/",
            "http://192.0.2.1/",
            "http://198.18.0.1/",
            "http://[::1]/",
            "http://[fc00::1]/",
            "http://[fe80::1]/",
            "http://[::ffff:127.0.0.1]/",
            "http://[64:ff9b::7f00:1]/",
        ] {
            assert!(is_private_host(&reqwest::Url::parse(url).unwrap()), "{url}");
        }
    }

    #[test]
    fn private_network_filter_allows_public_targets() {
        for url in [
            "https://example.com/",
            "https://1.1.1.1/",
            "https://[2606:4700:4700::1111]/",
        ] {
            assert!(
                !is_private_host(&reqwest::Url::parse(url).unwrap()),
                "{url}"
            );
        }
    }

    #[tokio::test]
    async fn resolver_rejects_names_with_only_private_addresses() {
        let result = WebDnsResolver::new(None, std::time::Duration::from_secs(1), false)
            .unwrap()
            .resolve("localhost".parse::<Name>().unwrap())
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn request_errors_explain_dns_failures_without_exposing_url_credentials() {
        #[derive(Debug)]
        struct FailedDnsResolver;

        impl Resolve for FailedDnsResolver {
            fn resolve(&self, _: Name) -> Resolving {
                Box::pin(async {
                    Err(Box::new(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "DNS returned no public address: proxy Fake-IP range",
                    ))
                        as Box<dyn std::error::Error + Send + Sync>)
                })
            }
        }

        let error = reqwest::Client::builder()
            .no_proxy()
            .dns_resolver(FailedDnsResolver)
            .build()
            .unwrap()
            .get("https://fixture-user:fixture-password@example.invalid/?token=fixture-token")
            .send()
            .await
            .unwrap_err();
        let message = request_error(error);
        assert!(message.contains("DNS returned no public address: proxy Fake-IP range"));
        assert!(!message.contains("fixture-user"));
        assert!(!message.contains("fixture-password"));
        assert!(!message.contains("fixture-token"));
    }

    #[tokio::test]
    async fn body_reader_enforces_limit_without_content_length() {
        use tokio::io::AsyncWriteExt as _;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\ncontent-type: text/plain\r\nconnection: close\r\n\r\n12345678",
                )
                .await
                .unwrap();
        });
        let mut response = reqwest::Client::builder()
            .no_proxy()
            .build()
            .unwrap()
            .get(format!("http://{address}/"))
            .send()
            .await
            .unwrap();
        let error = read_limited_body(&mut response, 4).await.unwrap_err();
        assert_eq!(error.code, ternilo_protocol::ErrorCode::PolicyDenied);
        server.await.unwrap();
    }
}
