# Local quickstart

[简体中文](../zh-CN/getting-started.md)

Follow these five steps to complete your first task on this computer. Local use needs only the client. Add [remote access](remote-access.md) when you want to work from another device.

## 1. Start the application

Download a matching desktop installer or client archive from [Releases](https://github.com/mosttt/ternilo/releases). Open the desktop app, or install the CLI executables on `PATH`. Examples use installed program names.

```sh
ternilo serve --open-browser
```

The service opens the system browser once its local web interface is ready, normally at `http://127.0.0.1:3210`. Use `ternilo serve` to open the printed address yourself. `--open-browser` applies to this launch only; a browser-launch failure does not stop the service.

Keep the launching terminal open. Ctrl-C stops the service; closing the browser does not stop tasks. If the desktop app already started it, use `ternilo status` to find its address and reuse it.

The interface follows your browser's Chinese/English/Korean preference until you save a manual choice under Settings → General. The local page obtains its connection credentials automatically and needs no account registration.

Prebuilt applications require no Rust or Node.js. File search needs `ripgrep` (`rg`); Linux commands, terminals and background tasks also need `bubblewrap`. On Windows, keep `ternilo-sandbox-windows.exe` beside the main program; see [package programs](binaries.md). The local management port is for this computer; other devices use Server.

## 2. Select a workspace and session

Choose a working folder from the empty workbench or the folder button beside Workspaces, then open a session. Later, select that workspace and choose New session.

Files stay in the selected directory. Task permissions determine access elsewhere; removing a workspace registration does not delete disk files. A browser's paths are checked by the local service; desktop uses the system folder picker.

Sessions in the same directory share real files. Use separate directories or worktrees for independent concurrent edits. Shared-computer directory coordination is described in [remote access](remote-access.md).

## 3. Configure a Provider and model

Open Settings → Models → Add Provider. A Provider stores a model service connection and can be reused.

| Field | Example / selection |
|---|---|
| Provider ID and display name | An ID such as `my-provider`; a readable interface name |
| API address | The service's base URL, such as `https://api.example.com/v1` |
| Protocol | Match the actual upstream protocol |
| API key | Paste the service key; leave empty if authentication is not required |
| Model ID | Fetch available models or add an exact upstream ID |

Use the API base path, not a complete `/chat/completions` or `/responses` request URL. Save the Provider and choose the model below the message input. Without a usable model, the interface retains your draft and guides you through configuration.

Supported protocols, native Claude/Gemini addresses, context windows, output limits and reasoning levels are covered in [model configuration](models.md). Adjust these after the first working conversation. Model discovery runs on the service, without returning saved keys to the browser; add models manually when discovery is unavailable.

After saving, the key input is cleared. Leaving it empty during editing retains the saved key; a new value replaces it. Model configuration and session exports do not include plaintext keys.

### When to use Credentials and login

Enter model keys in their Provider. Use Credentials and login for separate plugin/service authorization or named credentials. Automatically generated Provider references show status and need no duplicate configuration. Credentials supplied through matching environment variables are read-only.

## 4. Complete a first task

Ask a simple question such as “List this project's main directories and explain where to start reading.” The conversation shows model output and tool activity progressively. Open the trajectory for detailed requests, timings and usage when diagnosing a problem.

Choose task permissions in the input bar: read-only for inspection or workspace-write for project changes. Operations requiring confirmation enter approval. Goals are enabled only by your explicit setting or `/goal`, not automatically by the model.

| Problem | Check first |
|---|---|
| `401` / `403` | API key, account and model permissions |
| `404` | Base URL and required service path |
| `model not found` | Exact Model ID |
| Timeout / retries exhausted | Network, proxy and upstream status before changing timeout |
| Tools cannot execute | `rg`, Linux `bubblewrap`, or the Windows sandbox helper |
| Incompatible request format | Provider protocol and actual endpoint |

See the [user guide](user-guide.md) for queues, files, approvals and history, or [web access diagnostics](web-access.md) for network tools.

## 5. Data and backups

Application state is separate from project files. Defaults are `$XDG_DATA_HOME/ternilo`, or `~/.local/share/ternilo`; `--data-dir` selects another location. It contains configuration, workspace registrations, sessions, models/plugins, credentials, attachments and schedules. See the [directory layout](data-layout.md).

Back up both the complete application directory and your real project directories. Stop the service with `ternilo stop` first and close RPC/ACP processes using the same directory. Preserve external Profiles, service settings and environment credentials too. Backups contain private data and need restricted access.

For recovery, stop all users of the destination directory, restore the complete state and corresponding project files, then start a matching application version. Example custom directory:

```sh
ternilo serve --data-dir /path/to/restored-ternilo-data
```

Check workspaces, history and models. Interrupted work does not automatically repeat external effects; inspect real files before resubmitting. Connected computers also need their saved registration/credential and an online connection. Server backups are separate; see [deployment](deployment.md).

## Status, stop and restart

Use another terminal:

```sh
ternilo status
ternilo stop
```

Add the same `--data-dir` to both when using a custom directory. `status` displays the address; successful `stop` releases the directory for backup or restart. Inspect the service logs if shutdown fails.

## Optional startup settings

Defaults usually suffice. Use these options for a different port, state directory or execution configuration. Precedence is CLI > environment > configuration > defaults.

| Option | Environment | Purpose |
|---|---|---|
| `--listen` | `TERNILO_LOCAL_LISTEN` | Loopback listener, default `127.0.0.1:3210` |
| `--data-dir` | `TERNILO_LOCAL_DATA_DIR` | Complete application directory; use it for status/stop too |
| `--profile` | `TERNILO_LOCAL_PROFILES` | Additional Profiles; comma-separated paths in the environment |
| `--max-steps` | `TERNILO_LOCAL_MAX_STEPS` | Additional step ceiling; default `0` means none |

```sh
ternilo serve --listen 127.0.0.1:3211
```

Independent instances need different ports and state directories. One service owns a directory; desktop and browser can share it. Remote connections retain that same state; see [remote access](remote-access.md).

## Build from source

Checkout, the pinned Linorun revision, Rust/Node.js requirements and build commands are collected in [development](contributing.md). See [packaging](release-packaging.md) for distributable applications.

## Next steps

- [User guide](user-guide.md): conversations, permissions, files and tools.
- [Remote access](remote-access.md): connect computers and VPS instances.
- [Server deployment](deployment.md) and [Docker Compose](docker-compose.md): shared entry point and databases.
- [Desktop](desktop.md): installation, windows, service lifecycle and notifications.
