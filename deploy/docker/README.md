# 部署入口

`ternilo-deploy` 是 Server 和 Worker 的部署工具。每个服务使用自己的部署目录和私有数据卷，分别由 `compose.server.yml`、`compose.worker.yml` 启动。先完成 [Server 部署](../../docs/zh-CN/deployment.md)；需要平台托管执行时，再按 [Worker 部署](../../docs/zh-CN/worker.md)接入 Worker。

数据库、公开地址和执行隔离方式属于部署配置。账号、平台模型及授权在网页中管理；修改模型 API Key 不需要编辑执行策略或重启 Worker。单用户和多用户使用同一程序，两种模式都可选择 SQLite 或 PostgreSQL。

网页的“用户设置”管理个人偏好、账号、模型连接与“我的机器”，“我的模型”提供模型目录、接入密钥和个人用量。管理员另从“平台管理”进入实例、平台账号、模型服务和 Worker 管理。团队成员、权限组和空间配额从当前空间的管理入口进入。

自动化保存平台模型使用 `ternilo-deploy configure-model --provider-profile provider.json`，密钥位置及轮换见 [密钥说明](secrets/README.md)。Server 与 Worker 分别使用 `backup`、`restore` 操作；恢复时使用同一停写时间点的 Server 数据库、Server 配置和完整 Worker 卷，具体步骤以部署指南为准。

Server 默认拉取公开镜像 `ghcr.io/mosttt/ternilo-server:0.2.10`，不需要本地构建或 Linorun 源码。直接使用 Compose 的最短流程见[快速部署](../../docs/zh-CN/docker-compose.md)。可通过 `TERNILO_IMAGE` 或部署工具的 `--image` 改用其他明确版本／摘要；Worker 镜像仍需单独准备。源码构建仅用于开发，见[发布说明](../../docs/zh-CN/release-packaging.md)。
