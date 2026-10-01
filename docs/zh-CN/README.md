# Ternilo 文档

[English](../en/README.md) · [简体中文](README.md)

第一次使用，从[本地快速开始](getting-started.md)进入。需要远程管理多台机器，接着阅读[远程访问](remote-access.md)。先完成一个真实任务，再按需要配置协作、插件和托管执行。

## 安装部署

- [本地快速开始](getting-started.md)：启动程序、打开文件夹、配置模型、发送第一个任务。
- [远程访问](remote-access.md)：部署 Server、接入自己的电脑或 VPS、从其他设备访问。
- [Server 部署与运维](deployment.md)：初始化、SQLite／PostgreSQL、反向代理、诊断、备份与恢复。
- [桌面应用](desktop.md)：原生窗口、后台服务、通知和深链。
- [托管 Worker](worker.md)：现有可选执行服务的接入、存储和维护；完整 Work 能力仍为预留范围。

## 日常使用

- [工作区与对话](user-guide.md)：项目和目录、会话、队列、权限、阅读过程与导出。
- [模型配置](models.md)：Provider、协议、思考过程、上下文和请求超时。
- [设置与预设](settings.md)：配置归属、Agent 预设、工具调用和执行步数上限。
- [消息与文件](files.md)：上传、上下文引用、生成产物、预览和下载。
- [工具与多 Agent](agents.md)：技能、计划、工作流、子代理、后台任务与服务。
- [账号与协作](collaboration.md)：我的机器、独立输入、共享权限与提交者。
- [资源管理权交接](resource-management.md)：在团队内转交工作区或会话，保留原存储和执行身份。
- [联网工具排查](web-access.md)：网页抓取、搜索和代理 DNS。

## 平台管理

- [账号与平台管理](platform-management.md)：单／多用户模式、注册、账号、团队、管理职责与 Worker 页面。
- [登录与人机验证](server-authentication.md)：在 Server 设置 OAuth 2.0／OIDC、Turnstile、管理密钥及恢复登录。
- [原生账号密码恢复](account-recovery.md)：通过本机维护命令重置密码，保留原账号和资源。
- [平台模型服务](model-service.md)：发布模型、分配授权、领取模型 Key、客户端登录与用量。
- [产品与能力边界](product.md)：两个使用模式、三类共享、页面分工、已有能力与当前限制。
- [安全模型](security.md)：身份、凭据、审批、操作系统隔离和遥测。

## 开发维护

- [架构](architecture.md)：程序和模块的职责、数据归属与执行链路。
- [Server 技术参考](server-reference.md)：路由、事务、执行与计量、HTTP 接口。
- [扩展配置](extensions.md)与[扩展包开发](extension-packages.md)：插件、MCP、LSP、Hooks、Rhai、WASM 和签名。
- [自动化与 SDK](automation.md)：CLI、JSON-RPC、ACP、Python 和 TypeScript。
- [开发与验证](contributing.md)：源码环境、按范围测试、浏览器与双库验证。
- [构建交付包](release-packaging.md)：本地二进制包、版本镜像和离线交付。
- [CI 与 GitHub 发行](ci-release.md)：检查、各平台二进制／桌面安装包、GHCR Server 镜像和 Release 草稿。

文档按使用任务组织。功能限制见[产品与能力边界](product.md)，尚未实施的技术设计位于 `docs/development/`，不进入交付包。

命令默认从源码或交付包根目录执行。示例域名、路径、账号和密钥需要按自己的环境填写，不代表已提供公共服务。

- [未知模型用量核对](model-usage-reconciliation.md)：管理员依据上游记录补齐未知计数，保留原月份和审计。

- [标识与凭据命名](identifiers.md)：当前对外前缀与用途。
