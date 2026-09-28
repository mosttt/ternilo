# 开发与发布

本文面向维护 Ternilo 源码、构建发行物和运行仓库门禁的人。普通本地使用见 [快速开始](getting-started.md)，生产环境见 [部署文档](deployment.md)。

## 工具链与源码布局

Ternilo workspace 使用 Rust edition 2024，最低/固定工具链为 Rust 1.98.0。`rust-toolchain.toml` 包含 `rustfmt` 与 `clippy`。[Linorun 源码](https://github.com/mosttt/linorun)是相邻路径依赖；发行构建应固定与 Ternilo 配套的 commit 或 tag，不直接跟踪移动分支：

当前固定提交见 `.github/linorun-revision`，CI 与发行共用它。更新提交前检查该版本的接口、`Cargo.lock` 和相关测试，不将开发者机器上的未提交修改当作 CI 输入。

文件搜索通过外部 `ripgrep` 执行，运行程序和相关集成测试前应确认 `rg --version` 可用。Linux 受限命令执行还需要 `bubblewrap`；CI 在一次性 runner 中显式安装这些运行依赖。

```text
/path/to/source/
├── linorun/
└── ternilo/
```

从 Ternilo 仓库根目录执行：

```bash
rustc --version
npm --prefix web ci
npm --prefix web run build
cargo build --locked -p ternilo -p ternilo-server -p ternilo-worker
```

前端源码位于 `web/`。Markdown 渲染器由 Vite 从 `web/rich-text.source.js` 直接编入共享 Web bundle；Local 和 Server binary 嵌入 `web/dist` 中的页面、JS、CSS 与 PWA 版本，同时直接嵌入 `web/public` 中的 manifest、图标和 `web/service-worker.js`：

```bash
npm --prefix web ci
npm --prefix web run build
```

修改富文本依赖或入口后重新执行同一构建即可；仓库不维护第二份手工生成的富文本 bundle。

## 按修改范围验证

先运行受影响 package、前端组件或脚本的测试。例如：

```bash
cargo test --locked -p ternilo-kernel
npm --prefix web run test:unit -- src/domain/theme.test.ts
python3 deploy/docker/tests/deployment-tools.test.py
npm --prefix web run verify:docs
node web/scripts/verify-formal-docs.mjs --include-development
```

根据实际改动选择命令，不要求每次执行所有领域。前端变更还需要对应真实浏览器流程。共享内核、跨路径或发行候选再扩大验证：

```bash
cargo fmt --all -- --check
cargo test --locked --workspace --all-targets
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
RUSTDOCFLAGS='-D warnings' cargo doc --locked --workspace --no-deps
cargo test --locked --workspace --doc
npm --prefix web test
```

部署 shell 语法检查须逐文件执行，不能把通配符展开的多个文件误当成 `sh -n` 的多个输入：

```bash
for script in deploy/docker/*.sh deploy/docker/tests/*.sh scripts/*.sh; do
  sh -n "$script" || exit
done
```

Linorun 是独立库；集成其更新前阅读相邻仓库的当前规范与验证要求，按其公共 API 变化执行相关集成测试。Ternilo 某次测试通过不代表 Linorun 所有语义和平台已经验收。

## 浏览器和桌面端

```bash
npm --prefix web run test:browser
npm --prefix web run test:desktop-smoke
# After building the current Web and the ternilo / ternilo-server binaries:
node --test web/tests/server-accounts-nodes-browser-e2e.test.mjs
node --test web/tests/server-oidc-link-browser-e2e.test.mjs
```

这些流程使用真实应用和 Chromium：

- 本地浏览器覆盖工作区、布局、Provider／Key、真实 Bearer/SSE 模型链、预设、审批、附件、Skills、Plan、Workflow、PWA 与可访问性。
- Server 账号／Node 流程启动独立 SQLite Server 和三台临时电脑，覆盖原生设置／邀请／模式往返、同名本地 ID 的私有历史隔离、共享权限／撤销、附件、真实计划审批和移动布局。
- OIDC 绑定流程使用真实签名、JWKS 与 PKCE 跳转，验证显式绑定保持原生登录和同一 user_id，退出后组织登录仍是同一管理员。
- 桌面 smoke 使用真实 Tauri window 和临时 data/XDG，需要图形会话或虚拟显示。发行验收还要检查已有服务复用、后台冷启动、关闭窗口继续运行、版本／认证不匹配、headless 拒绝、停止与清理；不能以单项 deep-link smoke 替代全部生命周期。

复验现成 release 时，可设置 `TERNILO_E2E_NODE_BINARY`、`TERNILO_E2E_SERVER_BINARY` 和 `TERNILO_E2E_DESKTOP_BINARY` 指向实际程序；桌面生产模式与 Tauri CLI 一样启用 `tauri/custom-protocol`。PWA 验收使用临时持久浏览器配置，不用隐身上下文判断安装资格。桌面和 PWA 夹具清理继承的 `TERNILO_*` 运行配置，使用独立 XDG 目录，不借用日常连接或状态。组件包与两库恢复使用[独立验收入口](release-packaging.md#客户端与-server-安装恢复验证)，不会因运行普通 Web 测试而启动数据库容器。

Worker 的模型代理、隔离执行、用量和恢复有各自的真实浏览器及进程验证。公共协议变化时选择相关流程，不能仅以模拟接口或脚本语法检查代替实际运行。

浏览器测试使用临时 data/workspace/database，结束时清理；不得改成使用默认用户数据目录。包含 secret 的断言必须同时检查输入清空、页面/DOM 不含明文、Provider JSON 不含明文以及模型 fixture 收到正确 Bearer。

## PostgreSQL 集成测试

Control 与 Cloud 的 PostgreSQL 环境测试会删除并重建 `public` schema，只能连接名称包含安全标记的一次性数据库：

```bash
TERNILO_TEST_DATABASE_URL='postgres://postgres:<password>@127.0.0.1:55432/ternilo_control_test' \
  cargo test --locked -p ternilo-control --test postgres -- --ignored --nocapture --test-threads=1

TERNILO_CLOUD_TEST_DATABASE_URL='postgres://postgres:<password>@127.0.0.1:55432/ternilo_cloud_test' \
  cargo test --locked -p ternilo-cloud --test postgres -- --ignored --nocapture --test-threads=1
```

测试代码会检查连接 URL 是否包含对应的 `ternilo_control_test` 或 `ternilo_cloud_test` 标记；调用方仍须确认实际目标是一次性数据库。同一数据库上的测试须串行运行，不能同时启动多个会重建 schema 的测试进程。Control 用例覆盖非 owner RLS、RBAC、enrollment、quota、secret、audit 和 master-key 轮换；Cloud 的 PostgreSQL 集成用例分布在 `crates/ternilo-cloud/tests/` 的多个入口，覆盖 queue／lease／fencing、事件、usage、取消／reap、共享／附件／审批、Agent Team、遥测和签名 Extension，上述 `--test postgres` 只运行其中一个入口。受限 PostgreSQL runtime 无 DDL，不能以 superuser 测试替代 RLS 合同。

## Docker 验证

生产部署与源码构建分开。先构建明确版本的 Server 镜像：

```bash
TERNILO_IMAGE=ternilo:release-candidate docker compose \
  --env-file deploy/docker/.env.server.example \
  -f deploy/docker/compose.server.yml \
  -f deploy/docker/compose.server.build.yml build server
```

在独立目录使用 `ternilo-deploy init --image ternilo:release-candidate`，按[部署文档](deployment.md#备份恢复升级与轮换)验证：

- SQLite 默认初始化，以及外部 PostgreSQL 的同一账号、资源和共享行为；
- 原生账号／邀请／可选 OIDC，真实 Node enrollment、连接、浏览器任务与重连；
- `/livez`、实际写入探针 `/readyz`、无秘密内容的 `/metrics`；
- Tini PID 1 仅保留初始化和信号转发需要的 capability，业务进程 UID 10001、NoNewPrivs、清空 capability；
- 停机数据卷备份／checksum／新空卷恢复，PG dump 与匹配配置一起保存；
- 主密钥事务轮换、未完成轮换恢复保护，以及恢复后原账号／共享／凭据和实际任务。

只有当前候选镜像的结果能用于该发行物验收。其他版本镜像和 fake Docker 单元测试不能替代实际部署闭环。Worker 的 child 隔离、文件持久化、配额／租约和恢复需要单独验收，不把它当作 Server 启动的前置依赖。

测试结束后只清理本次创建的容器、网络、卷和临时凭据；不得运行会删除其他项目资源的全局清理。

## 依赖与许可证

```bash
cargo audit
cargo deny check
cargo tree -i rsa
npm --prefix web audit --audit-level=high
```

正式发布必须联网刷新 RustSec 和 npm 数据。`cargo tree -i rsa` 应报告没有匹配 package；OIDC JWT 使用 AWS-LC provider。新增 advisory 例外、许可证或非 registry source 必须在 `deny.toml` 中留下精确理由，并更新第三方归属（如适用）。详细策略见 [安全模型](security.md#依赖与许可证门禁)。

## 源码与运行产物

Git 忽略构建目录、依赖、数据库、运行配置、私有环境文件和浏览器测试报告；示例环境文件、测试夹具及供程序内嵌的 `web/dist` 保留在版本控制中。自定义名称的本机配置不要加入仓库，可放到仓库外或 `.local/`，必要时在本机 `.git/info/exclude` 精确排除。

Docker 构建上下文只接收声明的源码目录和构建输入，再排除其中的缓存、凭据及运行数据；仓库根目录临时文件或任意命名的实例配置不会被 `COPY .` 带入构建层。新增顶层构建输入时同步更新 `.dockerignore`，不能为方便构建而开放整个运行目录。Linorun 是独立构建上下文，只复制其 manifest、README 和 `crates/`，不复制根目录构建缓存；源码目录仍不应存放运行凭据。

## 文档维护

文档按用户任务组织，避免按一次开发迭代拆文件：

- 首次使用放在 `getting-started.md`，日常会话放在 `user-guide.md`；模型、设置、文件、工具和协作分别维护独立指南；
- Server 连接电脑与手机访问放在 `remote-access.md`；
- Docker 与运维命令放在 `deployment.md`，本地制品生成放在 `release-packaging.md`；
- Server 接口、事务和执行协议放在 `server-reference.md`；
- 插件配置放在 `extensions.md`，签名扩展开发放在 `extension-packages.md`；
- 信任边界放在 `security.md`。

新增、重命名或删除文档后运行 `npm --prefix web run verify:docs`；门禁检查根目录各语言 README、`THIRD_PARTY_NOTICES.md` 与 `docs/` 下除 `development/` 外的 Markdown 文档中的相对链接、标题锚点和开发机绝对路径。涉及技术设计时使用 `node web/scripts/verify-formal-docs.mjs --include-development`。示例 README 和 `deploy/docker/secrets/README.md` 需另行核对，命令语义也需人工复核。示例路径使用 `/path/to/...`、`/projects/...` 或“仓库根目录”。部署域名、OIDC、密钥和账号必须明确是替换值，不能作为可用默认值。

用户文档描述产品行为、能力限制和部署输入。接口与架构直接维护在对应指南中；`docs/development/` 保存开发进度、验证结果、待办与技术设计，并明确标注状态。删除或移动页面时同步维护索引和引用。

根目录与文档首页默认使用英文，`README.zh-CN.md` 提供对应的简体中文入口，两种语言的页面须互相链接。详细指南当前使用简体中文，英文首页明确标注链接目标的语言。验证命令保持可重复，执行结果与产物标识由 CI／发行记录保存。

## 平台发布

仓库工作流按检查、打包、发布拆分，固定 Linorun 提交并分别限制权限；触发方式、平台矩阵、SHA256 和 GHCR 行为见[CI 与 GitHub 发行](ci-release.md)。版本标签由维护者创建，公开 Release 前须审阅对应产物与验收结果。

Linux、Windows 与 macOS 安装包必须分别在真实平台 runner 构建和 smoke。通用 Linux 检查不能代替：

- Windows restricted-token/ACL/Job Object sidecar 的读写边界、stdin 与进程树回收；
- macOS Seatbelt、目录 dialog、通知和签名；
- 各平台代码签名、安装/卸载、deep link、单实例和 updater artifact 验证。

本地二进制与部署资料包可用 `scripts/package-release.sh` 生成，步骤见[交付包](release-packaging.md)；打包成功不等于运行和平台验收通过。

桌面 updater 的生产公钥、release endpoint 和回滚 channel 在配置完成前保持关闭。打包步骤见 [桌面应用](desktop.md#打包和更新)。
