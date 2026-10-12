# Tools, workflows and multiple agents

An Agent preset selects a reusable capability configuration. Standard exposes the usual software tools and Rhai Code Mode; PTC exposes capabilities through Code Mode; Minimal focuses on workspace files and shell; Creative adds guidance for experimenting with presets and runtime extensions. Host policy remains authoritative for every preset.

## Code Mode and skills

Code Mode uses Rhai to compose registered capabilities in a script. It is an orchestration interface, not unrestricted access to the host. Tool arguments, workspace permissions, approvals and execution limits still apply. Skills supply reusable task guidance and can be provided by extensions. Loading guidance does not authorize a forbidden filesystem or process operation.

Plans and todo lists describe work; goals track explicit execution objectives. Goals are disabled until the user sets one through `/goal` or the goal interface. Ordinary conversations cannot start goals. Until enabled, `update_goal` is absent from model and Code Mode catalogs and rejected by execution. Models cannot reactivate completed or blocked goals; the user must explicitly resume them. A displayed plan is not evidence that actions have completed. Keep progress and completion tied to durable task results, especially after cancellation or reconnects.

## Subagents and background work

Subagents can perform bounded independent work and exchange results through their registered capabilities. Agent Team coordinates members, tasks and messages. Their work still uses the corresponding session identity, model snapshot and permissions. A collaborator's later message does not replace the original author of a running child task.

Shell commands, background jobs and persistent terminals have different lifecycles. Inspect their status and stop them explicitly when appropriate. Closing a webpage does not by itself stop the local service or accepted tasks. Use application service status and stop commands for the service as a whole.

## Workflows

Workflows coordinate multiple steps, waits and child executions. Persisted activity and child results distinguish a completed step from a waiting one. Cancellation must release owned waiters and allow runtime shutdown; a reconnect must not blindly replay external side effects. Shared model grants and submit permissions are rechecked as work continues.

Configure MCP, LSP, hooks, external ACP agents and package-provided capabilities in [extensions](extensions.md). See [automation](automation.md) for programmatic clients, [settings](settings.md) for tool/step ceilings and [security](security.md) for sandbox boundaries.

## Roles and collaboration modes

Open `subagents` in Settings → Plugins to choose a collaboration mode and configure Investigator, Implementer and Reviewer roles. The coordinator uses the session's main model. Role settings do not change the main model or the account default.

| Mode | Default concurrent children | Behavior |
|---|---:|---|
| Independent | 0 | The current agent handles the task; spawning and follow-ups are disabled. |
| Assisted | 4 | The coordinator delegates bounded supporting tasks as needed. |
| Parallel | 8 | Independent tasks can run together; dependent work remains sequential. |

Assisted and Parallel allow a custom limit of 1–32 children; the coordinator does not occupy a slot. An explicit limit is retained when changing modes. Total child limits, execution capacity and model grants still apply. Workflows use the same collaboration limit to batch tasks while retaining result order and cancellation behavior.

Each role can inherit the coordinator model or select a saved model and a supported reasoning effort, with additional instructions. Investigators and Reviewers default to read-only; Implementers inherit session permissions. A role cannot broaden its parent's permissions. Role children default to Independent mode to prevent automatic team expansion. Existing child sessions retain their own configuration when the parent's role settings change.

Server rechecks role model authorization; a client-supplied internal model snapshot does not grant access. Connected computers and the existing Worker retain their model routing, quotas, cancellation and persistence rules. These settings apply to built-in persistent subagents; external ACP agents use their provider configuration.

Agent Team has Members, Tasks, Mailbox and Usage tabs. The same session tree shares its task board and mailbox; a normal conversation fork starts a separate team. Task edits use revisions to detect concurrent changes. Mailbox messages do not start an agent: use a member's follow-up input to resume work. Only the recipient can mark a message read. Refreshing or restarting the Worker preserves canonical team records on Server.

## Load tool definitions as needed

Enable “Load tool definitions on demand” in `tool-registry` settings to reduce tool schema content sent to the model. Common tools remain available; `search_tools` loads current definitions by name or keyword. The cache defaults to 32 additional definitions and supports 1–128. Older definitions are evicted at capacity and can be rediscovered.

This option is off by default. Host permissions, approvals and Code Mode restrictions remain authoritative. Explicit commands can still use enabled tools. The definition cache belongs to the current session runtime and is not written into project files.

## Context handoff summaries

Automatic context compaction defaults to a structured summary of objectives, constraints, decisions, completed work, pending work, files, verification and risks. Disable structured summaries in the context compaction plugin if needed. Expand the turn's process and compaction record to inspect the summary. Subsequent model requests retain the handoff, while the original history remains available after restart. Existing plain-text summaries remain compatible. Manual `/compact` retains its deterministic summary behavior.

## Team usage and estimated cost

The Usage tab shows the coordinator, descendants and totals for readable members. It groups responses by the provider and model actually recorded, includes responses before regeneration and excludes replayed responses. Cache reads and writes are part of input tokens; reasoning is part of output tokens and is not counted twice. Unreadable members and missing usage are marked unknown.

