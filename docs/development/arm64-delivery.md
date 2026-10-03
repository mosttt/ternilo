# Linux ARM64 发行

2026-10-03：新增原生 `ubuntu-24.04-arm` 检查、CLI／Server 归档、Desktop DEB／AppImage，以及在同架构 Runner 上独立验收的 Server 镜像。工作流源码已实现，实际远端构建和公开附件验收尚未完成。

Server amd64／arm64 分别执行已有初始化、登录、嵌入 Web、非 root、只读根目录和重启持久化检查。仅这些通过验证的 image ID 才能进入发布；将两份推送摘要组合成版本索引并核对平台集合。Compose 无需本地构建或指定固定架构。

实现核对 GitHub [Runner 架构文档](https://docs.github.com/en/actions/reference/runners/github-hosted-runners) 与 Tauri [AppImage 的 ARM Runner 说明](https://v2.tauri.app/distribute/appimage/)。不以跨编译成功代替原生执行测试；物理设备的桌面安装体验仍需独立验收。Windows ARM64、发行者签名／公证和自动更新不属于本条已实现结果。
