# Local session log repair

[简体中文](../zh-CN/session-log-repair.md)

A power loss or process crash can leave an incomplete JSONL tail. `ternilo repair-session-log` inspects one session while its local data directory is offline. Inspection is the default; `--apply` saves the original file before making a repair.

Exit the desktop application and stop the service for the selected data directory. Independent RPC/ACP processes must also exit. Maintenance acquires the application's exclusive writer lock and rejects both inspection and changes while another process owns it.

```sh
ternilo stop --data-dir /path/to/data
ternilo repair-session-log --data-dir /path/to/data --session-id SESSION_ID
ternilo repair-session-log --data-dir /path/to/data --session-id SESSION_ID --apply
ternilo serve --data-dir /path/to/data
```

`TERNILO_LOCAL_DATA_DIR` supplies the directory when no explicit option is given; otherwise the normal client default applies. Use the session ID from the workbench or automation API; no filename encoding is needed.

Inspection validates every event and its consecutive sequence number. A complete final event missing a newline is preserved and separated for future appends. An incomplete, unterminated JSON tail is removed while all preceding bytes remain intact. Healthy logs report `none` without a backup or rewrite. Invalid complete records, blank lines, sequence gaps and unrecognized records are refused without modifying the log.

Before applying a change, maintenance writes a uniquely named `*.jsonl.repair-*.bak` beside the original, synchronizes the backup, then changes and synchronizes the transcript. Backups use mode `0600` on Unix. The JSON report includes the action, whether it was applied, valid record count, byte lengths and backup path, without transcript content. During inspection, `bytes_after` is the proposed length. Protect backups as private session data.

Repair does not recover incomplete output, redo external actions or resume interrupted tasks. It applies to canonical local/Node logs, including archived sessions; it does not repair the Server database. Reconnect the repaired computer and verify history and subsequent events. Complete synchronized events retain their sequence numbers, and archives remain archived.
