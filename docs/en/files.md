# Files, attachments and artifacts

The workspace sidebar browses the selected execution directory. In Server, this is the target computer's or managed Worker's filesystem, not the browser's local disk. File access is authorized against the current workspace/session scope. Offline metadata does not make an offline computer's files available.

## Browse and open

Use the file tree to navigate directories and open supported files in the application. Text and supported media can be previewed; downloads return actual file bytes. Search and path navigation remain within the granted workspace boundaries. The workspace panel is read-only; other conversations or external editors may still change the underlying files.

Downloads are independent of the 2 MiB preview limit: ZIP files, unsupported formats and truncated text previews all offer a complete-file download with loading, cancel and error feedback. Node and Server read in 1 MiB chunks and reject files that change during transfer. Workspace originals are not subject to the generated-file retention cap. The browser saves a Blob, so browser resource limits still apply. Workspace file links in replies, such as `output/app.zip`, open the corresponding file tab; outside-workspace and other-session paths remain inert.

Opening a file for inspection does not automatically send its full contents to the model. Use explicit references or attachments when the contents should be part of a submission. Large output can be stored as a separate artifact with a reference instead of being inserted into every message.

`write_file` creates missing parent directories inside the workspace; `content: ""` creates or clears a UTF-8 file. `replace_in_file` may also leave an empty file. Both retain zero-byte historical versions for preview, download and recovery. Paths outside the workspace, external symlinks and read-only sessions remain protected. Ordinary uploaded attachments must still be nonempty.

`code_map` defaults to `detail: "summary"`: symbols, imports and candidate calls to symbols declared in the selected files, with an omitted-call count. Use `detail: "full"` for all builtin, external and unresolved call locations. Impact analysis always uses the full parsed graph.

## Retained generated files

In Settings → Plugins → All settings, the session-log plugin exposes **Retained generated-file limit (MiB)**. Its detailed editor shares the same setting, with normal preset/session inheritance. `max_retained_file_mib` is a positive integer, defaulting to 64. It limits newly retained generated files and full tool output, not uploads or previews. Lowering it does not remove or block downloads/restoration of existing objects. Larger limits increase execution-host and Server storage and processing costs. Historical downloads read the canonical object in chunks and verify its integrity, rather than substituting the current workspace file.

## Messages and attachments

A submitted message records its selected references and attachments. Uploading from the browser stores the attachment with the application's state; project files remain in the project directory unless explicitly copied or written there. Keep the data directory in backups if attachments and generated artifacts must survive recovery.

Conversation forks preserve the selected complete history prefix and the relevant durable artifacts according to the fork operation. Archiving a conversation retains its history; it is not a filesystem cleanup command. Use the UI's explicit download/export actions when handing results to another person.

On Server, viewing or downloading remains subject to current sharing permissions. Revoking access also blocks subsequent reads; a stale browser view is not continuing authorization. See [collaboration](collaboration.md), [security](security.md) and [local backups](getting-started.md).
