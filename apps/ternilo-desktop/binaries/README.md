# Desktop sidecars

Windows 打包前，从源码根目录运行 `python scripts/prepare-windows-sidecar.py --target <target-triple>`，构建 release runner 并按 Tauri 的 external binary 命名规则放入本目录。目标和 Windows 编译工具需预先安装；生成文件不提交为源码。完整步骤见[桌面应用](../../../docs/desktop.md#windows-sandbox-sidecar)。

Before invoking Tauri on Windows, run `python scripts/prepare-windows-sidecar.py --target <target-triple>` from the repository root. It builds and stages the matching release runner here as `ternilo-sandbox-windows-<target-triple>.exe`. Install the Rust target and Windows build tools first. Generated executables are release artifacts, not source files.
