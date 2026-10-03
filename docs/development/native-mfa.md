# 原生账号多因素认证

2026-10-03：源码及本地验收完成，纳入 v0.2.0 候选。GitHub 发行状态另见剩余任务清单。

首个完整流程采用验证码应用（TOTP）和一次性恢复码。原生账号用当前密码建立验证器、确认验证码后启用；启用后撤销既有本站浏览器登录。原生登录及已绑定的本站 OIDC 登录都必须完成第二因素。邮件密码找回保留 MFA，不能成为绕过路径。服务账号、电脑及模型 API 凭据继续使用各自的独立授权。

密钥按账号及配置代次加密保存；记录最后接受的 TOTP 时间步，避免同一验证码重放。恢复码只显示一次，服务端保存摘要；失败次数、临时限制和 OIDC 待验证状态存入双库，跨 Server 使用同一规则。启用／停用、并发配置变化、旧密码验证证明和 OIDC 刷新都必须复核当前 MFA 代次。

本机运维命令 `ternilo-server admin reset-mfa --config-dir DIR --username NAME` 清除 MFA、保留账号和资源并撤销原登录；没有匿名跳过 MFA 的接口。原生账号设置页、密码登录和 OIDC 第二因素页均已实现，含手机布局；正式使用说明见[账号安全](../zh-CN/account-recovery.md)。

实现核对 [RFC 6238](https://www.rfc-editor.org/rfc/rfc6238)、Google Authenticator 的 [Key URI Format](https://github.com/google/google-authenticator/wiki/Key-Uri-Format) 和 [totp-rs 6.0.0](https://crates.io/crates/totp-rs/6.0.0) 的发行源码。使用维护中的实现，不自行设计验证码算法；其 `check` 返回已匹配时间步，重放防护由 Ternilo 持久层承担。


## 验证结果

- SQLite 与受限 PostgreSQL 合同覆盖配置替换、启用／停用、验证码重放与限流、并发恢复码只成功一次、旧密码证明／旧 MFA 代次、OIDC 待验证／刷新／原始令牌边界、邮件改密和私有运维恢复；主密钥轮换保留验证器及尚未完成的 OIDC 挑战。
- 两种数据库的真实 Chromium 流程均通过：二维码可解码、保存恢复码确认、手机无水平溢出、密码二次验证、OIDC 二次验证及刷新、邮件改密后仍需 MFA、停用及实际 CLI 恢复。浏览器使用独立 Node HMAC 生成 TOTP，不复用服务端算法实现；没有调用用户邮箱或改动运行实例。
- 152 个前端测试文件／989 项测试通过；Server 相关测试 125 项通过、5 项 PostgreSQL 专项保持显式忽略；Control 定向认证回归、Clippy、依赖许可证／漏洞检查、TypeScript 和翻译检查通过。受限 PostgreSQL 合同和浏览器另行执行，不将忽略项计为通过。
- CI 已接入 MFA 浏览器和受限 PostgreSQL 测试。这里描述本地验收结果，不代表 GitHub 检查或公开发行已完成。

当前提供原生账号的验证器与恢复码；已绑定 OIDC 的同一账号也验证本站第二因素。仅 OIDC 的账号由 IdP 实施 MFA，本站不为它创建原生密码或验证器配置。通行密钥不在本实现中。
