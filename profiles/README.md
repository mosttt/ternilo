# 手动加载的配置示例

这个目录存放可选的启动配置示例。默认运行 `ternilo` 或 `ternilo serve` 时，程序使用代码内置的插件组合，不会自动读取本目录，也不会把网页设置保存到这里。

需要叠加某份配置时，显式传入 `--profile`，或设置 `TERNILO_LOCAL_PROFILES`。多个文件按指定顺序叠加到内置配置上；同一插件 ID 的后续配置覆盖前面的配置。

`local.json` 是基础组合示例，包含 `ternilo.model.rule` 离线测试模型，不是已接入真实大模型的生产配置。正常对话仍需配置可用的大模型 Provider。

日常从网页管理模型、插件和 Agent 预设。本机配置保存在对应 Ternilo 数据目录；Server 的用户配置和平台模型管理保存到 Server 数据库。平台管理、Worker 部署配置与本目录无关，参见[用户指南](../docs/user-guide.md)和[Worker 部署](../docs/worker.md)。
