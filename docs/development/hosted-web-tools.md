# 供应商托管网页工具

Provider 所有者通过 `hosted_tools` 显式启用供应商网页工具，默认关闭。Claude Messages 使用基本 `web_search_20250305` 和 `web_fetch_20250910`；DeepSeek Responses 的模型范围与独立合同见后文。以下先保留 2026-10-03 的 Claude 实现和验收。

## 行为与边界

- 本机 Provider、Server 账号／平台模型、另一台电脑的模型、仅授权模型的本地客户端共用配置。原生模型网关也在验证来源后注入工具定义。
- 同名本机网页工具不再提交到模型，其他文件和命令工具继续由执行电脑运行。模型来源电脑只调用模型，供应商执行托管网页工具。
- 暂停响应保留原始助手块后继续同一 Agent 任务，步数递增，仍检查取消和原有步数上限。服务器工具不会被派发成本机工具。
- 分片服务器工具参数、加密搜索结果、读取文档和引用均原样持久化并回放。网页来源链接经过 HTTP(S)／无 URL 用户信息检查，刷新后仍保留。
- 标题和压缩使用第 0 步文本辅助请求，Claude 的 `tool_choice: none` 禁止其调用托管工具。
- Server 账号／平台模型在接纳前按最大工具次数与模型上下文／输出限制保守预留 Token，实际完成后按报告结算。设备来源自己的 Provider 仍自行计费和报告；Token 预算不承诺覆盖供应商额外搜索费用。
- 来源电脑的工具配置变动参与原有配置撤销检查。协议增加暂停完成原因及 Provider 配置，执行器协议为 48，Server／Node／Worker 必须匹配。

## 已验证

两个 Rust 协议合同验证工具定义与同名替换、普通本机工具保留、分片输入、密文／引用保存及暂停后的逐块回放。预算合同验证小额预算在上游调用前拒绝、足够预算保守预留、实际用量结算和关闭工具后恢复普通请求预留。

`hosted-web-tools-browser-e2e.test.mjs` 使用真实 CLI／Server／两个登记电脑及一个仅授权模型的独立客户端，共 4 条路径、12 次上游请求，完成：暂停 → 托管读取 → 本机文件工具 → 最终回答。网页设置保存、链接刷新、390 px 展示、真实凭据来源、来源电脑无会话／文件工具执行及控制台／HTTP 错误均验证。接口上游是受控 Claude 合同服务，没有使用真实收费账号。

额外客户端验收发现独立原生网关仍将 `pause_turn` 当作失败，已按官方协议修正。SQLite 与受限 PostgreSQL 的四条路径均通过。前端全量检查通过 152 文件／989 项；相关 Rust 全目标 Clippy 通过。阶段 Rust 套件的关闭测试仍读取旧会话目录，修正为分类目录后两项关闭测试通过；其余模块与后续文档测试通过。

官方依据：[网页搜索](https://platform.claude.com/docs/en/agents-and-tools/tool-use/web-search-tool)、[网页读取](https://platform.claude.com/docs/en/agents-and-tools/tool-use/web-fetch-tool)、[工具定义与选择](https://platform.claude.com/docs/en/agents-and-tools/tool-use/define-tools)。动态过滤、供应商代码执行以及其他协议的托管工具没有并入这一批实现。

## DeepSeek Responses 内置搜索

初次接入的 `hosted_tools` 策略仅 `deepseek-v4-pro` 注入 `{"type":"web_search"}`；其他模型保留本机工具。Provider 对象继续使用 `hosted_tools`。Claude 原有次数、正文和域名配置合同保留；DeepSeek 不接受上游不支持的控制参数，此字段保留原有合同。当前统一入口与显式模式选择见 [Provider 网页访问](provider-web-access.md)。

搜索调用、模型回答、函数调用和引用使用现有 `ModelProviderState` 持久化，按协议和模型匹配回放。SSE 搜索状态不转换成本机工具；搜索后调用本机工具仍能续传完整搜索历史。只有引用、没有新搜索调用的最终回答也保存来源。标准 OpenAI Responses 的状态与托管工具边界不变。

原生网关先解析已授权目录中的实际协议，再验证历史；只有 DeepSeek 协议接受返回的搜索历史项，客户端提交的工具定义仍仅允许函数，托管搜索由所有者策略注入。Server 对 DeepSeek 按上下文和输出上限预留估算，按报告结算，不把不受上游支持的次数限制当作硬上限。界面和正式指南明确区分 Token 估算与搜索费用。

DeepSeek 相关 Rust 合同 15 项、配置策略 1 项、预算生命周期 1 项、Server 原生网关 12 项通过。真实四条模型来源浏览器通过（30.10 秒、8 次上游调用），覆盖中英韩启停保存、Flash 禁用状态、搜索历史续传、引用刷新及 390 px 窄屏；现有 Claude 四路径浏览器回归通过（31.47 秒、12 次上游调用）。已检查插件字段、Markdown 正文、内置搜索设置和引用截图，控制台、页面与非预期 HTTP 错误为空。使用官方协议合同服务，不使用真实收费凭据。官方依据：[DeepSeek Responses](https://api-docs.deepseek.com/zh-cn/guides/responses_api/)，引用结构参考其兼容的 [Responses 输出与引用](https://developers.openai.com/api/docs/guides/tools-web-search#output-and-citations)。

本次相关 Builtins／Protocol／Control／Server／Local／Cloud 六个 crate 的全目标严格 Clippy、Web 类型与三语检查、242 篇 Markdown 文档门禁和 23 项部署工具测试通过。

PR 安全检查命中 2026-10-28 公布的 [RUSTSEC-2026-0414](https://rustsec.org/advisories/RUSTSEC-2026-0414.html)：`pulldown-cmark` 的 WikiLinks 空显示文本可造成事件指数增长。文档工具未启用该选项；依赖仍升级并锁定至修复版本 0.13.5，没有新增忽略项。更新后的 advisories／licenses／sources 检查及 Markdown 公式、代码和货币文本合同通过。
