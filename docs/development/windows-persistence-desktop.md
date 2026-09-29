# Windows 状态持久化与桌面控制台修复

用户在 Windows 更改预设、选择工作目录时遇到 `open state directory ... Access denied`，启动桌面应用还会出现控制台，关闭控制台后应用退出。

状态写入原先先同步文件、重命名提交，然后无条件用普通 File::open 打开父目录做 fsync。目录 fsync 是 POSIX 路径，Windows 不能按普通文件方式打开目录；错误发生在文件已提交之后，使界面把成功写入报告成失败。LocalState 的工作区／会话／归档、预设、Provider、凭据和模型连接均共用该实现。附件发布、分叉日志和扩展注册表另有同类调用。

统一 Local 的父目录同步 helper：Unix 保留目录同步，Windows 保留文件同步和重命名，不执行不支持的目录句柄同步。附件和分叉复用同一 helper；扩展注册表同样只在 Unix 同步目录。日志修复已经按 Unix 条件同步父目录。工作目录浏览的目录句柄是受平台 cfg 控制的独立实现，Windows 使用 cap-std，不套用本次 POSIX 修正。

桌面入口缺少 Windows GUI subsystem 声明；后台服务已经有 CREATE_NO_WINDOW，问题在桌面主程序。发行构建声明 windows subsystem，CLI 与 Server 保持 console subsystem。Windows 打包时读取实际 PE header，验证桌面为 GUI、CLI／Server 为控制台。

Windows 与两种 macOS 架构的 CI 从仅编译／目录边界扩展到实际工作区／会话／归档状态、预设、Provider、凭据、模型连接、附件、分叉及扩展注册表持久化测试。各系统实际执行结果以对应 CI 为准；Linux 本机复验不冒充 Windows 实机结果。此修复不删除用户数据，不要求管理员启动，不修改已有数据目录归属。
