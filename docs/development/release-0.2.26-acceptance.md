# v0.2.26 发行验收

范围为 macOS 用户级 LaunchAgent 远程更新，包括服务归属检查、KeepAlive 停机边界、原 plist 重启、安装回退及本机恢复。实现与原生验收入口见 [LaunchAgent 更新](launchagent-updates.md)，对应 PR [#16](https://github.com/mosttt/ternilo/pull/16)。

## 已完成验证

更新模块四项测试通过，包含嵌套 PID 不误判、精确服务目标边界、官方启动入口切换／恢复及超过原 30 秒窗口的实际就绪检查。更新弹窗九项测试、TypeScript、i18n、受影响模块 all-targets Clippy 和客户端／Server 构建通过。

当前内嵌 Web 的真实浏览器验证覆盖三语言的启用说明与服务不匹配提示，共六种状态，使用 390×600 深色布局。已检查滚动后完整可见的帮助命令、底部操作、诊断展开和无横向溢出；页面、控制台与 HTTP 无意外错误，截图已检查。

Cargo 工作区、22 个锁文件工作区包、Tauri、默认镜像及构建示例统一为 0.2.26，外部依赖记录不变。发行元数据两项测试通过。Executor 协议 49、数据代次 1 与升级签名策略不变。

## 待完成门禁

macOS 首轮原生夹具将默认 GUI 会话配置加载进 user 域，bootstrap 返回 I/O 错误。最小原生探针确认相同路径下 GUI 默认配置与 user／Background 均可加载，只有 user 默认会话配置失败；原生夹具已改为默认 GUI 成功安装与 user／Background 二进制 plist 回退场景，指南同步会话类型要求。

版本候选的 Apple Silicon 原生验收通过。Intel 在安装后的重复停止检查发现：主进程已退出时，launchd 仍可能短暂保留正在移除的服务定义，再次 bootout 返回 ESRCH。适配现等待精确服务目标真正卸载后才返回，ESRCH 按已进入停止处理，其余错误仍上报；原生断言增加外部确认服务已不再注册。继续以修正候选完成两种架构、完整 Checks、六平台打包、双架构镜像、签名和公开产物验收。当前尚未发布。
