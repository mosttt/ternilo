# 搜索服务扩展

2026-10-03，按用户优先级 2 增加 Brave Search 与 Tavily，保留 SearXNG。三种服务使用独立插件 kind，共享 `web_search` 的工具、结果和生命周期；同一预设只启用一种。

## 当前结果

- Brave 默认 `https://api.search.brave.com/res/v1`，GET `/web/search`，使用 `X-Subscription-Token`、`q`、`count`、可选 `search_lang`。
- Tavily 默认 `https://api.tavily.com`，POST `/search`，Bearer 鉴权，固定 basic、general，关闭自动参数、生成答案、原始正文及图片；支持可选语言。
- SearXNG 保留自建地址、JSON 搜索、可选 Bearer 凭据。返回数量默认为 10，上限分别为 20／20／50。
- 凭据从执行电脑的 SecretResolver 按引用读取；不保存到插件 JSON，不因模型转发而复制来源电脑的凭据。取消覆盖凭据解析、发送和响应读取。
- HTTP 重定向不跟随，响应上限 2 MiB；上游错误不回显正文、Key 或查询 URL。没有供应商降级或自动选择。
- 配置目录、插件说明和搜索结果沿用现有界面，已同步中英文正式文档。

## 验证

官方依据为 [Brave Web Search](https://api-dashboard.search.brave.com/api-reference/web/search/get) 与 [Tavily Search](https://docs.tavily.com/documentation/api-reference/endpoint/search)，已直接读取两家的官方页面和协议说明。

4 项 Rust 合同测试覆盖原生请求路径／鉴权头、查询编码、JSON 参数、结果数量、401／429／500、禁止重定向、坏目录和无 Content-Length 的超大响应。

`search-services-browser-e2e.test.mjs` 启动真实本机服务与 Chromium，使用 HTTP 合同服务完成三家模型→搜索→工具结果→最终回答，以及 401 错误、取消活动连接、390 px 页面。最终通过 9 次搜索／15 次模型请求，控制台与网络错误均为零；HTTP 测试服务代替收费账号，不宣称已用供应商真实 Key 做在线收费调用。

本阶段并不完成整个优先级 2。供应商托管工具和更完整的外部代理适配继续推进；搜索插件不会绕过托管 Worker 的凭据或网络隔离。
