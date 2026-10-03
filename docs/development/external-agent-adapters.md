# 外部代理接入

2026-10-03，按优先级 2 扩展通用 ACP v1 接入，保留一次性外部会话的清晰边界。

## 当前实现

- `env_refs` 将子进程环境变量映射到执行电脑的凭据引用，每次启动重新解析。删除凭据后不能启动后续任务；不会复制当前会话的 Server 模型权限或跨电脑来源凭据。
- `auth_method` 仅允许 initialize 公布的 ID，在 session/new 前发送 authenticate。不配置时沿用代理自身登录或环境鉴权，不自动启动交互式登录。
- `session_mode` 仅允许新会话公布的模式，在 prompt 前设置。不配置时保留供应商默认值。
- 严格使用 ACP v1；未公布的认证或模式会拒绝任务。权限仍默认 reject，取消保留 session/cancel、进程组清理与执行目录占用释放。
- 配置统一 snake_case，移除旧 camelCase 别名。中文／英文扩展文档及 Gemini／Claude 插件配置示例已同步。

## 验证

使用实际发布包 Gemini CLI 0.62.0 和 Claude Agent ACP 0.85.1，分别启动 `gemini --acp` 与 `claude-agent-acp`，验证 ACP v1 initialize。Gemini 公布 oauth-personal、gemini-api-key、vertex-ai、gateway；本次无凭据的 Claude 初始化未公布认证方法。因此示例依赖明确 env_refs 或预先登录，不凭空指定认证 ID。

真实收费模型未调用。浏览器验收使用独立受控 ACP 进程，验证真实本机服务的凭据解析、authenticate→session/new→session/set_mode→prompt 顺序、默认拒绝外部权限、结果显示、删除凭据后不启动进程、未公布选项不发送 prompt，以及 UI 停止触发协议取消。桌面与 390 px 页面、控制台和网络同时检查。

5 项已有进程组单元测试验证父进程先退出、后代忽略 TERM、取消等待及宽限清理；Clippy 通过。验收不代表完整外部 Agent 图片、终端、持久会话恢复、供应商私有扩展或 follow-up 支持。

来源：[Gemini ACP](https://github.com/google-gemini/gemini-cli/blob/main/docs/cli/acp-mode.md)、[Claude Agent ACP](https://github.com/agentclientprotocol/claude-agent-acp)、[ACP 认证](https://agentclientprotocol.com/protocol/authentication)、[ACP 会话模式](https://agentclientprotocol.com/protocol/session-modes)。
