# 下载包中的程序

[English](../en/binaries.md)

客户端产品和启动命令都叫 `ternilo`，客户端压缩包统一使用 `ternilo-<版本>-<平台>`。`ternilo-local` 是 Rust 内部模块和旧压缩包的名称，不是另一个客户端。Windows 便携包为 ZIP；Linux／macOS 命令行包为 tar.gz。桌面安装器分别是 Windows EXE／MSI、macOS DMG、Linux DEB／AppImage。

| 程序 | 用途 | 普通用户怎样使用 |
|---|---|---|
| `ternilo` / `ternilo.exe` | 本机客户端，执行任务并提供网页入口，也可连接 Server | 直接启动，或运行 `ternilo serve` |
| `ternilo-plugin` / `ternilo-plugin.exe` | 扩展开发工具，生成发布者密钥、封装 WASM、签名和验证 Rhai／WASM 扩展 | 普通使用不需要启动；开发扩展时运行 `ternilo-plugin --help` |
| `ternilo-sandbox-windows.exe` | Windows 隔离执行辅助程序，使用受限权限运行工具并回收子进程树 | 保留在 `ternilo.exe` 同一目录，由主程序自动调用 |
| `ternilo-server` / `ternilo-server.exe` | 远程入口，提供账号、机器连接、协作和模型服务 | 单独下载 Server 包，或使用公开 Docker 镜像 |

解压便携包后进入 `bin` 目录，或者将整个 `bin` 目录加入 PATH。Windows 不要只移动 `ternilo.exe` 而留下沙箱辅助程序，否则需要隔离执行的工具会报告缺少运行器。它不需要独立启动、设置端口或连接 Server；Windows 当前不承诺完整读取隔离。

扩展开发示例命令：

```sh
ternilo-plugin --help
ternilo-plugin sign --help
ternilo-plugin verify --bundle extension.json --publisher publisher.json
```

`verify` 检查签名、内容摘要、宿主限制及运行时编译，不执行安装。安装和信任发布者在 Ternilo 扩展设置中完成。插件签名私钥属于开发者机密，不应放入扩展包或上传仓库。

客户端包附带插件 CLI 是为了方便扩展开发；日常工作只运行主程序。桌面安装器已经负责放置自身需要的程序，不需要把便携包再手动复制进去。
