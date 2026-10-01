# Local quickstart

[简体中文](../zh-CN/getting-started.md)

Use this guide to start Ternilo on your computer, select a project folder and complete a first model task. Local use requires only the client: no Server, Worker, database service or account login. For several computers or VPS instances, continue with [remote access](remote-access.md). The same [Server](deployment.md) supplies a shared entry point for individuals and teams.

## Start the client

Download the archive for your operating system from [Releases](https://github.com/mosttt/ternilo/releases). Windows uses ZIP; Linux/macOS use tar.gz. Extract it and run the executable in `bin`:

```sh
./bin/ternilo serve
```

On Windows, run `bin\ternilo.exe serve` and keep `ternilo-sandbox-windows.exe` beside it. Alternatively, add the extracted `bin` directory to PATH and run `ternilo serve`. Running `ternilo` without a subcommand also starts `serve`.

The CLI service occupies its terminal; Ctrl-C stops it gracefully. Closing the browser does not stop the service. Open the address printed by the program, normally `http://127.0.0.1:3210`. If the desktop application already started the service, run `ternilo status` and use its existing address.

Run `ternilo serve --open-browser` to open the system's default browser after the local web service is ready; `--open-brower` is also accepted. The URL uses the actual listening port, including a port assigned with `--listen 127.0.0.1:0`. This flag applies only to the current launch and is not saved to `config.json`. The local web UI must be enabled, so it cannot be used with `--no-local-web`. If opening the browser fails, the service keeps running and prints an address you can open manually.

Prebuilt executables do not require Rust or Node.js. File search requires `ripgrep`; verify `rg --version` in the service environment. Linux command execution, background jobs and terminals also require `bubblewrap`. See [programs in the package](binaries.md) for the plugin CLI and Windows sandbox helper.

The client management interface is loopback-only. Do not bind it to `0.0.0.0`, a LAN address or a public interface. Other devices connect through Server, with your computer establishing the outbound connection; port 3210 does not need public exposure.

The local browser obtains a temporary access token automatically. Restarting the service changes it; the interface retrieves fresh local credentials, retains unsent drafts and reconnects its live stream. If reconnection fails, check that the service is running and retry or refresh. A new-version notification refers to Web assets, not your model API key or Server login.

## Select a workspace

Choose a working folder from the empty workbench or the folder control beside Workspaces, then create a session. In a normal browser, the service validates the supplied path. The desktop application uses the operating system's directory picker.

The workspace sets the scope for file and terminal operations. Permissions determine access outside it. Removing a workspace registration does not delete the directory; its sessions become ungrouped. Registration does not move project files into Ternilo's data directory.

One local user can run several sessions in overlapping directories, but those sessions share the same files. Use separate directories or worktrees when concurrent edits would conflict. Through Server, overlapping directory access by different accounts waits for admission; ownership does not create an isolated file copy.

## Configure a Provider and model

Open Settings → Models → Add Provider. A Provider describes a model endpoint, credentials and model catalog.

| Field | Example | Meaning |
|---|---|---|
| Provider ID | `my-provider` | Stable lowercase identifier; immutable after creation |
| Display name | `My Provider` | Workbench label |
| API base URL | `https://api.example.com/v1` | Base path, not a complete `/responses` or `/chat/completions` URL |
| Protocol | `openai-chat-completions` | Must match the endpoint; also supports `openai-responses`, `deepseek-responses`, `google-gemini`, and `anthropic-messages` |
| API key | Your service key | May be empty for endpoints without authentication |
| Timeout/retries | Defaults initially | Adjust for the actual upstream service |

Enter the Provider's context-window and maximum-output-token defaults, then add at least one real upstream Model ID. Configure reasoning levels only if the upstream supports them. The [model guide](models.md) covers native Gemini/Claude addresses and protocol-specific reasoning values.

When several models share reasoning levels, configure enabled levels and their upstream values at Provider level. A UI level such as `high` can map to an upstream value such as `ultra`; disabled levels are neither shown nor sent.

Each model can choose its context window, output limit and reasoning configuration independently:

- Automatic: use explicit upstream metadata for that field, otherwise inherit Provider defaults. The interface identifies the source.
- Custom: override that field only. Returning to Automatic removes the manual override.
- Reasoning Disabled: explicitly declare no reasoning levels. Missing upstream metadata is different from an explicit disabled setting.

Context-window size drives the context indicator and automatic compaction. The default compaction trigger is 80% of the selected model's declared window; no declared window means no fallback fixed token threshold. Manual compaction remains available. Output limits and reasoning selections enter the actual model request.

Fetch available models requests the Provider catalog through the local service. Stored keys never reach the browser. Refreshing upstream metadata preserves custom names and manual overrides. If the endpoint has no catalog API, add models manually.

After saving, the key field clears. Keys are stored separately from model configuration and are not included in session exports. Leaving a key field empty while editing retains the existing key; supplying a value replaces it. There is no need to duplicate Provider keys in the Credentials page.

Finally, select Provider, model and reasoning level below the composer or in the current session model setting. If no model is usable, the workbench retains the draft and asks you to configure one before submission.

### Credentials and login

Most model users do not need this page. Plugin Login handles plugin-declared OAuth, device-code and one-time authorization flows. Named local credentials let non-model services resolve stable secret references. Provider-created references are informational. A nonempty environment variable with the same name takes precedence and is read-only from the interface.

## Complete a real task

Ask the agent to list the project's main directories and identify where to start reading. A tool-free question is enough for an initial model connection check. Responses stream into the conversation. Then verify file reading and stopping; test image input if needed. Open the trace view for detailed model/tool requests, timing and usage.

| Failure | Check |
|---|---|
| 401/403 | API key, account privileges and model access |
| 404 | Base URL, required `/v1`, and accidentally appended request paths |
| Model not found | Exact upstream Model ID |
| Missing credential | Saved key, empty environment value or mismatched reference |
| Response format mismatch | Correct Chat, OpenAI Responses, DeepSeek Responses, Gemini or Claude protocol |
| Timeout/retries exhausted | Endpoint, connectivity and proxy before increasing limits |
| Sandbox unavailable | Linux bubblewrap or the platform's packaged sandbox helper |

Compatible APIs still differ between vendors. Use small real requests to validate credentials, and do not paste keys into conversations, logs or repository files.

## Data and backup

The default data directory is `$XDG_DATA_HOME/ternilo` when XDG_DATA_HOME is set, otherwise `~/.local/share/ternilo`. It contains workspace registrations, history, model/plugin settings, credentials, attachments and schedules. Project source files normally live elsewhere.

Back up both the whole Ternilo data directory and the project directories you need. Before copying data, run `ternilo stop`, supplying the same `--data-dir` used at startup, and confirm success. Independent RPC/ACP processes using the directory must exit too. Closing a desktop window or copying one SQLite file is not a complete stopped backup.

Keep external Profiles, service-manager settings and environment-based configuration separately. A Node also needs its computer ID, Server address and one-time-displayed connection credential. Protect backups as private data.

To restore, stop all users of the target data directory, restore the complete backup into the original location or an empty directory, and restore project files to their original absolute paths. Use a matching program version:

```sh
ternilo serve --data-dir /path/to/restored-ternilo-data
```

Verify workspaces, history, model catalogs and credentials. The local browser token is regenerated; it does not need restoring. Node credentials are different: restore the remote startup settings and confirm the computer appears online in Server. Readable cached history alone does not prove remote connectivity. Server has its own [backup procedure](server-reference.md).

Restoring history does not restart a process. Unsettled runs are marked interrupted; started tool calls without recorded results become unknown and are not automatically repeated. Schedules recover under their own rules. Inspect history and external side effects before resubmitting interrupted work.

## Inspect and stop the service

```sh
ternilo status
ternilo stop
```

Use the same explicit data directory when applicable. Status reports the current address and service identity without the API token. Stop waits for cancellation and cleanup and releases the directory. A timeout after 30 seconds is an error; inspect the service log. Only then back up, replace or restart the data directory.

Desktop and CLI can share a service when user, data directory and version match and its listener remains on `127.0.0.1`. Opening Desktop does not replace an existing service's Profile or arguments. Stop and restart to change those settings. [Desktop installers](desktop.md) and portable CLI packages are separate downloads.

## Optional startup settings

| CLI | Environment | Applies to |
|---|---|---|
| `--listen ADDRESS` | `TERNILO_LOCAL_LISTEN` | serve; defaults to `127.0.0.1:3210` |
| repeated `--profile PATH` | `TERNILO_LOCAL_PROFILES` | serve/run/rpc/acp; comma-separated environment list |
| `--data-dir PATH` | `TERNILO_LOCAL_DATA_DIR` | serve/status/stop/rpc/acp |
| `--max-steps N` | `TERNILO_LOCAL_MAX_STEPS` | execution commands; 0 means no additional host ceiling |

Explicit arguments override environment values. Prompt text and `run --json` are per-invocation values. Change only the port to resolve a listener conflict; a new data directory creates a separate environment without copying models or sessions.

```sh
ternilo serve --listen 127.0.0.1:3211
```

To add remote access, use the existing service with `--gateway-url`, `--token` and `--node-id`, retaining its data directory. Do not start a second writer against the same directory.

## Build from source

Place the pinned [Linorun](https://github.com/mosttt/linorun) revision beside Ternilo:

```text
source/
├── linorun/
└── ternilo/
```

The repository uses Rust 1.98.0 through rust-toolchain.toml. Initial compilation needs registry access and Node.js to build Web assets; executing the packaged application does not. Install ripgrep and, for Linux tool execution, bubblewrap.

```sh
rustc --version
npm --prefix web ci
npm --prefix web run build
cargo run --locked -p ternilo -- serve
```

Continue with the [user guide](user-guide.md), [remote access](remote-access.md), [Server deployment](deployment.md), or [desktop guide](desktop.md).

See [configuration and data directories](data-layout.md) for startup settings, credentials, databases and runtime state. The first `serve` saves configuration; subsequent launches prefer CLI, environment, configuration and then built-in defaults.
