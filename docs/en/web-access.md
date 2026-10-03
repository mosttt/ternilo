# Web access and proxy DNS

[简体中文](../zh-CN/web-access.md)

`web_fetch` reads public pages and needs no separate model Provider or Web API key. Enable the web-fetch plugin on a network with working public DNS. `web_search` supports SearXNG, Brave Search and Tavily with a separately configured search service.

These local tools run on the execution computer or Worker. Claude also supports separately configured provider-hosted web tools below.

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

## Claude hosted web tools

In the Claude Provider editor, open Custom settings → Claude hosted web tools. Explicitly enable search and/or fetch and configure maximum calls per tool per request, fetched content tokens and domain filtering. Tools are disabled by default. Domains use bare names and optional paths, such as `example.com/docs`; choose an allow list or a block list.

```json
{
  "hosted_tools": {
    "web_search": true,
    "web_fetch": true,
    "max_uses": 2,
    "max_content_tokens": 20000,
    "allowed_domains": ["example.com"],
    "blocked_domains": []
  }
}
```

This Provider field is supported only for `anthropic-messages`, using the official basic `web_search_20250305` and `web_fetch_20250910` tools. The model provider executes them; same-named local web tools are excluded from the model request. File and command tools still run on the execution computer, including when model requests travel through Server or another computer. Title generation and context compaction do not enable hosted web tools.

Provider charges apply, and platform token budgets do not cover additional search fees. Server account/platform models conservatively reserve tokens using configured tool call limits, model context and output bounds, then settles actual reported usage. Insufficient budgets fail before an upstream call. Provider tool changes apply to later requests; existing source authorization, budget and stop controls remain in effect.

For `pause_turn`, Ternilo retains the original assistant blocks and continues the same task, counting each continuation as another Agent step and model request. Encrypted search results, fetched content and citations survive persistence. Web sources below the answer link to original pages and remain after refresh. Users retain cancellation control.

Official contracts: [Claude web search](https://platform.claude.com/docs/en/agents-and-tools/tool-use/web-search-tool), [Claude web fetch](https://platform.claude.com/docs/en/agents-and-tools/tool-use/web-fetch-tool). The upstream account and model must support these capabilities; unsupported models return an upstream error.

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
