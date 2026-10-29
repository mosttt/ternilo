# 网页访问与代理 DNS

默认 `web-fetch` 插件直接读取公开网页，不需要网页 API Key；也可选择 DeepSeek 抓取服务获取 Markdown 正文。搜索网页的 `web_search` 支持 SearXNG、Brave Search、Tavily 和 DeepSeek，需要单独配置搜索服务。

这两项本机工具由执行任务的 Ternilo 运行。Claude 和 DeepSeek Responses 还可使用下面单独配置的供应商托管网页工具。

## 让网页工具跟随 Provider

希望换电脑后无需重新配置搜索时，在“我的模型 → 账号自有”编辑 Provider，打开“自定义设置 → 网页访问”。独立使用 Ternilo 时，入口是“设置 → 模型”。

分别选择网页搜索和网页读取：使用会话插件、关闭、电脑执行的服务，或当前协议支持的模型内置工具。电脑搜索支持 DeepSeek、Brave、Tavily、SearXNG；读取可以直接访问网页或使用 DeepSeek 返回 Markdown。选择 DeepSeek 搜索与读取时，可勾选“使用搜索凭据”，只填写一次网页 API Key。服务地址和限制放在高级设置；网页地址不自动沿用模型代理地址。

选择 Provider 后，会话自动采用它的网页设置；个人新会话沿用已有默认模型选择，不另设网页默认值。切换 Provider 会更换对应网页工具。没有配置 `web_access` 或选择“使用会话插件”时，独立插件继续按原配置运行。明确禁用的会话／预设工具不会被 Provider 重新开启；每种能力只提供一个实现，不自动换来源或凭据。

账号只保存一份配置和独立网页凭据。Server 远程电脑会话按当前绑定取得配置；模型来自另一台电脑时，网页工具仍在任务电脑运行。网页 Key 只按需交给同一所有者的已授权执行电脑，不写入该电脑的凭据库、会话历史或插件配置。共享模型的调用权不包含向他人电脑导出网页凭据的权利。模型 Key 不自动用于网页请求。

后续对话任务读取更新后的配置。每次调用前核验配置和权限；已发出的请求保持原配置，后续调用遇到配置或凭据失效时明确失败。配置变化后请重新发起任务。集中配置与单插件视图显示 Provider 管理的生效值，修改入口仍是该 Provider 的“网页访问”。

电脑执行模式适用于本机 Provider、Server 账号 Provider 和同一账号的跨电脑 Provider。平台 Provider 不导出搜索凭据；托管 Worker 会话需要改用独立会话插件或模型内置工具。仅授权模型的独立 Server 连接不附带电脑网页凭据。模型内置搜索始终由上游服务商执行，不在任务电脑或 Ternilo Server 执行。

下面介绍独立插件配置。需要它们独立于所选 Provider 时，保持“使用会话插件”。

## 选择搜索服务

在自定义 Agent 预设中添加一个搜索插件。这些插件都注册 `web_search`，同一预设只启用其中一个；不会因某个服务失败而自动切换供应商。

| 插件 kind | 默认 base_url | 凭据引用 | max_results 上限 |
|---|---|---|---|
| `ternilo.web.search.searxng` | 必填，自建 SearXNG 地址 | 可选，Bearer 鉴权 | 50 |
| `ternilo.web.search.brave` | `https://api.search.brave.com/res/v1` | 必填，Brave API Key | 20 |
| `ternilo.web.search.tavily` | `https://api.tavily.com` | 必填，Tavily API Key | 20 |
| `ternilo.web.search.deepseek` | `https://api.deepseek.com` | 必填，DeepSeek API Key | 50 |

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

## DeepSeek 搜索与 Markdown 抓取

DeepSeek 搜索插件使用独立 Search API，不要求对话模型来自 DeepSeek。在上面的插件行中将 kind 改为 `ternilo.web.search.deepseek`，并填写执行电脑上的 DeepSeek 凭据名称即可。`query` 支持 `site:`、`-site:`、`lang:`、`after:`、`before:` 等官方搜索语法；可选 `language` 会追加为 `lang:` 筛选，最终查询不能超过 128 个 Unicode 字符。返回结果优先使用适合模型阅读的长摘要，不附带整页正文。

需要网页正文时，停用原来的 `web-fetch`，添加以下插件；同一预设只启用一种 `web_fetch` 实现：

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