“Configure estimate prices” accepts prices per million tokens for USD, CNY and KRW separately. Prices are stored only in the current browser. Missing required prices leave cost unknown; enter 0 explicitly for a free category. A single cache-write rate is used without inferring provider cache durations. Estimates cover recorded conversation responses, excluding auxiliary title or compaction calls and unrecorded failed requests; they are not provider bills. Refreshing Team reloads usage, and runtime or current-session response changes also refresh it.

The composer context indicator uses the selected model's context capacity and the last reported response's input, output, cache and reasoning usage. Later requests and compaction can change actual usage; it does not predict the exact size of the next request.

## Tool output budgets and file read caching

The session log plugin's history tool output budget defaults to 48,000 characters across results sent to the model, favoring recent results. Set 0 to disable this aggregate limit; the per-result limit still applies. Truncated results are labeled and retain call/result pairing. Durable history and full output attachments remain intact.

The local file plugin's read cache is a per-session byte budget and is disabled by default; 8,388,608 enables 8 MiB. Reads still check workspace paths, permissions, file type and file metadata. Tool writes and detected external changes invalidate entries. Directories, searches, commands and failures are not cached. A program that deliberately preserves all visible file metadata can evade version checks; disable caching when that behavior is expected.

## AST code maps

`code_map` reads specified Rust, TypeScript/TSX, JavaScript/JSX, Python and Go files. It returns symbol positions, nesting, imports and syntactic call targets without executing project code. For example: `{"paths":["src/lib.rs","web/src/app.tsx"]}`.

Defaults are 16 files and 256 KiB per file. Truncation and parse errors are reported; partial results are not a complete dependency graph. Dynamic calls are not resolved through type inference. The plugin is enabled locally by default. For managed execution, enable `code-map` and ensure Worker policy allows `ternilo.tools.code_map`.

## Prompt caching and reasoning observation

The agent loop exposes two options, both off by default. Prompt prefix caching adds Anthropic Messages cache markers to stable system instructions, with session identity and dynamic tool information following the stable block. Other protocols retain upstream behavior. Cache usage depends on provider reporting and does not guarantee savings.

Automatic reasoning observation records a suggested effort and its evidence for each step. It uses current-turn input length, tool calls and errors. This deterministic observation does not switch models, change actual reasoning effort or check which effort levels the model supports. Change actual effort using the main or role model picker.

## Project memory and history search

Project memory stores stable facts, conventions and verification commands. Use conversation history search for past discussions and execution details. With `ternilo.tools.project_memory` enabled, `read_project_memory` searches entries in `.ternilo-memory.json` at the workspace root. `update_project_memory` requires workspace write permission and the `expected_revision` returned by a read. A stale revision is rejected; read again before deciding how to merge. Use `content: null` to remove an entry.

Sessions using the same workspace directory share memory. Defaults are 64 entries and 4,096 characters per entry, configurable in plugin settings. Exclude credentials and temporary execution state. The plugin is enabled locally by default; managed execution requires it to be enabled and allowed by Worker policy.

## Git and GitHub workflow

`git_inspect` reads repository status, diffs, logs and commits. `git_commit_paths` commits only explicitly selected paths and preserves unrelated staged work. Inspect changes and run relevant checks before selecting paths and a commit message. Host shell and workspace permissions still apply.

The GitHub plugin is off by default. `github_list_prs` and `github_review_pr` read PRs, changed files and checks; pagination flags indicate incomplete results. A head change during review requires a fresh review. `github_create_pr` defaults to a draft. `github_merge_pr` requires the reviewed head SHA and checks the returned `merged` flag rather than treating every successful HTTP response as a merge.

Configure an API URL and a credential reference resolved on the execution host, not a literal token. Public reads can omit credentials. Private reads and publishing require suitable credentials and repository permissions; publishing also uses normal tool authorization. Nodes use credentials stored on that computer. Current managed Workers have no external network access: perform GitHub operations locally or on a connected computer. Enabling the plugin in an isolated Worker returns a clear restriction for GitHub calls without blocking other tools.

## Render and review documents

Enable `ternilo.tools.document_review` and install Chrome/Chromium on the execution host. In a Worker, the browser must be available inside its existing isolation boundary. The browser command defaults to `chromium` and can be an executable name or path. Use a model that accepts images. The plugin is off by default locally and in managed execution; Worker policy must also allow it.

Write Markdown or static HTML, call `render_document`, inspect every screenshot, correct the source and render again, then call `record_document_review` with a verdict and findings. Source is limited to 512 KiB, viewport width to 320–1920 pixels, viewport height to 480–1600 pixels, and output to eight screenshot pages. Page scripts and external/file URL resources are blocked; embed styles and images. This tool is intended for static documents.

The conversation card shows durable screenshots, horizontal overflow, missing images, blocked resources and whether the rendered version awaits review, needs changes or was reviewed. Screenshots survive restarts and enter subsequent model requests. A changed source or newer render prevents passing an old screenshot. Detected layout/resource problems also prevent a pass. These metrics cover only part of document quality; accuracy and visual quality still require actually inspecting the screenshots.
