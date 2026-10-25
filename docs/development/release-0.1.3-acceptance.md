# v0.1.3 发行验收

[v0.1.3](https://github.com/mosttt/ternilo/releases/tag/v0.1.3) 已公开，来源为 `cf965e47905303b488f3b2303821540b6b9a2b7c`。[发行检查 36765832448](https://github.com/mosttt/ternilo/actions/runs/36765832448) 全部通过。该版本交付四平台客户端／Server、Desktop 安装器和 28 项附件；这不是当前平台与附件数量。

当时的全工作区 Clippy、953 项 Web 单元测试、类型、国际化、SQLite 和受限 PostgreSQL 验收通过。真实浏览器覆盖 Node 账号清理、OIDC 来源校验、跨 Server 共享撤权及 PWA 恢复；Windows 和两种 macOS 完成持久化与账号授权检查。

28 项公开附件的 GitHub digest、校验清单和实际下载一致；八个便携归档的平台头、程序集合及各自 77 份正式文档通过检查，六个 Desktop 安装器格式正确。Desktop 后台服务的启动、持久化、停止和重启验证通过，安装器未进行发行者签名／公证，自动更新关闭。

公开镜像匿名拉取及无需本地构建的 Compose 初始化、登录、只读根目录、UID 10001 与重启后身份保留通过。该版本发布的 linux/amd64 镜像摘要为 `sha256:4744611182c4849fd13b53009dc9beedee5d93b34d577ef34912f721f2e91566`。

功能实现见[Node 账号清理](node-account-cleanup.md)、[OIDC 来源校验](oidc-browser-origin.md)和[共享权限通知](resource-notifications.md)。后续平台扩展、资源迁移及恢复进度见[任务清单](remaining-task-audit.md)。
