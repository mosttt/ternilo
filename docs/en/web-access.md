# Web access and proxy DNS

[简体中文](../zh-CN/web-access.md)

`web_fetch` reads public pages and needs no separate model Provider or Web API key. Enable the web-fetch plugin on a network with working public DNS. `web_search` uses SearXNG and requires a separately configured search service.

Both tools run on the computer or Worker executing the task. Vendor-hosted native search tools are not currently adapted; configuring a compatible model endpoint does not enable the vendor's cloud browsing features.

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
