# 本地构建与交付包

Ternilo 可以从本地源码生成二进制交付包和 Docker 镜像。本指南不依赖公共下载站或预先发布的镜像。构建机需要 Ternilo、相邻的 Linorun、仓库固定的 Rust 工具链和 Node.js；具体布局见[本地快速开始](getting-started.md)。

通过[GitHub CI 与发行流程](ci-release.md)可以生成五平台客户端／Server 二进制与桌面安装器，验收并上传 Server 镜像到 GHCR。本指南提供本地组件打包和 Server／Worker 镜像构建命令。版本、包可见性、代码签名与平台验收由发行方管理；桌面自动更新保持关闭。

组件包包含中英文首页、正式指南、项目许可和第三方许可。桌面安装包在应用资源目录保留许可文件，容器镜像将其放在 `/usr/share/ternilo/`。重新分发时须保留 `LICENSE`、`THIRD_PARTY_NOTICES.md` 和 `licenses/`。

## 二进制包

先选择本次交付的组件。当前电脑上的网页、CLI 和可选远程连接使用 `local`，不需要同时构建 Server 或 Worker：

```bash
scripts/package-release.sh --component local --version 0.2.10 \
  --output-dir /path/to/releases
```

它只构建并打包 `ternilo` 与 `ternilo-plugin`，产物为 `ternilo-0.2.10-<系统>-<架构>.tar.gz`。`server` 只提供 `ternilo-server`，`worker` 只提供 `ternilo-worker`；不指定组件时默认 `all`，完整包以 `ternilo-all-` 命名，与客户端包区分。Windows 使用 `--binary-suffix .exe` 时生成 `.zip`，可直接由资源管理器解压；Linux／macOS 使用 `.tar.gz` 保留 Unix 文件权限。各模式只要求对应程序存在，归档内 `RELEASE` 记录组件和程序清单。

需要包含全部程序时，在 Ternilo 源码根目录执行：

```bash
scripts/package-release.sh --version 0.2.10 --output-dir /path/to/releases
```

脚本先安装前端依赖、构建 Web 和 release binaries，再生成 `ternilo-all-0.2.10-<系统>-<架构>.tar.gz` 与 `.sha256`。已存在的同名产物会拒绝覆盖。包内包含：

- `bin/ternilo`：电脑上的本地使用、远程连接和程序化入口。
- `bin/ternilo-server`：服务器的 setup／serve／admin 子命令。
- `bin/ternilo-worker`：平台隔离执行进程。
- `bin/ternilo-plugin`：插件构建、签名和验证。
- `deploy/docker/`：生产 Compose、初始化与运维脚本、无密钥的配置模板。
- `docs/zh-CN/` 与 `docs/en/` 正式文档、许可证和版本标记。
- 文档引用的 Profile、Rhai/WASM 扩展示例、WIT 接口以及 Python/TypeScript SDK 源码。

每种组件包都附带共享正式文档及其引用的示例、SDK 和部署参考资料；这些资料不表示包中包含其他组件的程序。远程入口需要另外安装 Server 或加载服务器镜像，桌面窗口需要另外安装同版本桌面包。`local` 可以完全独立使用。

