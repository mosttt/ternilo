# Ternilo

[English](README.md) · [简体中文](README.zh-CN.md)

让想法，动起来。

Ternilo 是能读写项目文件、执行命令并保留工作过程的 AI 助手。你可以只在自己的电脑上使用，也可以通过一个 Server 管理多台电脑和 VPS，与其他人协作，或提供模型和托管执行服务。

## 选择使用方式

| 你的需要 | 要运行的程序 | 从这里开始 |
|---|---|---|
| 在当前电脑完成任务 | `ternilo` | [本地快速开始](docs/getting-started.md) |
| 从手机或其他电脑访问自己的机器 | `ternilo` + `ternilo-server` | [远程访问](docs/remote-access.md) |
| 多人使用、共享模型和工作 | 同一个 Server 开启多用户模式，按资源授权 | [部署](docs/deployment.md)、[平台管理](docs/platform-management.md) |
| 平台提供隔离的执行环境 | 在 Server 上启用托管执行，再接入 `ternilo-worker` | [Worker 部署](docs/worker.md) |

单用户和多用户都能管理本地电脑与云端 VPS，都能选择 SQLite 或 PostgreSQL。Worker 是可选组件；自己的 VPS 可以直接运行普通 `ternilo`。Work 容器模型是独立的预留设计，不等同于现有 Worker。

## 开始使用

已取得针对当前系统构建的交付包时，在解压目录运行：

```bash
./bin/ternilo serve
```

打开终端显示的地址，选择工作文件夹，在“设置 → 模型”添加模型连接，然后交代任务。没有可用模型时，页面会提示配置并保留草稿。

本地网页只监听回环地址。需要远程访问时，由电脑主动连接 Server，再通过 Server 的登录入口访问。桌面应用也连接同一个本地服务；关闭网页或窗口不会停止已经开始的任务。

二进制安装包与镜像的可用版本以仓库发行记录为准。也可以按[构建交付包](docs/release-packaging.md)生成独立的本地、Server 或 Worker 安装包；运行这些包无需 Rust、Node.js 或 Linorun 源码。

仓库已提供[GitHub CI 与发行流程](docs/ci-release.md)：分别检查、构建四平台客户端／Server 及桌面包，并验收和发布 Server 镜像。主分支生成候选产物；版本标签经过检查后发布 Server 镜像并生成 Release 草稿。

### 从源码运行

源码依赖同级目录中的 [Linorun](https://github.com/mosttt/linorun)，请使用与当前 Ternilo 版本配套的源码：

配套提交固定在 `.github/linorun-revision`；CI 和发行均使用该提交。本地切换 Linorun 前先确认它没有未保存的修改，不要覆盖正在开发的代码。

```text
/path/to/source/
├── linorun/
└── ternilo/
```

准备 `rust-toolchain.toml` 指定的 Rust 工具链和 Node.js，在 Ternilo 仓库执行：

```bash
npm --prefix web ci
npm --prefix web run build
cargo run --locked -p ternilo -- serve
```

Linux 执行受限命令还需要 `bubblewrap`。更完整的环境要求和验证方式见[开发指南](docs/contributing.md)。

## 可以做什么

- 管理工作区与会话，流式对话，折叠思考和工具过程，搜索与导出历史。
- 配置 OpenAI Chat、OpenAI Responses、DeepSeek Responses、原生 Gemini 或 Claude Messages；使用自己的模型或 Server 授予的模型额度。
- 读写文件、执行命令、运行持久终端和后台任务，预览并下载附件与生成文件。
- 使用技能、计划、目标、定时任务、子 Agent、Agent Team 和工作流。
- 通过预设组合插件，接入 MCP、LSP、网页工具、Rhai 与签名的 Rhai／WASM 扩展。
- 用只读、工作区写入或完整访问控制任务权限，按次审批需要确认的操作。
- 在 Server 管理账号、机器、团队、工作区／会话共享、模型授权、用量和审计。
- 通过网页、桌面、PWA、CLI、JSON-RPC、ACP、Python 或 TypeScript SDK 使用同一套能力。

支持范围和限制见[产品与能力边界](docs/product.md)。部署前应根据实际硬件和工作负载验证容量。

## 文档与源码

[文档首页](docs/README.zh-CN.md)按安装部署、日常使用、平台管理、开发维护组织。源码中的 `apps/` 是程序入口，`crates/` 是共享模块，`web/` 是工作台，`sdk/` 是客户端，`deploy/` 和 `scripts/` 提供部署与打包工具。`docs/development/` 保存开发进度、待办和技术设计，不作为已支持功能的说明。

## 许可证

Ternilo 使用 [Apache-2.0 许可证](LICENSE)。第三方组件保留各自的许可证，见[第三方声明](THIRD_PARTY_NOTICES.md)。
