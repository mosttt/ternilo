# Files, attachments and artifacts

The workspace sidebar browses the selected execution directory. In Server, this is the target computer's or managed Worker's filesystem, not the browser's local disk. File access is authorized against the current workspace/session scope. Offline metadata does not make an offline computer's files available.

## Browse and open

Use the file tree to navigate directories and open supported files in the application. Text and supported media can be previewed; downloads return actual file bytes. Search and path navigation remain within the granted workspace boundaries. Renaming or deleting a file changes the real workspace, so consider other conversations using the same directory.

Opening a file for inspection does not automatically send its full contents to the model. Use explicit references or attachments when the contents should be part of a submission. Large output can be stored as a separate artifact with a reference instead of being inserted into every message.

`write_file` creates missing parent directories inside the workspace; `content: ""` creates or clears a UTF-8 file. `replace_in_file` may also leave an empty file. Both retain zero-byte historical versions for preview, download and recovery. Paths outside the workspace, external symlinks and read-only sessions remain protected. Ordinary uploaded attachments must still be nonempty.

## Messages and attachments

A submitted message records its selected references and attachments. Uploading from the browser stores the attachment with the application's state; project files remain in the project directory unless explicitly copied or written there. Keep the data directory in backups if attachments and generated artifacts must survive recovery.

Conversation forks preserve the selected complete history prefix and the relevant durable artifacts according to the fork operation. Archiving a conversation retains its history; it is not a filesystem cleanup command. Use the UI's explicit download/export actions when handing results to another person.

On Server, viewing or downloading remains subject to current sharing permissions. Revoking access also blocks subsequent reads; a stale browser view is not continuing authorization. See [collaboration](collaboration.md), [security](security.md) and [local backups](getting-started.md).
