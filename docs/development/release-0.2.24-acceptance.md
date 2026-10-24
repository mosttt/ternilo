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

## 浏览器夹具的展开状态

第四轮候选 `40068310041` 的插件 Local／Server-Node 回归、慢电脑工作台综合流程及此前浏览器组均通过。后续托管网页工具测试无条件点击已自动展开的当前工作区，将其折叠后等待会话条目，导致超时。本地复现与失败截图确认会话仍已选中、工作区处于折叠状态。夹具改为仅在 `aria-expanded` 不为 true 时展开，未修改产品行为或放宽工具执行断言。修复后完整流程通过（20.15 秒），覆盖 Local、账号模型、另一台电脑模型和已连接账号四条实际调用路径及引用持久化；页面、控制台和 HTTP 无错误。

## 正式发布与公开验收

[v0.2.24](https://github.com/mosttt/ternilo/releases/tag/v0.2.24) 已于 2026-10-24 18:25（北京时间）公开。PR [#14](https://github.com/mosttt/ternilo/pull/14) 已合并；标签、来源清单和程序版本对应合并提交 `2d741cbe661f176ca5c9652d8726bb95d2d10508`，合并内容与已验证候选一致。

[候选 Checks](https://github.com/mosttt/ternilo/actions/runs/40073018838) 六项作业通过。首轮 Windows 的只读 PowerShell 编码探测超时，单独重跑失败作业后通过，其他结果保留。[正式 Release](https://github.com/mosttt/ternilo/actions/runs/40078417945) 20 个作业全部通过，包含完整 Rust／SDK／数据库／浏览器／桌面与安装恢复、六平台原生包、两架构 Server 镜像运行、升级清单签名和 latest 镜像提升。

44 项公开附件的清单、GitHub 摘要与 `SHA256SUMS` 对应，来源版本及提交一致。仓库公钥完成客户端清单 Ed25519 验签，六个平台的大小、摘要和兼容策略均匹配。公开 Linux x64 客户端和 Server 归档完整性通过，两个程序实际报告 0.2.24。

公开程序的三项真实浏览器验收全部通过（34.41 秒），覆盖两端关于页、中英韩文、慢电脑分层加载和局部重试、默认电脑与实际目录登记、账号／空间隔离、版本行／更新窗口及手机深色布局；无页面、控制台或意外 HTTP 错误，实际截图已检查。

GHCR `:0.2.24` 与 `:latest` 均与公开发行镜像索引一致，包含 linux/amd64 和 linux/arm64，索引摘要为 `sha256:22a098fc4ed48acb7965cc87d17fde94f7a221d4e558bb3e5552624f464d061f`。运行证据使用正式发行 CI 中已测试并原样发布的镜像。生产签名仍由既有受保护环境完成。

发行后用户反馈 DMIT 从 0.2.23 更新时，在没有可见任务的情况下等待空闲超过半小时。该现场问题继续排查，不计入本版已修复内容；见[远程更新进度](remote-client-updates.md)。
