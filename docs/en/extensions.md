# Plugins and integrations

Profiles compose enabled plugin entries, each with an ID, kind and configuration. Edit the target computer/session or cloud target deliberately; configuration ownership follows that target. Saving validates configuration and dependencies. Host permissions and ceilings cannot be expanded by a plugin setting.

## MCP and LSP

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
    "tool_call_timeout_ms": 60000
  }
}
```

Omitting `cwd` uses the session workspace. Child environments are cleared except for basic system variables, explicit `env` and credential references resolved by the host. Tool calls still pass through Plan-mode, permission, hook and approval checks. An MCP server is not authorized to bypass those checks because it is external.

LSP stdio similarly configures a language-server executable and workspace-specific environment. Install the required executable on the execution host. Inspect/start/stop registered services through the application. A temporary manual stop and permanently disabling a plugin have different lifetimes.

## Runtimes and hooks

Code Mode composes capabilities through Rhai. Hooks can inspect the supported lifecycle points and return their declared outcomes; their configuration does not grant arbitrary new host capabilities. External ACP agents can be configured as subagent integrations with explicit executable/arguments and host-controlled access.

External Rhai and WASM Component packages share one registry, trust model and lifecycle. Package installation and profile mounting are separate steps. Signatures, manifests, requested capabilities and runtime limits are checked before activation. For exact package contracts and runnable examples, see [extension packages](extension-packages.md). For everyday orchestration see [Agent tools](agents.md).
