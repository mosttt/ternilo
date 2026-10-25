# macOS LaunchAgent 远程更新

为官方 macOS browser／CLI 客户端增加用户级 LaunchAgent 更新适配，覆盖安装、重启、启动回退和本机恢复。

新增 `--restart launch-agent`，使用 plist 的 Label，默认读取当前用户 `~/Library/LaunchAgents/<Label>.plist`、目标为 `gui/<uid>`；支持显式 plist 路径和 `--launchd-domain user`。沿用签名包、安装布局、空闲维护门、操作锁、120 秒健康确认和恢复计划，不引入新更新协议或生产签名配置。

下载前及安装前检查 plist Label 和已加载服务的实际 PID。通过 macOS 自带 plutil 读取 XML／二进制 plist，通过 launchctl 控制精确的服务目标。安装先 bootout，避免 KeepAlive 重启旧程序；安装、启动回退及本机恢复复用原 plist bootstrap／kickstart，不修改用户 plist。已注册的服务直接 kickstart，已卸载服务的停止操作可重复执行。域与 Label 验证拒绝整域目标；不管理系统 LaunchDaemon。

更新辅助进程复用现有 Unix 独立进程组。launchd.plist(5) 的 AbandonProcessGroup 说明指出默认清理主服务同组进程；本次原生验收须证明从 Agent 内部启动的辅助进程在 bootout 后仍可执行安装与回退，不能仅凭 Linux 单元测试判定适配完成。

新增原生 macOS 验收，覆盖 KeepAlive、含空格与 & 的路径、正确与错误 PID／Label、已注册启动、实际程序替换、错误版本回退、服务重新加载及保存配置恢复。挂入现有 Apple Silicon／Intel 平台检查，使用各自构建的真实客户端和隔离 LaunchAgent。普通平台测试覆盖配置边界和嵌套 PID 不误判。中英韩更新说明与中英文客户端更新指南同步。

原生夹具首轮发现默认 GUI 会话 plist 不能加载到 user 域。最小 launchd 探针确认这与安装路径无关：GUI 默认配置、user／Background 均能正常加载。夹具分别使用默认 GUI 安装和 user／Background 二进制 plist 回退，临时诊断探针已移除。最终原生结果及发行状态见 [v0.2.26 验收](release-0.2.26-acceptance.md)。
