# Windows ARM64 发行产物

状态：构建矩阵与验证已编制，等待原生 Windows ARM64 runner 的实际结果；尚未发布或宣称支持完成。

GitHub 的[托管 runner 文档](https://docs.github.com/en/actions/reference/runners/github-hosted-runners)列出 `windows-11-arm`。Tauri 的[Windows 安装器文档](https://v2.tauri.app/distribute/windows-installer/)支持 `aarch64-pc-windows-msvc`，并说明 NSIS 安装器本身仍以 x86 仿真运行，应用主体是原生 ARM64。本阶段提供 ARM64 CLI／Server ZIP、Desktop NSIS EXE；不把它标成 x64 程序，也不承诺 ARM64 MSI。

产物矩阵集中在 `.github/package-targets.json`。手动包构建可选择一个目标，正常复用默认仍构建全部；未知目标直接拒绝，不将调用参数拼进 shell。Windows 产物验证增加 PE 架构校验，覆盖 Desktop、CLI、Server、插件工具和沙箱工具；Desktop 必须为 GUI 子系统，其他为控制台子系统。

ARM64 验收包含原生持久化、工作目录边界、子进程控制、Desktop 后台服务生命周期及实际安装器构建。编译通过、原生程序运行、安装器生成和用户物理电脑上的 GUI 安装是不同结果。签名／公证／自动更新属于独立工作，不从 ARM64 构建推导其已支持。

使用独立 GitHub 验证分支运行当前包工作流，不改写已冻结的 v0.2.0 候选，也不另建用户桌面的工作树。
