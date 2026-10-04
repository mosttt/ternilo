# v0.2.6 发行验收

本版交付多提供方 OAuth2／OIDC、OAuth2-only 开放／邀请注册、账号状态标签、Rustls WSS 修复和稳定版镜像 `latest`。本地功能合同与测试结果见[OAuth2](multiple-login-providers.md)、[WSS](wss-crypto-provider.md)及[别名](server-image-latest.md)记录。

两个发布前候选均未公开：v0.2.4 类型检查发现同一行文案清理误删连接键；v0.2.5 发布前复查发现普通 HTTP 设置页缺少 `crypto.randomUUID()`。连接键已恢复，随机 ID 使用项目既有工具；完整 Web 测试链、受限数据库和真实浏览器流程已通过，另增加普通 HTTP 新增提供方回归。旧候选标签保持不变。

本版实际 Server 程序已编译；完整 Web 测试链（999 项单元、7 项富文本及类型／国际化／构建）、Server 全目标 Clippy、14 项管理接口回归通过。单用户模式不再受到保留的 OAuth2-only 注册条件阻碍，可关闭 OIDC；多用户模式的可用提供方要求继续执行。

实际 `ternilo-server 0.2.6` 通过三项完整浏览器合同：多提供方与邀请、非安全 HTTP 的提供方新增／持久化与地址恢复、OIDC ID Token／PKCE／刷新及绑定。单用户模式关闭登录方式的回归也通过。接下来执行完整发行检查、六目标安装包与双架构镜像验收，并记录公开产物与 `latest` 的实际核对结果；当前不记为已发布。
