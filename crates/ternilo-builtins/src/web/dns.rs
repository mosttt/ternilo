use std::{
    io,
    net::{IpAddr, SocketAddr},
    time::Duration,
};

use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use serde::Deserialize;
use ternilo_protocol::HarnessError;

use super::{is_public_ip, read_limited_body, request_error};

type DnsError = Box<dyn std::error::Error + Send + Sync>;

#[derive(Clone, Debug)]
pub(super) struct WebDnsResolver {
    dns_json: Option<DnsJsonResolver>,
    allow_private_network: bool,
}

impl WebDnsResolver {
    pub(super) fn new(
        endpoint: Option<&str>,
        timeout: Duration,
        allow_private_network: bool,
    ) -> Result<Self, HarnessError> {
        let dns_json = endpoint
            .map(str::trim)
            .filter(|endpoint| !endpoint.is_empty())
            .map(|endpoint| {
                let endpoint = reqwest::Url::parse(endpoint).map_err(|_| {
                    HarnessError::composition("web fetch dns_json_url must be an HTTPS URL")
                })?;
                if endpoint.scheme() != "https"
                    || !endpoint.username().is_empty()
                    || endpoint.password().is_some()
                {
                    return Err(HarnessError::composition(
                        "web fetch dns_json_url must be an HTTPS URL without embedded credentials",
                    ));
                }
                let client = reqwest::Client::builder()
                    .timeout(timeout)
                    .user_agent("Ternilo/0.1")
                    .no_proxy()
                    .redirect(reqwest::redirect::Policy::none())
                    .build()
                    .map_err(|error| {
                        HarnessError::composition(format!(
                            "build web DNS client: {}",
                            request_error(error)
                        ))
                    })?;
                Ok(DnsJsonResolver { endpoint, client })
            })
            .transpose()?;
        Ok(Self {
            dns_json,
            allow_private_network,
        })
    }
}

impl Resolve for WebDnsResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let host = name.as_str().to_owned();
        let dns_json = self.dns_json.clone();
        let allow_private_network = self.allow_private_network;
        Box::pin(async move {
            let resolved = if let Some(resolver) = dns_json {
                let (ipv4, ipv6) =
                    tokio::try_join!(resolver.query(&host, "A"), resolver.query(&host, "AAAA"))?;
                ipv4.into_iter().chain(ipv6).collect()
            } else {
                tokio::net::lookup_host((host.as_str(), 0)).await?.collect()
            };
            let addresses = filter_addresses(&host, resolved, allow_private_network)?;
            Ok(Box::new(addresses.into_iter()) as Addrs)
        })
    }
}

#[derive(Clone, Debug)]
struct DnsJsonResolver {
    endpoint: reqwest::Url,
    client: reqwest::Client,
}

impl DnsJsonResolver {
    async fn query(&self, host: &str, record_type: &str) -> Result<Vec<SocketAddr>, DnsError> {
        let mut response = self
            .client
            .get(self.endpoint.clone())
            .query(&[("name", host), ("type", record_type)])
            .header(reqwest::header::ACCEPT, "application/dns-json")
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(|error| {
                io::Error::other(format!(
                    "DNS JSON HTTPS request failed: {}",
                    request_error(error)
                ))
            })?;
        let body = read_limited_body(&mut response, 64 * 1024).await?;
        parse_answers(&body)
    }
}

#[derive(Deserialize)]
struct DnsJsonResponse {
    #[serde(rename = "Status")]
    status: u16,
    #[serde(rename = "Answer", default)]
    answers: Vec<DnsJsonAnswer>,
}

#[derive(Deserialize)]
struct DnsJsonAnswer {
    #[serde(rename = "type")]
    record_type: u16,
    data: String,
}

