# Tools, workflows and multiple agents

An Agent preset selects a reusable capability configuration. Standard exposes the usual software tools and Rhai Code Mode; PTC exposes capabilities through Code Mode; Minimal focuses on workspace files and shell; Creative adds guidance for experimenting with presets and runtime extensions. Host policy remains authoritative for every preset.

## Code Mode and skills

Code Mode uses Rhai to compose registered capabilities in a script. It is an orchestration interface, not unrestricted access to the host. Tool arguments, workspace permissions, approvals and execution limits still apply. Skills supply reusable task guidance and can be provided by extensions. Loading guidance does not authorize a forbidden filesystem or process operation.

Plans and todo lists describe work; goals track explicit execution objectives. A displayed plan is not evidence that actions have completed. Keep progress and completion tied to durable task results, especially after cancellation or reconnects.

## Subagents and background work

Subagents can perform bounded independent work and exchange results through their registered capabilities. Agent Team coordinates members, tasks and messages. Their work still uses the corresponding session identity, model snapshot and permissions. A collaborator's later message does not replace the original author of a running child task.

Shell commands, background jobs and persistent terminals have different lifecycles. Inspect their status and stop them explicitly when appropriate. Closing a webpage does not by itself stop the local service or accepted tasks. Use application service status and stop commands for the service as a whole.

## Workflows

Workflows coordinate multiple steps, waits and child executions. Persisted activity and child results distinguish a completed step from a waiting one. Cancellation must release owned waiters and allow runtime shutdown; a reconnect must not blindly replay external side effects. Shared model grants and submit permissions are rechecked as work continues.

Configure MCP, LSP, hooks, external ACP agents and package-provided capabilities in [extensions](extensions.md). See [automation](automation.md) for programmatic clients, [settings](settings.md) for tool/step ceilings and [security](security.md) for sandbox boundaries.
