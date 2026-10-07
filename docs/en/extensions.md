# Plugins and integrations

Profiles compose enabled plugin entries, each with an ID, kind and configuration. Edit the target computer/session or cloud target deliberately; configuration ownership follows that target. Saving validates configuration and dependencies. Host permissions and ceilings cannot be expanded by a plugin setting.

The Agent preset editor provides a read-only complete Profile preview, including inherited base plugins, and editable preset overrides JSON. A copy of the standard preset may contain only `code-mode` in its overrides; the other base plugins still apply. Removing an override restores inheritance, while disabling an inherited plugin saves a disabled override for that entry.

## MCP and LSP

To add MCP for the first time, open **User settings → Agent presets**, copy the current preset and edit it. Under **Plugin Profile (advanced) → Preset overrides JSON (editable)**, append the configuration below to the existing `plugins` array. Save the preset and click **Use** on its card to apply it to the current idle Session. Subsequent field changes are available under **Plugins → Plugin configuration**. Install the MCP program on the execution computer or environment; the current integration uses stdio and does not accept HTTP/SSE URLs.

MCP stdio starts a configured server and exposes its declared tools through the shared tool pipeline. For example:

```json
{
  "id": "project-mcp",
  "kind": "ternilo.mcp.stdio",
  "enabled": true,
  "config": {
    "server_name": "project",
    "command": "mcp-server-command",
    "args": [],
    "env": {},
    "env_refs": {"SERVICE_API_KEY": "PROJECT_SERVICE_API_KEY"},
    "cwd": "/projects/example",
    "startup_timeout_ms": 15000,
    "tool_call_timeout_ms": 60000,
    "reconnect_attempts": 0
  }
}
```

Omitting `cwd` uses the session workspace. Child environments are cleared except for basic system variables, explicit `env` and credential references resolved by the host. Tool calls still pass through Plan-mode, permission, hook and approval checks. An MCP server is not authorized to bypass those checks because it is external.

After `notifications/tools/list_changed`, the next tool preparation refreshes the catalog once active calls finish, keeps the same process and invalidates handlers captured from the old schema. A failed refresh stops the service and records its error in background-service status; native tools remain available.

`reconnect_attempts` defaults to `0` and accepts integers from `0` through `10`. When enabled, later task preparations may reconnect a failed service within that budget. Failed or cancelled tool calls are never replayed. A completed tool RPC or an explicit restart replenishes the budget; a successful handshake alone does not. A manually stopped service requires an explicit start.

LSP stdio similarly configures a language-server executable and workspace-specific environment. Install the required executable on the execution host. Inspect/start/stop registered services through the application. A temporary manual stop and permanently disabling a plugin have different lifetimes.

## Runtimes and hooks

Code Mode composes capabilities through Rhai. Hooks can inspect the supported lifecycle points and return their declared outcomes; their configuration does not grant arbitrary new host capabilities. External ACP agents can be configured as subagent integrations with explicit executable/arguments and host-controlled access.

External Rhai and WASM Component packages share one registry, trust model and lifecycle. Package installation and profile mounting are separate steps. Signatures, manifests, requested capabilities and runtime limits are checked before activation. For exact package contracts and runnable examples, see [extension packages](extension-packages.md). For everyday orchestration see [Agent tools](agents.md).

## External ACP agents

`ternilo.subagents.acp` starts an installed ACP v1 executable with explicit `provider_name`, `command` and `args`. Each task creates its own external session. Omitting `cwd` uses the parent workspace. Child environments contain only basic system variables, explicit `env` and host credentials named by `env_refs`. For example, `"env_refs": {"ANTHROPIC_API_KEY": "CLAUDE_AGENT_KEY"}` injects that saved credential into the child without storing a key in the preset. Names must not overlap `env`; missing references fail before process launch.

Optional `auth_method` selects an ID advertised by `initialize` and authenticates before session creation. Omit it to use the agent's existing login or environment credentials. Optional `session_mode` selects an advertised mode before submitting the prompt. Unsupported protocols, authentication methods or modes fail explicitly. Ternilo does not automatically start an interactive login or choose a more permissive mode.

`permission` defaults to `reject`. Use `allow` only with an independently trusted external sandbox or policy. Cancellation sends `session/cancel` and then terminates the owned process group after the configured grace period. External sessions do not support follow-up messages or restoration.

Gemini CLI runs as `gemini --acp`; the Claude Agent SDK adapter runs as `claude-agent-acp`. See the [Gemini plugin profile](../../examples/gemini-acp-profile.json) and [Claude plugin profile](../../examples/claude-acp-profile.json). Install and configure these programs on the execution computer; Ternilo does not download them automatically. On Windows, `command` can be `node`, with the absolute installed package path to `bundle/gemini.js` or `dist/index.js` as one argument; add `--acp` after the Gemini entry point. Their model calls use their own authentication, not the parent session's Server model delegation or computer model forwarding.

References: [Gemini ACP mode](https://github.com/google-gemini/gemini-cli/blob/main/docs/cli/acp-mode.md), [Claude Agent ACP](https://github.com/agentclientprotocol/claude-agent-acp), [ACP authentication](https://agentclientprotocol.com/protocol/authentication). Supported scope is ACP v1 text tasks/results, authentication and mode negotiation, permission decisions and cancellation. Full terminal, image, history restoration and vendor-specific extensions are not provided.