fn parse_answers(body: &[u8]) -> Result<Vec<SocketAddr>, DnsError> {
    let response: DnsJsonResponse = serde_json::from_slice(body).map_err(|_| {
        io::Error::other("DNS endpoint did not return a valid Google/Cloudflare DNS JSON response")
    })?;
    if response.status != 0 {
        return Err(io::Error::other(format!(
            "DNS JSON resolver returned DNS status {}",
            response.status
        ))
        .into());
    }
    Ok(response
        .answers
        .into_iter()
        .filter_map(
            |answer| match (answer.record_type, answer.data.parse::<IpAddr>().ok()) {
                (1, Some(IpAddr::V4(address))) => Some(SocketAddr::new(IpAddr::V4(address), 0)),
                (28, Some(IpAddr::V6(address))) => Some(SocketAddr::new(IpAddr::V6(address), 0)),
                _ => None,
            },
        )
        .collect())
}

fn filter_addresses(
    host: &str,
    resolved: Vec<SocketAddr>,
    allow_private_network: bool,
) -> Result<Vec<SocketAddr>, io::Error> {
    let mut fake_ip = false;
    let addresses = resolved
        .into_iter()
        .filter(|address| {
            if let IpAddr::V4(address) = address.ip() {
                let [first, second, _, _] = address.octets();
                fake_ip |= first == 198 && matches!(second, 18 | 19);
            }
            allow_private_network || is_public_ip(address.ip())
        })
        .collect::<Vec<_>>();
    if addresses.is_empty() {
        let detail = if fake_ip {
            "the proxy Fake-IP range 198.18.0.0/15 is not a public destination; configure dns_json_url with a trusted HTTPS DNS JSON endpoint, or make the proxy DNS return real public addresses"
        } else {
            "private-network targets are disabled or the name has no address"
        };
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("DNS for {host:?} returned no usable public address: {detail}"),
        ));
    }
    Ok(addresses)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dns_json_keeps_public_ipv4_and_ipv6_but_rejects_private_and_fake_answers() {
        let answers = parse_answers(
            br#"{"Status":0,"Answer":[
            {"type":5,"data":"alias.example"},
            {"type":1,"data":"1.1.1.1"},
            {"type":28,"data":"2606:4700:4700::1111"},
            {"type":1,"data":"127.0.0.1"},
            {"type":1,"data":"169.254.169.254"},
            {"type":1,"data":"198.18.0.1"},
            {"type":28,"data":"::ffff:127.0.0.1"}
        ]}"#,
        )
        .unwrap();
        let addresses = filter_addresses("public.example", answers, false).unwrap();
        assert_eq!(
            addresses,
            [
                "1.1.1.1:0".parse().unwrap(),
                "[2606:4700:4700::1111]:0".parse().unwrap()
            ]
        );
        let error = filter_addresses("fake.example", vec!["198.18.0.1:0".parse().unwrap()], false)
            .unwrap_err();
        assert!(error.to_string().contains("proxy Fake-IP"));
        assert!(error.to_string().contains("dns_json_url"));
    }

    #[test]
    fn dns_json_does_not_silently_fall_back_on_resolver_errors() {
        assert!(
            parse_answers(br#"{"Status":3}"#)
                .unwrap_err()
                .to_string()
                .contains("DNS status 3")
        );
        assert!(parse_answers(b"not DNS JSON").is_err());
        let answers = parse_answers(br#"{"Status":0}"#).unwrap();
        assert!(filter_addresses("empty.example", answers, false).is_err());
    }

    #[test]
    fn dns_endpoint_is_explicit_https_configuration() {
        let timeout = Duration::from_secs(1);
        assert!(
            WebDnsResolver::new(None, timeout, false)
                .unwrap()
                .dns_json
                .is_none()
        );
        assert!(
            WebDnsResolver::new(Some(" "), timeout, false)
                .unwrap()
                .dns_json
                .is_none()
        );
        assert!(WebDnsResolver::new(Some("http://dns.example/resolve"), timeout, false).is_err());
        assert!(
            WebDnsResolver::new(
                Some("https://user:password@dns.example/resolve"),
                timeout,
                false
            )
            .is_err()
        );
        assert!(
            WebDnsResolver::new(Some("https://dns.example/resolve"), timeout, false)
                .unwrap()
                .dns_json
                .is_some()
        );
    }
}
