# v0.2.0 公开发行验收

2026-10-04：状态为已公开发行并完成本轮交付核对。[Release](https://github.com/mosttt/ternilo/releases/tag/v0.2.0) 于 05:10:35 UTC（北京时间 13:10:35）公开，当前标记为最新发行；[完整发行工作流](https://github.com/mosttt/ternilo/actions/runs/37174492575)成功。来源提交 f81afb7cb7056cd331da6ffb64ef2120e1214ae8，固定 Linorun 为 46cca18d63d1bef1c5475ac6912f18f5426a115b。

## 产物与校验

共 40 项附件，包含 12 个客户端／Server 便携归档及其 12 个独立校验文件、9 个桌面安装器，以及 SOURCE、SHA256SUMS、Compose、中英文部署说明和两份镜像元数据。平台为 Linux x86_64／ARM64、Windows x86_64／ARM64、macOS Apple Silicon／Intel；Windows 使用 ZIP，其余便携程序使用 tar.gz。桌面为两组 DEB／AppImage、Windows x64 EXE／MSI、Windows ARM64 EXE、两种 DMG。

无认证请求取得公开 Release 和全部小型附件。40 项清单与 GitHub 上传摘要、总 SHA256SUMS 和 12 个独立校验文件一致，SOURCE 的两个源码提交与固定输入一致。实际下载的小型附件逐份计算 SHA-256；大型程序附件的摘要在这项核对中使用 GitHub 提供的上传摘要，不将其写成本机已完整下载全部安装器。

本次 CI 的六目标程序版本运行、安装器构建、Desktop 后台服务验证均通过。Windows 两种架构验证五个实际 EXE 的 PE 架构与 GUI／控制台子系统；ARM64 增加原生持久化、目录边界和进程监督测试。Linux 原生桌面、真实浏览器、双库及安装器恢复等完整检查通过。构建与后台服务验收不等于所有物理设备的 GUI 安装。

## Server 镜像与 Compose

公开镜像 ghcr.io/mosttt/ternilo-server:0.2.0 的索引摘要为 sha256:460bc9384ca413facceb57798c2855e347bd38b9de145b565d9e46d49d4bcfde。linux/amd64 摘要为 sha256:e5e05e7982ac9da5cc74131396c3437e8f780e13dd82332c2cdd9356eb1d1bc6，linux/arm64 为 sha256:5c105066416aef177ac475106d3f8f837ae41753a1958374ab9706abc8c66c2d。

使用空的独立 Docker 凭据目录完成匿名拉取 amd64 镜像，检查架构、版本、源码和 Apache-2.0 标签；匿名 registry manifest 与发布的 server-image-index.json 完全一致。两个架构的镜像均在各自原生 Runner 验收，发布复用相同镜像 ID／摘要；本机未运行 ARM64 镜像。

实际使用公开下载的 compose.server.yml，没有 build。在独立项目／数据卷／回环端口内，完成初始化、Chromium 登录、实例设置保存、公共密钥说明分隔线、只读根目录、实际 Server UID 10001，以及 down／up 后原身份和设置保留。控制台和 HTTP 错误为零。结束后只清理本次独立容器／卷，没有修改或重启用户运行实例。

## 后续 main

标签保持上述来源，之后已提交并推送的 JSONL 较早历史页定位、缺失日志前缀校验和测试停止定时器修正不写成已经进入 v0.2.0。相关 3 项合同、Clippy 和本机／Server 实际浏览器验证通过；详见[历史性能记录](read-load-performance.md)。main 检查不再自动打包，显式候选与发行工作流负责产物构建，全部检查保留。

Desktop 发行者签名／公证／自动更新、资源迁移／跨主机接管／故障转移、Cloud 增量统计和进一步容量验证仍待推进；PostgreSQL 持续压测的 SIGKILL 来源未确定，不能记录为通过。凭据轮换按用户要求后移，Work 仅保留设计边界。
