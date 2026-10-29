# Web access and proxy DNS

[简体中文](../zh-CN/web-access.md)

The default `web-fetch` plugin reads public pages directly without a Web API key; DeepSeek fetch is an alternative that returns Markdown. `web_search` supports SearXNG, Brave Search, Tavily and DeepSeek with a separately configured search service.

These local tools run on the execution computer or Worker. Claude and DeepSeek Responses also support separately configured provider-hosted web tools below.

## Follow the selected Provider

To configure search once across your computers, edit a Provider under **My models → Account-owned → Custom settings → Web access**. Standalone Ternilo uses **Settings → Models**.

Choose search and page reading separately: keep session plugins, turn the capability off, use a computer-executed service, or select supported model-hosted tools. Computer search supports DeepSeek, Brave, Tavily and SearXNG. Reading supports direct fetch or DeepSeek Markdown. DeepSeek search and reading can share one explicitly entered web API key. Addresses and limits are under advanced settings; the model relay address is not reused for web services.

Sessions use the selected Provider’s web settings. Personal new sessions already select the default model and therefore use its Provider; there is no separate web default. Switching Providers switches their web tools. Missing `web_access` or **Use session plugins** preserves standalone plugins. Explicitly disabled preset/session tools stay disabled. Each capability has one selected implementation, without automatic source or credential fallback.

The account stores one configuration and separate web credentials. Server remote sessions retrieve settings for their bound Provider. When the model comes from another computer, web tools still execute on the task computer. Web keys are provided only to authorized execution computers owned by the credential owner, not persisted in their credential stores, session history or plugin configuration. Shared model invocation rights do not authorize exporting web credentials to somebody else’s computer. Model keys are not automatically used for web calls.

New dialogue tasks read updated settings. Configuration and authority are checked before each call. In-flight requests retain their original settings; later calls fail explicitly if settings or credentials have become invalid. Start a new task after configuration changes. All settings and individual plugin views show effective Provider-managed values; edit them in that Provider’s Web access section.

Computer mode supports local Providers, Server account Providers and same-owner computer-forwarded Providers. Platform Providers do not export web credentials. Managed Worker sessions must use independent session plugins or hosted tools instead. Model-only Server device connections do not include computer web credentials. Built-in search remains an upstream service, not code executing on the task computer or Ternilo Server.

The following sections configure independent plugins. Select **Use session plugins** when web access should remain independent of the model Provider.

## Choose a search service

Add one search plugin to a custom Agent preset. All of them register `web_search`, so enable only one per preset. A failed request does not switch providers.

| Plugin kind | Default base_url | Credential reference | Maximum max_results |
|---|---|---|---|
| `ternilo.web.search.searxng` | Required, your SearXNG instance | Optional Bearer token | 50 |
| `ternilo.web.search.brave` | `https://api.search.brave.com/res/v1` | Required Brave API key | 20 |
| `ternilo.web.search.tavily` | `https://api.tavily.com` | Required Tavily API key | 20 |
| `ternilo.web.search.deepseek` | `https://api.deepseek.com` | Required DeepSeek API key | 50 |

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

## DeepSeek search and Markdown fetch

The DeepSeek search plugin uses the standalone Search API and works independently of the conversation model. In the example above, change kind to `ternilo.web.search.deepseek` and reference a DeepSeek credential on the execution computer. The query supports official syntax such as `site:`, `-site:`, `lang:`, `after:` and `before:`. Optional `language` adds a `lang:` filter; the resulting query must fit within 128 Unicode characters. Results prefer the longer model-oriented summary and omit full page content.

To read page content, disable the existing `web-fetch` plugin and add this one. Enable only one `web_fetch` implementation per preset:

```json
{
  "id": "deepseek-fetch",
  "kind": "ternilo.web.fetch.deepseek",
  "enabled": true,
  "config": {
    "api_key_env": "SEARCH_KEY",
    "timeout_ms": 30000,
    "max_bytes": 2097152
  }
}
```

The model still calls `web_fetch({"url":"https://example.com/article"})` and receives the page title, original URL and Markdown body. Queries and target URLs are sent to DeepSeek; the execution computer does not fetch the target page directly. Availability depends on DeepSeek’s access and safety policies. Changes stay synchronized between All settings and individual plugin configuration.

Both services use a DeepSeek API key, saved in Provider Web access or referenced from the execution computer by an independent plugin. Billing is per successful request with a nonempty result; failures, timeouts and empty searches are not charged. See [DeepSeek search and fetch](https://api-docs.deepseek.com/guides/search/) for current pricing and rate limits. HTTP 402 requires topping up the credential’s account; 401 means check the credential; 403/404 means check or change the page; 429 means reduce request frequency. Errors do not echo upstream bodies or keys. Cancellation closes the active request.

## DeepSeek Responses built-in search

Choose the **DeepSeek Responses** protocol, add `deepseek-v4-pro`, then select **Custom settings → Web access → Web search → Built-in · model provider**. It is not enabled by default and is unavailable in a Flash-only Provider. An explicit built-in selection rejects unsupported models; choose a supported model or computer search instead of relying on fallback.

Provider configuration:

```json
{
  "protocol": "deepseek-responses",
  "web_access": { "search": { "mode": "hosted" } }
}
```

DeepSeek runs searches inside the model request using the model Provider’s credentials. The same-named local `web_search` is excluded, while local `web_fetch`, file and command tools remain available. After a local tool runs, the next model request replays the saved search calls and answer. Citations appear under Web sources and survive refresh. Switching model or protocol does not forward model-specific search state to a different model.

The same setting applies to local Providers, Server account/platform models, computer-forwarded models and Connect to Server model access. Only the Provider owner enables search; external clients cannot turn it on by submitting their own `web_search` definition. Title generation and context compaction do not enable built-in search.

DeepSeek does not currently support built-in search call limits, domain filters or built-in Fetch settings, so these controls are not shown. Server reserves an estimate using the full context and maximum output, then settles reported usage. This reservation cannot impose a hard limit on provider-internal generation or additional search charges. The DeepSeek account pays those charges; Ternilo token budgets are not spending caps. Use the standalone plugins when you need explicit control over each search and fetch call.

See the [DeepSeek Responses contract](https://api-docs.deepseek.com/guides/responses_api/). The official API currently supports `web_search` only for `deepseek-v4-pro`; this setting does not enable search for standard OpenAI Responses or Flash.

## Claude hosted web tools

In the Claude Provider editor, open **Custom settings → Web access** and select **Built-in · model provider** for search and/or reading. Then configure maximum calls per tool per request, fetched content tokens and domain filtering. Hosted tools are not enabled by default. Domains use bare names and optional paths, such as `example.com/docs`; choose an allow list or a block list.

```json
{
  "web_access": { "search": { "mode": "hosted" }, "fetch": { "mode": "hosted" } },
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

This Claude-specific Provider configuration uses `anthropic-messages`, using the official basic `web_search_20250305` and `web_fetch_20250910` tools. The model provider executes them; same-named local web tools are excluded from the model request. File and command tools still run on the execution computer, including when model requests travel through Server or another computer. Title generation and context compaction do not enable hosted web tools.

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
