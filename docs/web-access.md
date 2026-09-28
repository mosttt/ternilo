# 网页访问与代理 DNS

`web_fetch` 用于读取公开网页，不需要单独的模型 Provider 或网页 API Key。在能够正常解析公网地址的网络中，启用 `web-fetch` 插件即可使用。搜索网页的 `web_search` 使用 SearXNG，是另一项需要配置搜索服务的能力。

这两项工具由执行任务的 Ternilo 运行。模型服务商在云端执行的原生联网搜索目前尚未适配；配置一个兼容的模型接口，并不意味着自动启用服务商的联网工具。

## 使用透明代理时无法读取网页

部分代理的 Fake-IP 模式会把公开域名解析为 `198.18.0.0/15` 内的地址。这些地址不是真实公网地址，`web_fetch` 默认拒绝连接。因此可能出现浏览器能访问网页，而工具很快报错的情况。

工具会在错误详情中说明被拒绝的 DNS 结果和 Fake-IP 原因。这与模型是否配置成功无关，也不应通过关闭 TLS 校验或开启所有私网访问来解决。

可以让代理对所需域名返回真实公网地址，或者为 `web-fetch` 配置一个你信任的 HTTPS DNS JSON 服务：

1. 打开“设置 → 插件 → 插件配置”，展开 `web-fetch`。
2. 在 `Dns json url` 字段填入 HTTPS DNS JSON 接口地址，保存配置。字段对应 `dns_json_url`。
3. 再次发起网页访问。该配置只影响所选配置范围，不会修改操作系统或代理设置。

例如，Cloudflare 提供的接口为 `https://cloudflare-dns.com/dns-query`，Google 的接口为 `https://dns.google/resolve`。也可以使用兼容相同 JSON 格式的自建服务。所选服务必须从执行任务的电脑或 Worker 网络可达；它会收到待访问网页的域名查询。

```json
{
  "dns_json_url": "https://cloudflare-dns.com/dns-query"
}
```

这是可选设置。留空继续使用系统 DNS，Ternilo 不会自动选用第三方 DNS。接口必须支持 Google／Cloudflare 的 DNS JSON 请求和响应，不能填只接受 DNS 二进制报文的普通 DoH 地址。JSON 格式说明见 [Cloudflare 文档](https://developers.cloudflare.com/1.1.1.1/encryption/dns-over-https/make-api-requests/dns-json/)。

## 访问边界

指定的 DNS 服务只负责解析域名。Ternilo 仍会检查返回的 IPv4／IPv6 地址，只连接通过检查的公网地址，并在网页跳转时再次检查目标。DNS 失败时不会静默换到其他解析服务。

DNS 服务和网页访问都保留正常 TLS 证书校验、超时与响应大小限制。DNS 接口由用户或管理员在插件设置中配置，模型不能通过 `web_fetch` 参数指定解析服务。

`allow_private_network` 用于明确需要访问内网服务的场景，不是 Fake-IP 的修复开关。普通公开网页访问保持关闭。
