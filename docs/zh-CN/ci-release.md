# CI 与 GitHub 发行

流水线分成检查、打包和发布三个职责。源码检出在 `ternilo/`，配套 Linorun 检出在同级 `linorun/`；公共 Action 统一准备依赖，不要求自托管 runner 或访问开发者的机器。

## 检查与打包

| 工作流 | 触发 | 内容与产物 |
|---|---|---|
| `Checks` | PR、main 推送、手动；发行时复用 | Web、文档、Rust、部署脚本、依赖门禁；真实浏览器、Linux 原生桌面、SQLite／PostgreSQL 恢复；Linux ARM64／Windows／两种 macOS 架构编译检查。main 推送、PR 和手动检查不自动打包；需要候选程序时单独运行 `Build packages` |
| `Build packages` | 手动；发行时复用 | 各平台 release 二进制、桌面安装器与 SHA256；仅上传 Actions artifacts，不创建 Release 或推送镜像 |
| `Release` | `v*` 标签、手动 | 版本检查后并行执行 `Checks`、`Build packages` 与 Server 镜像构建／验收；全部通过后，只有匹配版本标签才推送 GHCR、上传全部附件并公开 GitHub Release |

在 GitHub Actions 页面手动运行 `Build packages` 可选择一个 Rust 目标或 `all`，独立取得构建产物；未经过完整 `Checks` 的手动包只能当作候选。手动运行 `Release` 并选择分支时，完成全部验证和构建，但不发布。选择版本标签时等同于标签发行。

客户端／Server 门禁同时运行 LocalApplication、内置工具、授权、RPC／ACP 的 Rust 测试，以及 Python／TypeScript SDK 对实际客户端二进制的读写验证。浏览器覆盖本地任务、电脑登记与共享、团队权限组和独立模型服务；PostgreSQL 使用一次性数据库与受限运行账号验证认证及 Node 工作区路由。现有 Worker 不作为这条闭环的运行前提。

Rust 缓存包含依赖的编译结果，按工具链、构建环境、目标平台及 Linorun 提交区分；检查失败后也保留依赖缓存，便于修复后复验。工作区自身代码仍由 Cargo 重新判断和编译，缓存不替代检查结果。

检查按工作流、分支／PR、事件及具体作业控制并发；同一范围的新提交会取消过时的检查。六个目标打包分别排队，已经开始的构建继续完成，后续保留最新候选。新提交的检查无需等待旧提交的安装器构建。Release 标签流程仍按标签串行，发布权限和门禁不变。

`Checks` 只负责校验；`Release` 统一调用一次 `Build packages`，普通源码推送不会再额外启动六个发行构建。检查、六个目标包和镜像构建可以并行；镜像推送与 Release 公开仍等待全部检查和构建成功。候选构建作业没有发布权限。

二进制及安装器矩阵：

| 平台 | Rust target | CLI／Server 归档 | 桌面安装器 |
|---|---|---|---|
| Linux x86_64 | `x86_64-unknown-linux-gnu` | Client、Server `.tar.gz` | `.deb`、`.AppImage` |
| Linux ARM64 | `aarch64-unknown-linux-gnu` | Client、Server `.tar.gz` | `.deb`、`.AppImage` |
| Windows x86_64 | `x86_64-pc-windows-msvc` | Client、Server `.zip` | NSIS `.exe`、`.msi` |
| Windows ARM64 | `aarch64-pc-windows-msvc` | Client、Server `.zip` | NSIS `.exe` |
| macOS Apple Silicon | `aarch64-apple-darwin` | Client、Server `.tar.gz` | `.dmg` |
| macOS Intel | `x86_64-apple-darwin` | Client、Server `.tar.gz` | `.dmg` |

客户端归档以 `ternilo-<版本>-<平台>` 命名，包含 `ternilo` 和 `ternilo-plugin`，Windows 另包含必须的 `ternilo-sandbox-windows.exe`；Server 归档包含独立 `ternilo-server`。桌面包与 CLI 包独立，同版共享实现。各归档附带正式文档和必要示例，不包含运行数据、真实凭据、开发记录或 Work 产物。

Linux 二进制在 Ubuntu 24.04 构建，不承诺更早 glibc 系统兼容；Server 容器另在 Debian Bookworm 源码构建。Windows ARM64 提供 NSIS EXE，安装器自身通过 x86 仿真运行，应用及辅助程序为原生 ARM64；其他架构未列入当前发行矩阵。桌面安装器默认未进行发行者代码签名／公证，自动更新关闭；macOS 和 Windows 可能显示来源警告。构建成功不代替目标平台安装、原生隔离和签名验收。

