<p align="center">
  <img src="https://raw.githubusercontent.com/mosttt/ternilo/main/web/public/assets/icon.svg" alt="Ternilo logo" width="88" height="88">
</p>

<h1 align="center">Ternilo</h1>

<p align="center"><strong>让想法，动起来。</strong><br>能读取项目文件、执行工具的 AI 助手，支持本机使用、远程访问与多电脑协作。</p>

<p align="center">
  <a href="https://github.com/mosttt/ternilo/actions/workflows/ci.yml"><img src="https://github.com/mosttt/ternilo/actions/workflows/ci.yml/badge.svg?branch=main" alt="CI status"></a>
  <a href="https://github.com/mosttt/ternilo/releases/latest"><img src="https://img.shields.io/github/v/release/mosttt/ternilo?color=456bdc" alt="Latest release"></a>
  <a href="https://github.com/mosttt/ternilo/pkgs/container/ternilo-server"><img src="https://img.shields.io/badge/GHCR-ternilo--server-456bdc?logo=docker&amp;logoColor=white" alt="Server container image"></a>
  <a href="LICENSE"><img src="https://img.shields.io/github/license/mosttt/ternilo?color=456bdc" alt="Apache-2.0 license"></a>
</p>

<p align="center">
  <a href="docs/zh-CN/getting-started.md">快速开始</a> ·
  <a href="docs/zh-CN/README.md">文档</a> ·
  <a href="docs/zh-CN/deployment.md">部署</a> ·
  <a href="https://github.com/mosttt/ternilo/releases">下载</a> ·
  <a href="https://github.com/mosttt/ternilo/issues">问题反馈</a>
</p>

<p align="center"><a href="README.md">English</a> · <a href="README.zh-CN.md">简体中文</a></p>

---

Ternilo 是能读取项目文件、执行工具并保留工作过程的 AI 助手。你可以在自己的电脑上使用，也可以将电脑和 VPS 接入 Server，从其他设备继续工作或与他人协作。

## 先在自己的电脑上使用

从 [Releases](https://github.com/mosttt/ternilo/releases) 下载适合当前系统的桌面安装包或客户端归档。打开桌面应用；使用命令行时，先将可执行程序安装到 `PATH`，再运行：

```sh
ternilo serve --open-browser
```

选择工作文件夹，在“设置 → 模型”添加模型连接，然后发送任务。对话页会逐步显示回答和工具执行过程。[本地快速开始](docs/zh-CN/getting-started.md)说明安装、第一次配置模型和停止服务的完整步骤。

本机使用只需要 `ternilo`。桌面应用和浏览器连接同一个本地服务。界面首次按浏览器首选语言显示中文或英文，手动选择后使用保存的语言。

## 从其他设备访问电脑

在其他设备可访问的电脑或服务器运行 `ternilo-server`。Docker 用户按 [Compose 部署](docs/zh-CN/docker-compose.md)操作；直接运行二进制见 [Server 部署与运维](docs/zh-CN/deployment.md)。

Server 首次启动提供受保护的网页设置：从日志复制初始化 Key，在网页选择 SQLite 或 PostgreSQL，再创建管理员。设置完成后，重启读取保存的数据库连接和原账号。

登录 Server，进入“用户设置 → 我的机器”，生成连接命令并在执行任务的电脑上运行。之后从手机或其他浏览器打开 Server，选择这台电脑即可继续工作。文件和工具仍在执行电脑上，电脑主动连接 Server，不需要开放它的本地端口。

多人使用时，由所有者开启多用户模式，并授予具体资源权限。模型授权与工作区权限分别管理。接入步骤见 [远程访问](docs/zh-CN/remote-access.md)，共享方式见 [账号与协作](docs/zh-CN/collaboration.md)。

## 按需要继续阅读

| 你要做什么 | 阅读入口 |
|---|---|
| 本机启动并完成第一个任务 | [本地快速开始](docs/zh-CN/getting-started.md) |
| 安装和使用桌面应用 | [桌面应用](docs/zh-CN/desktop.md) |
| 用 Docker 部署，选择 SQLite 或 PostgreSQL | [Docker Compose](docs/zh-CN/docker-compose.md) |
| 直接运行 Server 二进制，配置 HTTPS 或备份数据 | [Server 部署与运维](docs/zh-CN/deployment.md) |
| 接入自己的电脑和 VPS | [远程访问](docs/zh-CN/remote-access.md) |
| 使用模型、文件、工具和会话 | [用户指南](docs/zh-CN/user-guide.md)、[模型配置](docs/zh-CN/models.md) |
| 管理账号、电脑与共享资源 | [平台管理](docs/zh-CN/platform-management.md)、[电脑管理](docs/zh-CN/computers.md) |

[文档首页](docs/zh-CN/README.md)还提供 SDK、扩展和运维入口。平台托管执行使用可选的 [Worker](docs/zh-CN/worker.md)，Work 容器组件仍是独立预留设计。

## 主要能力

Ternilo 提供工作区和会话、流式回答、文件与命令工具、持久终端、后台任务、附件、历史搜索和导出。支持 OpenAI Chat／Responses、DeepSeek Responses、原生 Gemini 和 Claude Messages，也可使用 Server 授权的模型。

预设可以组合插件与工具，包括技能、计划、显式启用的目标、定时任务、子 Agent、MCP、LSP 以及签名的 Rhai／WASM 扩展。任务权限控制文件访问和执行范围。Server 另提供账号、电脑管理、资源共享、模型授权、用量和审计。

已支持行为、隔离边界和剩余能力见 [产品与能力边界](docs/zh-CN/product.md)。

## 开发

源码构建需要 `rust-toolchain.toml` 指定的 Rust、Node.js，以及 `.github/linorun-revision` 固定的 Linorun 提交。检出、构建和验证步骤见 [开发指南](docs/zh-CN/contributing.md)，制作交付包见 [构建与打包](docs/zh-CN/release-packaging.md)。

`apps/` 保存程序入口，`crates/` 是共享 Rust 模块，`web/` 是界面，`sdk/` 是客户端库。正式文档位于 `docs/zh-CN/` 和 `docs/en/`，`docs/development/` 保存中文开发记录。

[CI 与发行](docs/zh-CN/ci-release.md)验证源码、原生平台和部署，再发布客户端／Server 归档、桌面安装包及 Server 镜像。可用下载以 [Releases](https://github.com/mosttt/ternilo/releases) 为准。

## 许可证

[Apache-2.0](LICENSE)。第三方组件保留自己的许可证，见 [第三方声明](THIRD_PARTY_NOTICES.md)。

## 友链

[LINUX DO](https://linux.do/)
