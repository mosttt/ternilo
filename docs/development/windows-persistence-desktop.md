# Windows 状态持久化与桌面控制台修复

用户在 Windows 更改预设、选择工作目录时遇到 `open state directory ... Access denied`，启动桌面应用还会出现控制台，关闭控制台后应用退出。

状态写入原先先同步文件、重命名提交，然后无条件用普通 File::open 打开父目录做 fsync。目录 fsync 是 POSIX 路径，Windows 不能按普通文件方式打开目录；错误发生在文件已提交之后，使界面把成功写入报告成失败。LocalState 的工作区／会话／归档、预设、Provider、凭据和模型连接均共用该实现。附件发布、分叉日志和扩展注册表另有同类调用。

统一 Local 的父目录同步 helper：Unix 保留目录同步，Windows 保留文件同步和重命名，不执行不支持的目录句柄同步。附件和分叉复用同一 helper；扩展注册表同样只在 Unix 同步目录。日志修复已经按 Unix 条件同步父目录。工作目录浏览的目录句柄是受平台 cfg 控制的独立实现，Windows 使用 cap-std，不套用本次 POSIX 修正。

桌面入口缺少 Windows GUI subsystem 声明；后台服务已经有 CREATE_NO_WINDOW，问题在桌面主程序。发行构建声明 windows subsystem，CLI 与 Server 保持 console subsystem。Windows 打包时读取实际 PE header，验证桌面为 GUI、CLI／Server 为控制台。

Windows 与两种 macOS 架构的 CI 从仅编译／目录边界扩展到实际工作区／会话／归档状态、预设、Provider、凭据、模型连接、附件、分叉及扩展注册表持久化测试。各系统实际执行结果以对应 CI 为准；Linux 本机复验不冒充 Windows 实机结果。此修复不删除用户数据，不要求管理员启动，不修改已有数据目录归属。

## 后续 CI 检查

2026-09-29，macOS Apple Silicon 的目录与持久化测试通过。Windows 的工作区、状态、配置、预设和附件检查通过；分叉测试的数据断言通过，但退出后立即删除临时数据库时仍遇到共享占用。`tokio-rusqlite` 的 Drop 仅通知后台线程，现将数据库连接的显式异步关闭纳入应用退出，等待句柄释放后才交还数据目录。队列持久化断言在重新打开的应用上检查，以验证实际落盘结果。

依赖门禁同时发现新公布的 RUSTSEC-2026-0315、RUSTSEC-2026-0316；Wasmtime 及配套依赖升级至 48.0.3，`cargo deny --locked check` 已通过。运行时回归与 Windows 实际 runner 结果仍需继续确认。
