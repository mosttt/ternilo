# 桌面应用

桌面应用是同一工作台的原生窗口。浏览器、桌面和远程连接可以同时使用同一个本地服务，文件、Provider、插件和会话只有一份。

## 运行

安装桌面包后直接打开应用。从源码运行时先构建 Web：

```bash
npm --prefix web ci
npm --prefix web run build
cargo run --locked -p ternilo-desktop
```

桌面按数据目录查找已运行的本地服务，并校验认证、实例身份和版本。已有 `ternilo serve` 时直接连接其实际地址；为使用原生目录选择器等能力，共享的服务须监听 `127.0.0.1`；没有服务时，桌面使用自己的可执行文件启动独立后台进程，再打开窗口。无需额外安装一个 Node binary。

因此桌面与浏览器可以同时使用 `127.0.0.1:3210`：只有后台服务监听端口，桌面是它的客户端。使用不同数据目录时不会复用该服务，需指定另一个空闲端口或 `127.0.0.1:0`，不能让两个独立服务绑定同一地址和端口。

**关闭桌面窗口不会停止后台服务。** 任务和远程连接可继续运行；再次打开应用会连接原服务。后台服务由桌面启动时，日志位于数据目录的 `service.log`。

### 查看与停止后台服务

按当前配置生成的桌面包不包含独立的 `ternilo` 管理命令；Windows 另需下文所述的 sandbox sidecar。管理命令由同版本的本地命令行交付包提供。先按[本地快速开始](getting-started.md#1-启动程序)准备 `ternilo` 并加入 `PATH`，再使用相同系统用户、相同数据目录运行：

```bash
ternilo status --data-dir /path/to/ternilo-data
ternilo stop --data-dir /path/to/ternilo-data
```

默认目录可以省略 `--data-dir`。`status` 显示实例 ID、版本、PID、实际监听地址、网页和远程功能状态，不显示 API token。`stop` 请求优雅关闭并等待该实例完成应用收尾和数据目录释放，运行中的任务也会停止。成功返回后可以重新启动；超过 30 秒仍未完成时明确报告正在关闭，不会把请求已接收误报为完成。

不重复启动第二个服务。已有服务的监听地址、Profile、远程连接和运行上限继续生效，打开桌面不会用新窗口参数覆盖它们。需要调整时先停止服务，再以新参数启动。桌面与服务版本不一致时同样需要重启对应版本。

### 参数与环境变量

| 命令行参数 | 环境变量 | 默认值或格式 |
|---|---|---|
| `--listen ADDRESS` | `TERNILO_DESKTOP_LISTEN` | 新建服务时使用 `127.0.0.1:3210`，IP 必须为 `127.0.0.1` |
| 可重复的 `--profile PATH` | `TERNILO_DESKTOP_PROFILES` | 新建服务时的 Profile 层；环境变量用逗号分隔 |
| `--data-dir PATH` | `TERNILO_DESKTOP_DATA_DIR` | 与 `ternilo serve` 相同的默认目录 |

CLI 优先于环境变量。已有服务时，桌面从实际服务信息获得端口；`--listen` 与 `--profile` 只用于没有服务时的首次启动。内部 `--service`、smoke-test instance 和系统 deep link 不属于普通部署选项。

开发时可用独立数据目录和随机端口：

```bash
cargo run --locked -p ternilo-desktop -- \
  --listen 127.0.0.1:0 \
  --data-dir /path/to/isolated-data
```

如果服务以 `ternilo serve --no-local-web` 启动，后台仍保留认证后的应用与管理 API，但没有网页资源。桌面会提示先重启服务并移除该参数，不会私自启用界面或另开一份状态。

## 远程连接与后台生命周期

需要电脑同时接受手机操作时，先按[远程访问指南](remote-access.md)运行带 `--gateway-url` 的 `ternilo serve`，再打开同一数据目录的桌面应用。远程配置由后台服务拥有，桌面关闭不会关闭它。

服务是该目录中 `LocalApplication` 的唯一写入者，桌面窗口只通过 HTTP API 操作。`rpc` 和 `acp` 目前仍自行拥有持久应用，不能与该服务并发打开相同目录，详见[自动化接口](automation.md)。

## 原生能力

工作区、会话、模型、工具和设置通过同一 loopback API。Tauri IPC 仅提供：

| command | 用途 | 边界 |
|---|---|---|
| `pick_workspace` | 系统目录选择器 | 返回 canonical directory，不提供通用文件 API |
| `desktop_notify` | 失焦时通知任务完成或问题待回答 | 标题最多 80 字符、正文最多 240 字符 |
| `desktop_initial_links` | 冷启动 deep link | 将 URL 交给共享前端解析 |

Linux 使用 XDG Desktop Portal，Windows/macOS 使用系统 dialog。桌面只连接经过认证的 loopback 服务；Tauri capability 只授权这些原生命令与事件。普通浏览器和远端页面不能调用 Tauri IPC。

桌面窗口不显示 WebView 默认右键菜单，应用自身的菜单与复制、粘贴等快捷键保留；普通浏览器的右键菜单不受影响。滚动条与共享网页使用同一主题，初始及闲置时隐藏，用户滚动或移动指针进入可滚动区域时显示。

桌面启动锁只协调服务发现与创建，应用数据锁由后台服务持有。WebView 导航前会验证服务身份和版本。桌面窗口创建失败不会自动停止已复用的后台服务。

## Deep link 与单实例

工作区链接格式：

```text
ternilo://workspace?path=<percent-encoded-absolute-path>
```

例如：

```bash
cargo run --locked -p ternilo-desktop -- \
  'ternilo://workspace?path=%2Fprojects%2Fexample'
```

冷启动时链接由桌面 IPC 读取；应用已运行时，第二实例把参数交给首实例后退出。首实例聚焦窗口，并由共享 host canonicalize 和登记工作区。未知 action 或缺少 `path` 的链接不会执行。

## Windows sandbox sidecar

Windows 的 sandbox 由与主程序同目录的 `ternilo-sandbox-windows.exe` 提供。开发时先构建 sidecar：

```powershell
python scripts/prepare-windows-sidecar.py --target x86_64-pc-windows-msvc
cargo run --locked -p ternilo-desktop
```

缺少 runner 时，`ReadOnly` 和 `WorkspaceWrite` shell/terminal 会关闭式拒绝，不会退回裸进程。发布构建要按 Tauri external binary 命名规则把它放为：

```text
apps/ternilo-desktop/binaries/ternilo-sandbox-windows-<target-triple>.exe
```

`tauri.windows.conf.json` 只在 Windows bundle 加入该 sidecar。在 Windows 的源码根目录用 Python 3 准备匹配目标的 release runner：

```powershell
python scripts/prepare-windows-sidecar.py --target x86_64-pc-windows-msvc
cd apps/ternilo-desktop
cargo tauri build --target x86_64-pc-windows-msvc
```

不指定 `--target` 时依次使用 `CARGO_BUILD_TARGET` 或 rustc 的主机目标。脚本通过 Cargo 实际报告的可执行文件路径复制产物，支持自定义 `CARGO_TARGET_DIR`，不猜测固定的 `target/release`；构建失败或没有对应产物时不会用旧文件冒充成功。Rust 目标和 Windows SDK／编译工具仍需预先安装，脚本不安装交叉编译环境。开发版 runner 已构建不代表 release bundle 已带齐文件，生成文件不提交仓库。各平台 sandbox 语义见 [安全模型](security.md#本地进程-sandbox)。

## 打包和更新

安装 Tauri CLI 后，从仓库根目录构建共享 Web，再生成安装包：

```bash
npm --prefix web run build
cd apps/ternilo-desktop
cargo tauri build
```

Tauri 包只携带 `web/dist` 作为最小前端资源；运行时仍由与浏览器端相同的 Salvo adapter 提供该构建，不维护第二份前端。

Linux `.deb` 声明 `bubblewrap` 运行依赖；AppImage 与 CLI 归档仍要求系统安装它。没有可用沙箱时不会静默退回未隔离的命令执行。

打包配置显式包含 PNG、Windows ICO 和 macOS ICNS 图标。更新已有 PNG 图案后，在源码根目录运行 `python3 scripts/prepare-desktop-icons.py` 重新生成 ICO／ICNS；脚本只封装已有图像，不调用图像生成服务。生成的两份图标与 PNG 一起作为源码资产提交，Windows sidecar 可执行文件仍是单独的构建产物。

Tauri 已配置启用 bundle；`Build packages` 在 Linux x86_64、Windows x86_64、macOS Apple Silicon 与 Intel runner 构建原生安装器，Windows 先准备同目标 sidecar，详见[CI 与发行](ci-release.md)。默认未接入发行者签名／公证，也尚无全部平台正式安装器验收记录。各平台仍需实际安装／卸载、启动和签名检查，不能以 Linux 本机或浏览器测试代替。

源码配置保持 `createUpdaterArtifacts = false`。在启用自动更新前，发行方必须提供：

1. 离线 updater 私钥和编译进应用的公钥；
2. HTTPS release endpoint 与单调版本 channel manifest；
3. 至少一个仍受信的旧版本用于回滚；
4. 平台安装包签名，以及冷/热 deep-link、通知和 sandbox smoke。

缺少生产下载域名或公钥时保持 updater 关闭是安全默认。测试与发布命令见 [开发与发布](contributing.md)。
