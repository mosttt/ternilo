# Web access and proxy DNS

[简体中文](../zh-CN/web-access.md)

`web_fetch` reads public pages and needs no separate model Provider or Web API key. Enable the web-fetch plugin on a network with working public DNS. `web_search` supports SearXNG, Brave Search and Tavily with a separately configured search service.

Both tools run on the computer or Worker executing the task. Vendor-hosted native search tools are not currently adapted; configuring a compatible model endpoint does not enable the vendor's cloud browsing features.

## Choose a search service

Add one search plugin to a custom Agent preset. All three register `web_search`, so enable only one per preset. A failed request does not switch providers.

| Plugin kind | Default base_url | Credential reference | Maximum max_results |
|---|---|---|---|
| `ternilo.web.search.searxng` | Required, your SearXNG instance | Optional Bearer token | 50 |
| `ternilo.web.search.brave` | `https://api.search.brave.com/res/v1` | Required Brave API key | 20 |
| `ternilo.web.search.tavily` | `https://api.tavily.com` | Required Tavily API key | 20 |

Save a credential such as `SEARCH_KEY` under Credentials & sign-in on the execution computer, then put its name in `api_key_env`. This field references a credential or host environment variable, never the key itself. For Server remote sessions, the execution computer calls search; forwarding model requests through another computer does not move the search tool or supply search credentials. Managed Workers separately require credentials and permitted network access; this configuration does not bypass Worker isolation.

Example Brave plugin row:

```json
{
  "id": "web-search",
  "kind": "ternilo.web.search.brave",
  "enabled": true,
  "config": {
    "api_key_env": "SEARCH_KEY",
    "max_results": 10,
    "timeout_ms": 30000
  }
}
```

For Tavily use `ternilo.web.search.tavily`. For SearXNG use its kind and supply `base_url`. The model supplies `query`, optionally `limit` and `language`; language codes follow the selected service. Results share title, URL, snippet and engine fields. The default is at most 10 results, bounded by the configured maximum.

Tavily uses `basic` search with automatic parameters, generated answers, raw page content and images disabled. Provider billing still applies. See the official [Brave Web Search](https://api-dashboard.search.brave.com/api-reference/web/search/get) and [Tavily Search](https://docs.tavily.com/documentation/api-reference/endpoint/search) contracts.

Requests support cancellation, default to a 30-second timeout and cap responses at 2 MiB. Search endpoint redirects are not followed. Errors disclose service and status categories without echoing upstream bodies, query URLs or keys. Search results are external reference content.

## Fake-IP proxy networks

Some transparent proxies resolve public domains into `198.18.0.0/15`. These are synthetic addresses rather than real public destinations, so web_fetch rejects them. A browser can therefore reach a site while the agent's tool fails immediately.

The error reports the rejected DNS result and Fake-IP reason. It is unrelated to model credentials. Do not resolve it by disabling TLS validation or enabling unrestricted private-network access.

Configure the proxy to return real public addresses for the required domains, or configure a trusted HTTPS DNS JSON endpoint in Settings → Plugins → web-fetch → Dns json url. This changes the selected plugin scope, not system or proxy settings.

```json
{
  "dns_json_url": "https://cloudflare-dns.com/dns-query"
}
```

Cloudflare's endpoint above and `https://dns.google/resolve` support the required format; a compatible self-hosted service also works. It must be reachable from the executing machine and receives the domain queries. This setting is optional: empty means system DNS, and Ternilo never chooses a third-party resolver automatically. The endpoint must accept Google/Cloudflare DNS JSON, not only binary DNS-over-HTTPS messages. See the [DNS JSON format](https://developers.cloudflare.com/1.1.1.1/encryption/dns-over-https/make-api-requests/dns-json/).

## Access boundaries

The resolver supplies addresses only. Ternilo validates returned IPv4/IPv6 addresses and connects only to accepted public destinations; redirects are checked again. DNS failure does not silently switch resolver services.

DNS and page requests retain TLS certificate validation, timeouts and response-size limits. Users or administrators configure the resolver in plugin settings; a model cannot select one through web_fetch arguments.

`allow_private_network` is for explicitly permitted internal services. It is not the Fake-IP repair switch and should stay disabled for ordinary public Web access.
