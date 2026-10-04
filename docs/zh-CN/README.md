# Ternilo 文档

[English](../en/README.md) · [简体中文](README.md)

先选择下面的入口，完成启动与第一个任务，再阅读所需功能。文档中的命令使用已安装到 `PATH` 的程序名。

## 从这里开始

| 你的情况 | 阅读顺序 |
|---|---|
| 只在当前电脑使用 | [本地快速开始](getting-started.md) → [用户指南](user-guide.md) |
| 使用桌面窗口 | [桌面应用](desktop.md) → [本地快速开始](getting-started.md) |
| 用手机或另一台电脑继续工作 | [部署 Server](deployment.md)／[Docker Compose](docker-compose.md) → [接入电脑](remote-access.md) |
| 与其他账号协作 | [远程访问](remote-access.md) → [账号与协作](collaboration.md) → [平台管理](platform-management.md) |
| 提供平台托管执行 | [Worker 部署](worker.md) |

本机客户端叫 `ternilo`，统一远程入口叫 `ternilo-server`。自己的电脑和 VPS 使用同一种客户端接入。Worker 是可选的托管执行组件，Work 仍是独立预留设计。

## 使用工作台

- [用户指南](user-guide.md)：工作区、会话、队列、任务权限和历史。
- [模型配置](models.md)与[平台模型接入](model-service.md)：连接自己的服务或使用已授权模型。
- [设置与预设](settings.md)：界面语言、主题、配置目标和 Agent 预设。
- [文件与附件](files.md)：引用项目文件、上传、预览和下载。
- [工具与多 Agent](agents.md)：技能、目标、计划、定时任务、后台任务和工作流。
- [联网排查](web-access.md)：抓取、搜索和代理连接问题。

## 管理 Server 与协作

- [Server 部署与运维](deployment.md)和[Docker Compose](docker-compose.md)：网页首次设置、SQLite／PostgreSQL、HTTPS、运行与备份。
- [电脑管理](computers.md)：系统 ID、可修改名称、详情、暂停／恢复、吊销和移除登记。
- [账号与平台管理](platform-management.md)、[登录配置](server-authentication.md)和[账号安全](account-recovery.md)：访问模式、账号、OIDC、验证、密码与 MFA。
- [账号与协作](collaboration.md)、[项目共享](project-sharing.md)和[资源管理权交接](resource-management.md)：成员、资源权限、提交者及归属。
- [服务账号](service-accounts.md)：自动化身份、凭据与明确授权。
- [设备模型用量](device-provider-usage.md)、[用量核对](model-usage-reconciliation.md)和[性能](performance.md)：额度、账本、导出、限制及验证范围。
- [多 Server 路由](server-cluster.md)：共享数据库、实例地址、跨实例 Node 请求与事件补读。
- [配置与数据目录](data-layout.md)：程序配置、持久数据、凭据与备份位置。

## 扩展与开发

- [自动化与 SDK](automation.md)和[远程 SDK](remote-sdks.md)：CLI、JSON-RPC、ACP、Python 与 TypeScript。
- [扩展配置](extensions.md)和[扩展包开发](extension-packages.md)：MCP、LSP、Hooks、Rhai、WASM、签名与安装。
- [开发指南](contributing.md)：源码检出、环境、构建和按范围验证。
- [架构](architecture.md)与[Server 参考](server-reference.md)：模块职责、存储、事务和接口。
- [包内程序](binaries.md)、[构建交付包](release-packaging.md)和[CI／发行](ci-release.md)：可执行程序、安装包、镜像及发布验证。
- [标识与凭据](identifiers.md)和[会话日志修复](session-log-repair.md)：当前命名与离线维护。

支持范围见[产品与能力边界](product.md)，权限和隔离见[安全模型](security.md)。正式文档提供中文与英文版本，`docs/development/` 保存中文开发记录；开发记录中的计划不代表已交付能力。
