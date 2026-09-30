# MCP 工具目录刷新与可选重连

`ternilo.mcp.stdio` 已处理 `notifications/tools/list_changed`。通知只标记工具目录变化；下一次任务准备工具时，在当前调用全部结束后重新读取 `tools/list`，沿用同一个 MCP 进程。刷新完成后同时替换工具名称、说明与输入 schema，先前持有的 handler 无法继续调用旧目录。正在执行的调用仍可完成。

读取的新目录无效或发现过程失败时，服务进入 `Failed`，停止原进程和所属子进程，并禁止旧 handler 发出 RPC。原生工具仍能准备和执行，服务状态保留具体错误。取消目录读取不会替换目录；后续准备可以重新处理尚未完成的通知。

插件配置新增 `reconnect_attempts`，默认 `0`，允许整数 `0` 到 `10`。大于 `0` 时，失败服务可在后续任务准备时重连，每次准备最多启动一次，不创建后台重连循环，不重新发送已经失败或取消的工具调用。连续重连的预算仅在实际完成工具 RPC 或手动重新启动服务后恢复；单纯握手成功不会清零预算。手动停止保持停止，卸载后不可再次启动。

配置示例：

```json
{
  "server_name": "workspace",
  "command": "mcp-server",
  "startup_timeout_ms": 15000,
  "tool_call_timeout_ms": 60000,
  "reconnect_attempts": 2
}
```

验证命令：`cargo test --locked -p ternilo-builtins --lib`。155 个测试全部通过，其中 19 个 MCP 测试覆盖同进程 schema 更新、活动调用期间的刷新等待、旧 handler 失效、无效目录处理、重连预算、手动控制、默认不自动重连、取消及进程清理。`cargo clippy --locked -p ternilo-builtins --all-targets -- -D warnings` 通过。共享 stdio 夹具对应的 LSP 生命周期测试也通过。进程和所属子进程的这些验证在 Unix 执行；Windows 的进程管理代码沿用现有实现，仍需平台 CI 验证。

工作区执行许可沿用当前会话的目录占用规则。重连成功后服务继续占用相应许可，停止、失败清理和卸载会释放许可。此改动不增加 MCP HTTP 传输、服务器请求交互或跨进程会话恢复。