## 固定输入与权限

Rust 使用 `rust-toolchain.toml`，依赖使用 `Cargo.lock` 和 `web/package-lock.json`，Linorun 使用 `.github/linorun-revision` 中的固定提交，不追踪移动的 `main`。所有外部 Actions 固定到完整 commit；Tauri CLI 同样固定版本。更改 Linorun 时必须同时核验源码兼容性和 lockfile。

PR 检查只读仓库，不使用 `pull_request_target`、部署密钥或发布 Token。浏览器访问临时测试服务，不需要生产模型 Key、LINUX DO 密钥或 Turnstile 密钥。Ubuntu runner 会释放预装 Android／.NET／Haskell SDK 的空间，并调整测试所需命名空间设置；这些步骤只用于可销毁的 GitHub 托管 runner，不是生产部署或开发者机器的操作要求。执行测试不得连接日常数据库。

GHCR 上传仅在镜像作业取得 `packages: write`，使用 GitHub 自带的 `GITHUB_TOKEN`；Release 发布作业才取得 `contents: write`。源码 checkout 不保留 Git 凭据。仓库应允许 Actions，并为 `main` 配置必需检查；环境保护和标签发布权限由维护者在 GitHub 设置。

## Server 镜像

发行镜像包含 `linux/amd64` 与 `linux/arm64`。两种架构在各自的原生 GitHub Runner 上从同一源码和固定 Linorun 构建 `server` target，并分别验证初始化、原生登录、实例设置、嵌入网页、只读根文件系统、UID 10001 和重启持久化。每份已验收镜像及 image ID 暂存一天；发布作业等待全部检查及六个目标打包通过，加载镜像并核对 image ID 与架构，再按不可变来源提交标签推送各架构镜像。最终版本标签和 `sha-<commit>` 指向由这两份摘要组成的多平台索引；发布前核对索引的架构和摘要集合。过期候选必须重建，失败不会进入公开 Release。

镜像地址自动采用仓库所有者的小写名称，例如仓库由 `mosttt` 持有时为 `ghcr.io/mosttt/ternilo-server:<版本>`。另推送 `sha-<完整源码提交>` 标签。稳定版 Release 公开后，`Update Server image latest` 工作流把 `latest` 指向 GitHub 最新稳定版的同一份多平台索引；预发行版不更新它。工作流核对发行附件的校验和、两种架构及镜像摘要，不重新构建，也不会因为重跑旧版本而降级。维护者也可手动运行该工作流补齐别名。`server-image.txt` 记录 registry digest，部署时优先固定版本或 digest。

镜像地址仅在对应版本发布成功后可用。发布后检查 Package 的可见性和仓库访问权限；公开仓库不保证包自动公开。Work／Worker 镜像不在这条发布流程中。

## 发布步骤

1. 确认 `Cargo.toml` 的 workspace 版本与 `apps/ternilo-desktop/tauri.conf.json` 完全一致；同步变更后的正式文档。
2. 在经过检查的提交创建并推送对应 `v<版本>` 标签，例如 `v0.2.10`。工作流拒绝标签和程序版本不一致。
3. 等待检查、六个目标包、镜像验收和上传全部完成；失败必须处理后重跑，不以跳过门禁发布。
4. 工作流上传全部附件后自动公开 Release；检查其中的六个目标产物、`SHA256SUMS`、`SOURCE` 和 `server-image.txt`。`SOURCE` 记录 Ternilo 与 Linorun 提交，校验和覆盖实际附件。
5. 下载 `compose.server.yml` 可直接使用已发布的 Server 镜像，无需源码构建；`docker-compose.md` 与英文版提供初始化和启动步骤。未签名安装器始终明确标注，签名／公证及目标平台实机安装验收单独推进。

流水线不修改源码、不自动创建版本标签，也不更改用户部署。凭据、数据库和私有运行配置不得进入仓库或交付包。实际发布状态以对应版本的 Actions、Release 和 GHCR 记录为准，候选构建不等于公开发行。

维护者可在本地运行 `actionlint`、`python3 -m unittest discover -s scripts/ci -p '*_test.py'` 和 `python3 scripts/ci/server-image-smoke.py --image <候选镜像>`。镜像检查只创建并清理自己的临时容器和卷，不运行全局 Docker 清理。组件恢复入口见[交付包](release-packaging.md#客户端与-server-安装恢复验证)。
