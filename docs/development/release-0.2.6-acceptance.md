# v0.2.6 发行验收

2026-10-05：[v0.2.6](https://github.com/mosttt/ternilo/releases/tag/v0.2.6) 已正式公开。源码为 `02e67b580cb403cc5d11ece367fc57b80f485bfd`，固定 Linorun 为 `46cca18d63d1bef1c5475ac6912f18f5426a115b`。本版交付多提供方 OAuth2／OIDC、OAuth2-only 开放／邀请注册、账号状态标签、Rustls WSS 修复和稳定版镜像 `latest`。

[主分支完整检查](https://github.com/mosttt/ternilo/actions/runs/37221120060)与[完整发行流程](https://github.com/mosttt/ternilo/actions/runs/37221120329)均已通过，来源是同一提交。发行首轮六目标打包与双架构镜像候选全部成功，受限 PostgreSQL 通知合同一次未通过静默期断言；同一合同在独立临时数据库连续三次通过，发行第 2 次在原断言下通过。未修改源码、放宽测试或移动版本标签，复用已通过的包和镜像。完整 Web、Rust、SDK、受限 PostgreSQL、实际浏览器、Desktop 后台服务及双数据库恢复门禁通过。

本地完整 Web 测试链包含 999 项单元、7 项富文本及类型／国际化／构建检查；Server 全目标 Clippy、14 项管理接口回归通过。SQLite 7 项注册合同及受限 PostgreSQL 的 18 个身份场景、四项会话／MFA 集成通过。单用户模式可关闭所有 OIDC，不受保留的 OAuth2-only 注册条件阻碍；多用户模式继续要求可用提供方。

42 项公开附件全部实际下载，文件长度、本地 SHA256、GitHub 摘要及清单一致。12 个程序归档覆盖 Linux x86_64／ARM64、Windows x86_64／ARM64、macOS Intel／Apple Silicon，原生程序头、程序集合、94 份双语正式文档、最新版 README、文档索引、许可证与第三方说明通过核对，开发记录与私有配置没有进入归档。九个桌面安装器的实际摘要和格式通过；一份 DMG 首次下载被截断，重新下载后长度与摘要通过，不将首次不完整传输记为通过。

公开 Linux 归档与发行流程中已测的归档字节完全一致。实际 `ternilo 0.2.6` 通过真实 WSS 握手回归：HTTPS 清理后到达 WSS，拒绝未信任证书并保持运行，无 CryptoProvider panic。实际 `ternilo-server 0.2.6` 通过三个完整浏览器合同：多提供方及 OAuth-only 开放／邀请注册、普通 HTTP 新增／保存与地址恢复、ID Token／PKCE／刷新／账号绑定。320px 设置没有横向溢出，浅深主题账号状态标签已检查；截图等待登录弹窗退出动画结束后取证，无残留遮挡，仅调整临时取证时机，未改动产品或正式测试流程。

空 Docker 配置的匿名拉取通过，版本、来源与许可标签正确。`ghcr.io/mosttt/ternilo-server:latest` 和 `:0.2.6` 的完整索引相同，摘要为 `sha256:64b4ad09c80adf81900d88af64a4e6ba54fe5f63bd80a00b00ad8fdf8616659a`；linux/amd64 为 `sha256:0ae99b47c313311d01c449be7a9303c0130d6856e257406ec4871da39fef2a69`，linux/arm64 为 `sha256:a3f0bb8b87ff9de1689f7829d367fb2aa50d7b994905b8280de3e12f4fca3e0c`。别名更新已随发行流程成功执行。

公开 SQLite Compose 与 PostgreSQL overlay 均通过实际手机浏览器的初始化 Key、网页数据库选择、创建管理员、浏览器默认英文／手动中文覆盖、登录与 down/up 后身份及配置保留验收。根文件系统只读，Server UID 为 10001；PostgreSQL 使用独立 schema-owner 与受限 runtime，无超级用户权限和数据库主机端口。控制台、页面和核心 HTTP 流程没有错误。两个测试项目、容器、网络和私有卷均已清理，未操作已有实例；本机验证所需的独立网段覆盖仅留在临时目录，没有固化为通用部署或 CI 要求。

此前 v0.2.4 类型检查发现文案清理误删连接键，v0.2.5 发布前复查发现普通 HTTP 不提供 `crypto.randomUUID()`，两版均未公开，标签保持不变。最终版本恢复连接键并使用既有 `randomUuid()` 工具。Desktop 发布者签名、公证和自动更新仍未启用；实际格式与构建验收不代替所有物理设备的安装／卸载验收。认证采用最新多提供方配置格式，未保留旧格式兼容。
