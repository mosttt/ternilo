# Windows ARM64 发行产物

状态：原生 Windows ARM64 构建、程序架构／持久化／进程控制、Desktop 后台服务与 NSIS 安装器验收通过，纳入本次发行；公开附件已随 v0.2.0 发布。

GitHub 的[托管 runner 文档](https://docs.github.com/en/actions/reference/runners/github-hosted-runners)列出 `windows-11-arm`。Tauri 的[Windows 安装器文档](https://v2.tauri.app/distribute/windows-installer/)支持 `aarch64-pc-windows-msvc`，并说明 NSIS 安装器本身仍以 x86 仿真运行，应用主体是原生 ARM64。本阶段提供 ARM64 CLI／Server ZIP、Desktop NSIS EXE；不把它标成 x64 程序，也不承诺 ARM64 MSI。

产物矩阵集中在 `.github/package-targets.json`。手动包构建可选择一个目标，正常复用默认仍构建全部；未知目标直接拒绝，不将调用参数拼进 shell。Windows 产物验证增加 PE 架构校验，覆盖 Desktop、CLI、Server、插件工具和沙箱工具；Desktop 必须为 GUI 子系统，其他为控制台子系统。

ARM64 验收包含原生持久化、工作目录边界、子进程控制、Desktop 后台服务生命周期及实际安装器构建。编译通过、原生程序运行、安装器生成和用户物理电脑上的 GUI 安装是不同结果。签名／公证／自动更新属于独立工作，不从 ARM64 构建推导其已支持。

独立 GitHub 验证分支完成包工作流验收；本次 v0.2.0 候选包含后续已验证功能与界面修正，公开标签在最终提交准备好后创建。

2026-10-03：GitHub [原生 ARM64 验证](https://github.com/mosttt/ternilo/actions/runs/37123402210)全部成功，源码 `047d6fe`。实际生成客户端／Server ZIP 和 Desktop EXE；五个 Windows 程序 PE 架构均为 ARM64，后台服务和原生持久化／工作目录／子进程测试通过。
