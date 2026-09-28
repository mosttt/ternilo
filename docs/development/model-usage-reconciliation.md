# 未知模型用量核对 / Unknown model usage reconciliation

状态：实现及本机完整验收通过。 / Status: implementation and local end-to-end validation passed.

先完成 Server 已接收请求的核对，不把设备直接调用 Provider 的自报记录写入权威预算账本。仅 Owner／Admin（现有额度管理权限）可核对已结束且实际尝试过、缺少完整用量的 attempt。记录上游凭据之外的核对依据和说明，复用已有尝试结算与预算刷新事务，保留原月份、操作者、模型受益人、请求状态、错误和上游 request ID。禁止修改已有确定用量；相同补录重试须幂等，迟到上游结算与人工核对须在同一请求锁下串行。

First close reconciliation for requests accepted by Server. Device-reported direct Provider usage remains separate from authoritative budgets. Only Owner/Admin with the existing grant-management permission may reconcile terminal, attempted records missing complete usage. Require an evidence reference and explanation without credentials, reuse attempt settlement and budget-refresh transactions, and retain the original month, actor, beneficiary, request state, error and upstream request ID. Known usage cannot be overwritten; identical retries are idempotent and late upstream settlement shares the request lock.

页面按 attempt 提供入口，清晰标记为管理员核对，显示输入／输出和可选缓存／推理分项。不将预留 token 当作已知消耗；补录后列表、汇总和预算应一致。验证包括双库、角色／状态限制、冲突、重试、审计、跨月预算以及真实浏览器操作。

Expose reconciliation per attempt and label its administrative provenance. Show input/output and optional cache/reasoning details. Reservations remain distinct from known consumption; request lists, aggregates and budgets must agree after reconciliation. Validate both databases, authorization and terminal-state rules, conflicts, retries, audit records, month boundaries and the real browser flow.


实现复用 `control_platform_audit` 保存每次管理员核对，仅在管理页面按需读取，不改变正常网关计量路径或数据库结构。计数写入、托管预算刷新与审计原子提交。页面提供核对及持久记录查看；冲突后停止编辑并刷新账本。

Reconciliation records reuse `control_platform_audit` and are fetched on demand by the management UI. Normal gateway accounting and the database schema are unchanged. Counter settlement, managed-budget refresh and audit commit atomically. The UI stops editing after a conflicting settlement and refreshes the ledger.

已通过 13 项 SQLite 模型服务测试、4 项受限 PostgreSQL 模型契约及原有 4 项认证／Node／恢复 PostgreSQL 契约；13 项受影响 Web 测试、TypeScript、i18n、文档检查及 Control／Server Clippy。真实浏览器覆盖未知消耗、管理员核对、幂等重试、普通账号／Auditor 拒绝修改、重新加载后的审计记录、原账号额度和 320／390 视口。

Passed: 13 SQLite model-service tests, four restricted PostgreSQL model contracts and four existing PostgreSQL authentication/Node/recovery contracts; 13 affected Web tests, TypeScript, i18n, documentation checks and Control/Server Clippy. The real browser covers unknown usage, administrative correction, idempotent retries, ordinary-user/Auditor write denial, durable records after reload, the original account quota and 320/390 viewports.

设备直接 Provider 用量尚未在本次实现中汇入；下一项应维持其设备报告来源，与已核验的 Server 账本分开呈现。

Direct Provider usage reported by devices is the next task and remains separate from the authoritative Server ledger.