交付包不会收录开发过程文档、运行中的 `.env`、secrets、数据库或备份。它不包含 Docker 镜像，也不生成桌面安装包。桌面应用按[桌面打包](desktop.md#打包和更新)在各自平台构建。

包根目录的 `START-HERE.md` 和英文版解释三个客户端程序的用途，亦见[程序清单](binaries.md)。解压后可将包内 `bin/` 加入 `PATH`，直接使用所选组件包含的程序。示例和 SDK 均保留文档中的相对路径。运行扩展示例中的签名命令使用 `ternilo-plugin`；构建 WASM 示例仍需 Rust 与对应 WASM target。

包内维护文档的源码编译、仓库门禁及桌面构建步骤须在完整 Ternilo 与 Linorun 源码中执行；交付包不包含 Cargo workspace、Web 源码或开发测试环境，不应在解包目录执行这些仓库命令。运行现成程序、部署 Compose、使用配置示例或 SDK 无需获取项目源码。

已有经过验证的本机 release binaries 时，可以只打包：

```bash
scripts/package-release.sh --component local --version 0.2.10 \
  --no-build --bin-dir /path/to/verified-binaries \
  --target-name linux-x86_64 --output-dir /path/to/releases
```

`--target-name` 只命名产物，不做交叉编译或兼容性保证。应在目标系统及架构构建、启动验证；文件搜索需要 `ripgrep`，Linux 本地命令执行另需 `bubblewrap`。这两项不内嵌在 Local 归档或 AppImage 中；Linux `.deb` 声明对应依赖，Docker 运行镜像已安装它们。脚本使用 POSIX shell；Windows CI 通过 Git Bash 执行并传入 `--binary-suffix .exe`，Local 包会检查并携带同次构建的 sandbox runner，不能只复制主程序。桌面安装包由目标平台的 Tauri 构建入口生成。

| 参数 | 环境变量 | 默认值 |
|---|---|---|
| `--version` | `TERNILO_RELEASE_VERSION` | 必填 |
| `--component` | `TERNILO_RELEASE_COMPONENT` | `all`；可选 `local`、`server`、`worker`、`all` |
| `--output-dir` | `TERNILO_RELEASE_OUTPUT_DIR` | 源码根目录 `dist/` |
| `--bin-dir` | `TERNILO_RELEASE_BIN_DIR` | `target/release/` |
| `--target-name` | `TERNILO_RELEASE_TARGET` | 当前系统和架构 |
| `--binary-suffix` | `TERNILO_RELEASE_BINARY_SUFFIX` | 空；Windows 使用 `.exe` |
| `--build` / `--no-build` | `TERNILO_RELEASE_BUILD` | `true` |

命令行优先。打包本身不等于通过发行验收：每种组件都应使用实际 release 程序验证相应运行流程，debug 验证包不能作为正式发行物。Server 镜像的测试与发布门禁见[可复现 release gate](deployment.md#可复现-release-gate)。

## Docker 镜像

生产 Compose 默认使用 `ghcr.io/mosttt/ternilo-server:0.2.10`，直接拉取即可，[部署流程](docker-compose.md)不包含构建步骤。仅开发自定义镜像时使用源码构建 override，并显式选择自己的镜像名：

```bash
TERNILO_IMAGE=ternilo-server:custom-0.2.10 docker compose --env-file deploy/docker/.env.server.example \
  -f deploy/docker/compose.server.yml \
  -f deploy/docker/compose.server.build.yml build server
```

上述命令产出本机的 `ternilo-server:custom-0.2.10`，只包含 Server。Worker 使用独立镜像：

```bash
docker compose --env-file deploy/docker/.env.worker.example \
  -f deploy/docker/compose.worker.yml \
  -f deploy/docker/compose.worker.build.yml build worker
```

默认产出 `ternilo-worker:0.2.10`。Worker 镜像不构建或携带网页，部署时只接收 Server 地址和独立凭据。修改版本可通过 `TERNILO_IMAGE` 显式提供。Linorun 不在同级目录时用 `TERNILO_LINORUN_SOURCE` 指定绝对目录。上述 build override 用于源码检出；Server 二进制交付包直接拉取公开镜像；自定义和 Worker 镜像使用自己构建或加载的版本。

Rust 构建并行度可通过 `build --build-arg CARGO_BUILD_JOBS=<数量>` 按构建机资源指定。依赖下载连接数可通过 `--build-arg NPM_CONFIG_MAXSOCKETS=<数量>` 按构建网络调整；两项均无项目固定默认值，未设置时沿用工具默认值。npm 与 Cargo 下载使用构建缓存，发行包目录不进入 Docker 源码上下文。

没有 registry 时，在构建机导出：

```bash
docker image save --output /path/to/releases/ternilo-server-0.2.10-image.tar ternilo-server:0.2.10
docker image save --output /path/to/releases/ternilo-worker-0.2.10-image.tar ternilo-worker:0.2.10
```

将镜像文件和二进制交付包复制到服务器，加载镜像：

```bash
docker image load --input /path/to/releases/ternilo-server-0.2.10-image.tar
docker image load --input /path/to/releases/ternilo-worker-0.2.10-image.tar
```

然后按[Docker 部署](deployment.md)初始化 Server，按[Worker 部署](worker.md)初始化 Worker，分别指定对应镜像。也可以推送到自己管理的 registry，并在部署配置中固定版本或 digest；不要把 `latest` 当作可复现的发行版本。

## 完整发行门禁

```bash
deploy/docker/release-gate.sh \
  --server-image ternilo-server:release-candidate \
  --worker-image ternilo-worker:release-candidate \
  --test-image ternilo-acceptance:release-candidate \
  --artifact-dir ./release-artifacts
```

对应环境变量为 `TERNILO_RELEASE_SERVER_IMAGE`、`TERNILO_RELEASE_WORKER_IMAGE`、`TERNILO_ACCEPTANCE_TEST_IMAGE` 和 `TERNILO_RELEASE_ARTIFACT_DIR`。源码不在同级目录时使用 `--linorun-source`／`TERNILO_LINORUN_SOURCE`。

门禁分别构建两个生产镜像，并检查原生及容器浏览器、SQLite／PostgreSQL 的匹配恢复和凭据轮换。`acceptance` 只是与生产镜像使用相同系统库的 Rust 环境合同运行器，不进入用户部署，也不包含在双生产镜像元数据中。门禁脚本本身的单元测试通过，不代表整条实际发行门禁已经执行。

完整成功后，门禁输出各步骤日志、`gates.txt`、记录两个生产镜像准确 ID 的 `release.json` 和 `SHA256SUMS`。这条门禁不生成二进制交付压缩包或桌面安装器；这些产物仍需分别构建和验收。

验收必须对应实际程序和协议。当前执行器协议为 `48`，Control schema 为 `14`。客户端／Server 的安装恢复可以独立验证，不必先启动 Worker；Docker 双镜像门禁、优化 release、桌面安装器、其他操作系统、物理手机、PostgreSQL 备份恢复与 Worker 活动任务恢复仍须分别验收，不能由一次本机检查代替。

## 客户端与 Server 安装恢复验证

仓库提供独立的安装恢复验收脚本，不混入普通 Web 回归，也不自动构建、发布或重启日常服务。它需要 POSIX 环境、`tar`、Python 3、可运行 TypeScript SDK 的 Node.js、已安装的 Web 开发依赖和 Playwright Chromium；浏览器安装在其他位置时可用 `TERNILO_BROWSER_EXECUTABLE` 指定。

在完整源码根目录提供同次构建的程序绝对路径，内嵌网页必须与 `web/dist` 一致。例如：

```bash
TERNILO_E2E_NODE_BINARY=/path/to/verified-binaries/ternilo \
TERNILO_E2E_SERVER_BINARY=/path/to/verified-binaries/ternilo-server \
TERNILO_E2E_PLUGIN_BINARY=/path/to/verified-binaries/ternilo-plugin \
TERNILO_E2E_ARTIFACT_DIR=/path/to/verification-results \
node --test web/tests/delivery-restore-acceptance.test.mjs
```

脚本用指定程序生成标为 `installation-validation` 的 Local／Server 包，核对校验和与解包后二进制，在独立安装目录及临时 HOME／数据目录运行。验证包内 Python／TypeScript SDK 的真实文件操作、Rhai 示例的签名／验证、初始化拒绝覆盖、模型凭据、电脑接入，以及在线 SQLite 快照和停机完整归档分别恢复。模型服务仅访问临时回环测试上游，不读取真实凭据或产生付费调用。

恢复检查包括账号与资源身份、模型逐项配置和推理强度、模型授权设备限制、正常与归档历史，以及 Local／Server 网页重新执行任务。进程重启按现有安全合同暂停队列，不自动重跑旧任务；用户再次提交与自动续跑是不同操作。桌面与 390／320px 浏览器截图、网络结果写入指定证据目录，临时服务、数据、备份和安装包结束后清理。脚本不会迁移旧 schema，也不是跨版本迁移工具。

指定 debug 程序时只能证明当前构建的安装／恢复流程，不能称为正式发行验收；最终 release 程序还应单独运行并保留结果。`Checks` 复用此入口，不覆盖 Worker、桌面安装器和真实手机。

PostgreSQL 另有独立匹配恢复检查，需要 Docker 和 PostgreSQL 17 镜像：

```bash
TERNILO_E2E_NODE_BINARY=/path/to/verified-binaries/ternilo \
TERNILO_E2E_SERVER_BINARY=/path/to/verified-binaries/ternilo-server \
TERNILO_E2E_ARTIFACT_DIR=/path/to/postgres-verification-results \
node --test web/tests/postgres-restore-acceptance.test.mjs
```

该检查创建独立容器、schema-owner 和受限 runtime，停 Server 后导出 custom-format 转储并与完整配置配对，重新导入新数据库，再验证身份、原 Node 凭据、私有资源、模型密钥／限制／账本及网页文件任务。源数据库保持不变；结束后只删除本次容器和临时数据。可用 `TERNILO_E2E_POSTGRES_IMAGE` 指定兼容镜像、`TERNILO_DOCKER` 指定 Docker 程序，不接受日常数据库地址。

每次发行保留与实际程序对应的构建信息、校验和和验收结果。Windows sidecar 准备脚本及 ICO／ICNS 资源不能代替各系统安装器、签名、上传和平台验证；PWA 安装资格检查也不能代替真实设备上的安装与使用。
