# Claude 托管网页工具

2026-10-03，优先级 2 的托管模型工具接入。Provider 所有者显式配置 `hosted_tools`，仅 Claude Messages 协议使用基本 `web_search_20250305` 和 `web_fetch_20250910`；默认关闭。

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
