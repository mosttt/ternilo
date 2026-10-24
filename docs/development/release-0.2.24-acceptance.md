# v0.2.24 发行验收

2026-10-24：用户授权发布小版本。基于 main `93f081c`，功能提交为 `efeb1f9`，范围为 Server 与电脑分层加载、打开工作区的默认电脑、版本状态图标及账号退出时旧模型目录请求的修复。资源迁移 WIP、Work 及已取消的 AppImage 成品启动 CI 不在本次范围。

## 已有证据与发行变更

功能验收见[工作台加载](server-workbench-loading.md)、[侧栏电脑选择](sidebar-computer-picker.md)及[远程更新状态](remote-client-updates.md)。工作台／目录／模型选择器 89 项、版本设置 14 项及此前受影响组件测试通过；两个 Server SQLite HTTP 回归、Server all-targets Clippy、TypeScript、i18n、文档和 Web 构建通过。独立双 Node／Server 的当前内嵌 Web 浏览器流程在 32.96 秒内通过，覆盖暂停电脑、导航和切换、局部 503 重试、真实文件夹登记、空间／账号隔离、三语言与 390px 深色版本行及更新弹窗。既有电脑管理流程另通过。

发行将 Cargo、锁文件中的工作区版本、Tauri、默认镜像及构建示例统一为 0.2.24，并提供中英文发行说明。既有浏览器 CI 增加本次双电脑回归入口；不增加 AppImage 启动验证。Executor 协议 49、数据代次 1 与签名兼容策略保持，0.2.23 客户端可继续连接，并在本机已启用时由 Server 发起远程更新。

## Tokio 安全门禁

首轮候选 Checks `40062623659` 的依赖门禁发现新收录的 [RUSTSEC-2026-0386](https://rustsec.org/advisories/RUSTSEC-2026-0386)：Tokio 1.53.1 的安全 AsyncFd 构造器接受缺乏文件描述符所有权保证的参数，可能违反 I/O 安全并造成释放后使用。官方公告和 [Tokio 1.53.3 说明](https://github.com/tokio-rs/tokio/releases/tag/tokio-1.53.3)均确认修复版本范围。

仅将直接依赖的最低版本及锁文件更新为 Tokio 1.53.3，其他外部依赖保持。项目与固定 Linorun 源码未直接调用被弃用的 AsyncFd 构造器，不需要新增 unsafe 调用或豁免公告。本地 `cargo deny check advisories licenses sources` 及锁定依赖解析已通过，继续以完整候选 CI 验证受影响运行路径。

## 发布状态

发布 PR [#14](https://github.com/mosttt/ternilo/pull/14) 已提交，候选检查中，尚未创建 v0.2.24 标签或公开 Release。后续按现有 Checks 和 Release 门禁完成候选检查、合并、六平台包、双架构镜像及签名发布，再核对公开产物。生产签名私钥仅由现有受保护发行环境使用。
