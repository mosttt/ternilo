# 网页访问与代理 DNS

`web_fetch` 用于读取公开网页，不需要单独的模型 Provider 或网页 API Key。在能够正常解析公网地址的网络中，启用 `web-fetch` 插件即可使用。搜索网页的 `web_search` 支持 SearXNG、Brave Search 和 Tavily，需要单独配置搜索服务。

这两项工具由执行任务的 Ternilo 运行。模型服务商在云端执行的原生联网搜索目前尚未适配；配置一个兼容的模型接口，并不意味着自动启用服务商的联网工具。

## 选择搜索服务

在自定义 Agent 预设中添加一个搜索插件。三个插件都注册 `web_search`，同一预设只启用其中一个；不会因某个服务失败而自动切换供应商。

| 插件 kind | 默认 base_url | 凭据引用 | max_results 上限 |
|---|---|---|---|
| `ternilo.web.search.searxng` | 必填，自建 SearXNG 地址 | 可选，Bearer 鉴权 | 50 |
| `ternilo.web.search.brave` | `https://api.search.brave.com/res/v1` | 必填，Brave API Key | 20 |
| `ternilo.web.search.tavily` | `https://api.tavily.com` | 必填，Tavily API Key | 20 |

先在执行电脑的“凭据与登录”保存凭据，例如名称 `SEARCH_KEY`，再把名称填入 `api_key_env`。这个字段引用凭据或宿主环境变量，不填写 Key 原文。Server 远程会话由执行电脑调用搜索服务；跨电脑模型转发不会改变搜索工具的执行位置，也不提供搜索凭据。托管 Worker 必须另有可用的凭据和网络策略；本配置不会突破 Worker 隔离。

Brave 插件行示例：

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

Tavily 把 kind 改为 `ternilo.web.search.tavily`。SearXNG 使用自己的 kind 并填写 `base_url`。模型传入 `query`、可选的 `limit` 和 `language`；语言代码采用所选服务的规则。结果统一为标题、URL、摘要与来源。默认每次最多 10 项，模型不能超过配置的上限。

Tavily 固定使用 `basic` 搜索，关闭自动参数、生成答案、原始网页正文和图片。搜索仍由供应商按账号计费。协议依据 [Brave Web Search](https://api-dashboard.search.brave.com/api-reference/web/search/get) 和 [Tavily Search](https://docs.tavily.com/documentation/api-reference/endpoint/search)。

搜索请求可取消，默认超时 30 秒，响应最多 2 MiB。不会跟随搜索接口的 HTTP 重定向；错误只显示服务名和状态类别，不回显上游正文、查询 URL 或 Key。结果来自外部网页，是供模型参考的内容。

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
