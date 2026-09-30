# Windows 状态持久化与桌面控制台修复

状态：Windows、macOS Intel 和 Apple Silicon 的目录边界、状态及配置持久化回归已通过。四平台发行作业均通过真实 Desktop 后台服务的启动、持久化、停止和重启验收；Windows 安装器内的主程序已核对 GUI subsystem。

状态写入先同步文件并重命名提交。Unix 同步父目录；Windows 保留文件同步和重命名，不用普通文件句柄执行目录 fsync，避免把已经提交的写入误报为拒绝访问。工作区、会话、归档、预设、Provider、凭据和模型连接共用该实现；附件、分叉、日志修复和扩展注册表采用相同的平台边界。工作目录浏览的 Windows 实现使用 cap-std。

Desktop 发行入口使用 Windows GUI subsystem，CLI 与 Server 使用 console subsystem。后台服务与受管工具进程隐藏控制台。Windows 打包检查真实 PE header，确认程序类型符合用途。进程监督的主动终止与句柄释放检查见 [Node 账号清理](node-account-cleanup.md)。

完整退出通过 `LocalApplication::close` 关闭偏好、队列、搜索、投影和 Agent Team 的五个 SQLite 连接，等待句柄释放后再交还数据目录。`shutdown` 先停止运行并允许适配器读取收尾数据；服务等待已接收 Node 命令结束后再关闭存储，保证上传与指令结果落盘。

验证覆盖工作区、会话、归档状态、预设、Provider、凭据、模型连接、附件、分叉及扩展注册表。队列与上传通过关闭后重新打开检查真实持久化。无需以管理员身份启动，也不改变用户数据目录归属。Wasmtime 使用 48.0.3，依赖安全检查已通过。
