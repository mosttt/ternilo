# CI 与客户端／Server 闭环进度

当前版本准备为 `0.1.1`。`v0.1.0` 已公开，包含四平台客户端／Server、桌面安装器及公开 Server 镜像；当前修复通过新版本交付。正式发布必须等待对应源码的检查、打包、镜像验收和附件上传完成。

## 当前结果

- Apache-2.0 正文、第三方声明及交付物许可文件齐全。
- CI 覆盖 Web、Rust、依赖、两种 macOS 架构、Windows、SDK、受限 PostgreSQL、真实浏览器、原生 Linux 桌面及安装恢复。Windows 和 macOS 持久化检查已通过。
- 客户端归档使用 `ternilo-`；Windows 使用 ZIP，Linux／macOS 使用 tar.gz，Desktop 使用各平台安装器。正式文档按 `docs/zh-CN/`、`docs/en/` 分类；开发记录仅保留中文，不进入发行包。
- Compose 使用已发布版本的 Server 镜像，生产部署无需在用户机器构建。发行作业验证并推送实际镜像，上传全部附件后公开 Release。
- Windows 状态保存采用对应平台的文件同步方式；桌面 Release 使用 GUI subsystem。完整退出会等待 Node 指令收尾，再释放所有本地数据库。
- 主题默认跟随系统。Node 命令使用跨终端参数，中文名称可用，参数在登记前校验。
- 对外凭据和公开会话 ID 采用 `ter_` 加用途短码；当前规则见[标识命名](../zh-CN/identifiers.md)。登录会话显示设备描述、首次／最近 IP 和活动时间，转发 IP 仅接受可信代理。
- 普通历史自动补齐，较大历史分批加载，实时游标独立；手机轨迹虚拟列表保留测量结果并暂停触摸时追尾。
- 不同电脑选择同名目录时自动建议包含电脑 ID 的工作区名称，显式冲突返回 409；重开保持名称，目录弹窗不进行定时轮询。
- 工作区图片可以通过 Server 预览。HTML 预览清理主动内容与外部资源；远程文件返回前重新校验工作区查看权限。
- 原生账号恢复、远程 Python／TypeScript SDK、项目共享继承、未知模型用量核对、设备 Provider 用量观察和 Server 模型连接的 Gemini／Anthropic 协议已实现并验证。
- MCP 支持通知后的工具目录刷新，以及默认关闭、最多 10 次的重连；失败调用不重放，旧 handler 失效。155 项 Builtins 测试及 Clippy 通过。
- Wasmtime 使用 48.0.3，Web 测试依赖 undici 锁定 8.11.2；依赖安全检查通过。

## 验证

本地已完成前端完整 948 项、客户端／Local 235 项测试、Clippy、部署打包 23 项测试、正式／开发文档链接检查、SQLite 与受限 PostgreSQL 会话元数据检查，以及历史、预览、手机触摸、账号撤销、OIDC、PWA、SDK／插件和安装恢复的真实浏览器验收。后续简写命名修改另执行相应认证、模型授权、Worker 和浏览器回归，以最新 CI 及发行记录确认交付版本。

[GitHub Actions](https://github.com/mosttt/ternilo/actions) 保存检查与各平台构建结果；[发行记录](https://github.com/mosttt/ternilo/releases)保存正式产物、来源和校验和。源码检查通过不等于已经发布，构建安装器也不代替发行者签名或所有物理设备上的安装验收。

## 后续范围

1. 完成 `0.1.1` 对应提交的 CI、各平台实际产物及 Server 镜像验收。
2. 完善资源所有权交接，以及账号停用后完整任务／资源清理的语义。
3. 跨 Server 扩容仍缺少 Node 连接持有者路由与通知同步，当前按单 Server 部署。

Work 保留持久资源 ID、宿主节点、版本化执行接口和授权边界的设计；不增加当前闭环不需要的容器调度、空页面或占位程序。具体约束见[Work 资源与执行边界](execution-coordination.md)。
