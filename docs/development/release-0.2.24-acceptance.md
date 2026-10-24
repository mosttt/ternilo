# v0.2.24 发行验收

2026-10-24：用户授权发布小版本。基于 main `93f081c`，功能提交为 `efeb1f9`，范围为 Server 与电脑分层加载、打开工作区的默认电脑、版本状态图标及账号退出时旧模型目录请求的修复。资源迁移 WIP、Work 及已取消的 AppImage 成品启动 CI 不在本次范围。

## 已有证据与发行变更

功能验收见[工作台加载](server-workbench-loading.md)、[侧栏电脑选择](sidebar-computer-picker.md)及[远程更新状态](remote-client-updates.md)。工作台／目录／模型选择器 89 项、版本设置 14 项及此前受影响组件测试通过；两个 Server SQLite HTTP 回归、Server all-targets Clippy、TypeScript、i18n、文档和 Web 构建通过。独立双 Node／Server 的当前内嵌 Web 浏览器流程在 32.96 秒内通过，覆盖暂停电脑、导航和切换、局部 503 重试、真实文件夹登记、空间／账号隔离、三语言与 390px 深色版本行及更新弹窗。既有电脑管理流程另通过。

发行将 Cargo、锁文件中的工作区版本、Tauri、默认镜像及构建示例统一为 0.2.24，并提供中英文发行说明。既有浏览器 CI 增加本次双电脑回归入口；不增加 AppImage 启动验证。Executor 协议 49、数据代次 1 与签名兼容策略保持，0.2.23 客户端可继续连接，并在本机已启用时由 Server 发起远程更新。

## Tokio 安全门禁

首轮候选 Checks `40062623659` 的依赖门禁发现新收录的 [RUSTSEC-2026-0386](https://rustsec.org/advisories/RUSTSEC-2026-0386)：Tokio 1.53.1 的安全 AsyncFd 构造器接受缺乏文件描述符所有权保证的参数，可能违反 I/O 安全并造成释放后使用。官方公告和 [Tokio 1.53.3 说明](https://github.com/tokio-rs/tokio/releases/tag/tokio-1.53.3)均确认修复版本范围。

仅将直接依赖的最低版本及锁文件更新为 Tokio 1.53.3，其他外部依赖保持。项目与固定 Linorun 源码未直接调用被弃用的 AsyncFd 构造器，不需要新增 unsafe 调用或豁免公告。本地 `cargo deny check advisories licenses sources` 及锁定依赖解析已通过，继续以完整候选 CI 验证受影响运行路径。

## 测试环境 HTTP 依赖

第二轮候选 `40063559849` 的 Rust 安全门禁通过，npm 门禁发现 jsdom 间接引入的 undici 8.11.2 受三项公告影响：[GHSA-4qv8-2p9j-2685](https://github.com/advisories/GHSA-4qv8-2p9j-2685)、[GHSA-f72v-6w45-q472](https://github.com/advisories/GHSA-f72v-6w45-q472) 和 [GHSA-xjj6-f33q-929c](https://github.com/advisories/GHSA-xjj6-f33q-929c)，分别涉及 EventSource 内存耗尽、SRI 校验绕过及过多内容编码导致未处理错误。官方修复范围为 8.12.2 及以上。

按原依赖范围仅更新 undici 锁记录到当前补丁 8.12.3，未增加直接依赖或批量升级。npm audit 为 0 漏洞，工作台及更新弹窗 53 项测试通过，Web 构建产物不变。这是测试环境依赖，未纳入浏览器交付 bundle。新候选继续完整 CI。

## 异步目录刷新与插件继承

第三轮候选 `40064063202` 的安全与四个平台检查通过，主作业的 Rust、SDK、受限 PostgreSQL 也通过。浏览器回归中，Server／Node 的插件设置在移除会话覆盖后出现继承输入框空白，其他同组 14 项通过；错误发生在同一目标后台刷新暂时清空插件目录、表单在 schema 未返回时建立空文本草稿的窗口。

同一目标后台刷新现在保留已显示的目录与预设，切换目标仍立即隔离旧数据。插件输入框按当前草稿和最新 schema 推导未编辑字段，仅保留用户实际编辑过的文本；因此迟到的默认值会补齐，同时不会覆盖用户清空或尚未保存的内容。新增回归在修复前复现空值；修复后工作台、插件卡片及设置的 78 项测试、类型检查和构建通过。重新构建当前 0.2.24 Client／Server 后，Server／Node 插件三语言同步和继承流程通过（51.29 秒），慢电脑工作台综合流程通过（42.21 秒）。Local 首次验证遭遇本机 Chrome 页面进程崩溃；使用项目锁定的 Playwright Chromium 补验通过（52.56 秒）。两个入口均无页面、控制台或非预期 HTTP 错误，实际截图确认恢复继承后子代理数量为 8，其他默认参数正常显示。

## 发布状态

发布 PR [#14](https://github.com/mosttt/ternilo/pull/14) 已提交，候选检查中，尚未创建 v0.2.24 标签或公开 Release。后续按现有 Checks 和 Release 门禁完成候选检查、合并、六平台包、双架构镜像及签名发布，再核对公开产物。生产签名私钥仅由现有受保护发行环境使用。
