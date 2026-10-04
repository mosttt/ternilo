# v0.2.5 发行验收

本版包含已完成本地验收的多提供方 OAuth2／OIDC、OAuth2-only 开放及邀请注册、账号状态标签、Rustls WSS 初始化修复及稳定版镜像 `latest` 别名。功能结果见[OAuth2 验证](multiple-login-providers.md)与[WSS 验证](wss-crypto-provider.md)。

v0.2.4 在公开前因误删连接翻译键而未通过类型检查，未发布；原标签保留。v0.2.5 恢复该键，重新执行完整 Web 测试链，再进行完整发行检查、六目标打包及双架构镜像验收。公开产物与镜像待发行结束后记录实测结果。

2026-10-05：发布前复查发现新增登录提供方的 ID 生成直接使用 `crypto.randomUUID()`，普通 HTTP 的管理页面缺少此方法。未公开发行，取消候选构建；复用现有 `randomUuid()` 工具及随机字节生成路径，不降低 OAuth 登录的安全环境要求，通过 v0.2.6 继续交付。
