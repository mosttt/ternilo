# 原生账号恢复实施记录

状态：已实现并完成分支验证，等待合入。用户指南见[密码恢复](../account-recovery.md)及其[英文版](../account-recovery.en.md)。

`ternilo-server admin reset-password` 使用已有私有配置和隐藏输入／标准输入，为现有原生账号更新密码。账号身份、角色、状态、个人空间、项目、电脑和模型授权不变。密码哈希、本站原生与 OIDC 会话撤销、运维审计在同一事务提交。

原生登录先在数据库锁外验证 Argon2，再携带不可伪造的验证结果进入会话签发事务。在账号锁内复核密码哈希，拒绝重置前通过验证的旧凭据；保留已有的纯身份验证接口供可信调用使用。

真实浏览器验证发现，服务端拒绝旧实时连接后，网页没有主动确认登录状态。现在仅对 Server 的连接级 policy 错误请求当前身份，由既有 HTTP 认证处理区分失效凭据和仍有效但被策略限制的账号。会话级权限错误不会触发登录失效。

已通过以下验证：

- 新增 SQLite 与受限 PostgreSQL 恢复契约，包括原账号／个人空间／电脑凭据保留、旧密码和旧验证结果拒绝、原生与 OIDC 会话撤销、其他账号不受影响、错误密码长度无副作用、封禁状态保留、已移除及纯 OIDC 账号拒绝恢复。
- 4 项原有原生身份回归，以及 PostgreSQL 认证设置、OIDC 会话与 Node 路由回归。
- 真实 Server／CLI／Chromium 恢复流程，包含隐藏密码输入路径的非交互拒绝、标准输入 CRLF 和空格保留、无密码输出、旧网页自动返回登录、恢复后项目归属不变。
- 5 项原有浏览器流程：多账号 Node 共享与暂停、权限组及派生会话撤权、标准 OIDC、原生账号绑定 OIDC、OAuth／Turnstile 设置。
- Control／Server 全目标 Clippy、TypeScript、49 项 API／实时连接／工作台状态测试、i18n、文档链接与工作流语法检查。

CI 已加入新浏览器入口与 PostgreSQL 恢复契约。该功能不提供邮件找回、外部 IdP 凭据恢复、自助密码更改或 MFA，也不通过密码重置自动解封账号或取消已经接受的任务。

## English

Implemented and validated on the feature branch, pending merge. The operator CLI resets an existing native password while preserving identity and resources. Password replacement, native/OIDC site-session revocation and audit commit atomically. Native session issuance revalidates the password proof under the account lock, rejecting verification completed before a reset.

Browser verification also identified and fixed missing client reauthentication after a connection-level policy rejection. The client checks its identity through the existing HTTP authentication flow, retaining the distinction between invalid credentials and valid accounts restricted by policy.

Validation passed for SQLite and restricted PostgreSQL recovery contracts, existing identity/PostgreSQL regressions, real CLI/browser recovery, five existing account/sharing/OIDC/security browser scenarios, scoped Clippy, TypeScript, 49 frontend state/API tests, i18n, documentation and workflow checks. Email recovery, external IdP recovery, self-service password changes and MFA remain outside this feature.