模型仍调用 `web_fetch({"url":"https://example.com/article"})`，收到页面标题、原始 URL 和 Markdown 正文。查询和目标网址会发送给 DeepSeek，不会由执行电脑直接抓取目标网页；目标网页能否读取取决于 DeepSeek 的访问与安全策略。插件配置可在集中配置和单插件配置之间同步修改。

两项服务使用同一种 DeepSeek API Key；可在 Provider 的“网页访问”中保存，也可由独立插件引用执行电脑的凭据，按成功且有结果的请求计费。没有结果、失败或超时的请求不计费；当前官方价格与限速见 [DeepSeek 搜索与抓取](https://api-docs.deepseek.com/zh-cn/guides/search/)。HTTP 402 需要为该凭据所属账号充值；401 检查凭据；403／404 检查或更换目标网页；429 降低调用频率。错误不回显上游正文或密钥，取消会关闭当前请求。

## DeepSeek Responses 内置搜索

在模型提供方中选择 **DeepSeek Responses** 协议，添加 `deepseek-v4-pro`，再将“自定义设置 → 网页访问 → 网页搜索”设为“模型内置 · 服务商执行”。默认不启用；只有 Flash 的提供方不展示此选项。显式选择内置搜索后，使用不支持的模型会提示更换模型或改用电脑搜索，不自动降级。

Provider 配置为：

```json
{
  "protocol": "deepseek-responses",
  "web_access": { "search": { "mode": "hosted" } }
}
```

内置搜索由 DeepSeek 在模型请求内部执行，使用模型提供方的凭据；本机的同名 `web_search` 不会重复提交。本机 `web_fetch`、文件和命令工具继续独立执行。普通工具执行后，下一次模型请求会带回保存的搜索调用与回答；引用显示在答案下的“网页来源”，刷新后保留。切换协议或模型时不会把专属搜索状态带到不同模型。

本机 Provider、Server 账号／平台模型、电脑转发模型及“连接 Server”的模型服务均使用同一配置。仅模型提供方所有者能够启用；外部客户端不能通过添加 `web_search` 定义替共享模型开启搜索。标题生成和上下文压缩不启用内置搜索。

DeepSeek 当前不支持内置搜索调用次数上限、域名筛选或内置 Fetch 配置，界面不展示这些无效选项。Server 按完整上下文与最大输出预留 Token，并按上游报告结算；这是预留估算，不能硬性限制上游内部多次生成或额外搜索费用。搜索计费由 DeepSeek 账号承担，Ternilo 的 Token 额度不是费用上限。需要自己控制每次搜索与抓取时，使用前面的独立插件。

能力与限制依据 [DeepSeek Responses 文档](https://api-docs.deepseek.com/zh-cn/guides/responses_api/)。当前官方仅为 `deepseek-v4-pro` 提供 `web_search`；该配置不会为标准 OpenAI Responses 或 Flash 启用搜索。

## Claude 托管网页工具

在 Claude Provider 的“自定义设置 → 网页访问”中，将搜索或读取设为“模型内置 · 服务商执行”，再设置每次请求每种工具的调用上限、读取正文的 Token 上限及域名筛选。默认不启用。域名列表使用裸域名和可选路径，例如 `example.com/docs`；允许列表与阻止列表二选一。

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

这是 Provider 对象中的 Claude 工具配置，适用于 `anthropic-messages`。采用官方基本工具 `web_search_20250305` 和 `web_fetch_20250910`；执行地点是模型服务商，同名的本机网页工具不会同时提交到模型。其他本机文件和命令工具继续在执行电脑运行，模型请求通过 Server 或另一台电脑转发时也保留这个分工。标题生成和上下文压缩不启用托管网页工具。

搜索和读取由供应商计费，平台 Token 预算不代表搜索费用额度。Server 的账号／平台模型按配置的最大工具次数、模型上下文和输出上限保守预留 Token，完成后按实际报告结算；额度不足会在调用前拒绝。修改 Provider 的工具设置影响后续请求，预算、授权和停止仍遵守原模型来源。

长请求返回 `pause_turn` 时，Ternilo 保留原始助手内容并继续同一轮任务，每次续传计入 Agent 步数与模型请求。搜索密文、读取结果和引用完整保存；答案下的“网页来源”可打开原始页面，刷新后仍保留。取消仍由执行任务的用户控制。

协议依据：[Claude 网页搜索](https://platform.claude.com/docs/en/agents-and-tools/tool-use/web-search-tool)、[Claude 网页读取](https://platform.claude.com/docs/en/agents-and-tools/tool-use/web-fetch-tool)。供应商需要为对应账号和模型开放该能力；不支持的模型会返回上游错误。

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
