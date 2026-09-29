# Security model

Security checks are enforced by the host and Server, not delegated to model instructions or plugin claims. Authentication, account status, resource sharing, model authorization, workspace boundaries, tool approvals and operating-system isolation serve different purposes. Passing one does not imply permission for the others.

## Identity and secrets

Local browser access is loopback-only with authenticated service discovery. Remote users enter through Server. Native login, OIDC sessions, Node credentials, model API keys and the instance master key have different scopes and lifecycles. Secrets are not returned as ordinary settings fields. Backups containing configuration or keys require the same protection as the live instance.

A browser session list exposes public IDs and descriptive device/IP metadata, not credentials or token hashes. User-Agent is unverified, and a normal browser cannot read the host's real name. Forwarded source IPs are accepted only from explicitly configured trusted proxies. See [authentication](server-authentication.md) and [account recovery](account-recovery.md).

## Files and previews

Workspace browsing requires workspace view permission; sharing only a conversation does not expose the directory. Requests identify an authorized session/workspace, then validate relative paths. Local preview access rejects traversal, links escaping the intended boundary and special files. Server rechecks workspace access after a delayed remote read before returning bytes.

Preview/download controls are not public filesystem URLs. File bytes are obtained through authenticated APIs and converted into browser-local objects. HTML/SVG/Markdown rendering is sanitized and isolated in a sandboxed frame with a restrictive policy. Image previews use permitted local blob URLs; enabling those does not make arbitrary external images or scripts trusted. A person granted workspace view permission can read/download files within that granted scope, so do not share a directory containing unrelated secrets.

## Execution

Host ceilings cannot be relaxed by presets or plugins. Read-only, workspace-write and full-access permissions govern tool behavior alongside one-time approval. Linux restricted execution uses Bubblewrap. Windows packages include a dedicated sandbox helper; keep the matching helper with the application. The policy is not a substitute for a separate VM against every host-kernel vulnerability.

Managed Worker tasks run in restricted child processes with scoped mounts and no network by default. Parent process privileges required to create the sandbox are distinct from child privileges. Model secrets remain in Server/the parent broker, not the task child. Jobs intentionally sharing a workspace still share its files.

## Extensions and durable data

Extension signatures establish publisher/package integrity; requested capabilities must still be allowed by host policy. Rhai and WASM use the same package lifecycle and capability rules. A signature alone does not authorize arbitrary network, process or credential access.

Session events and audit records preserve execution facts. Read caches and telemetry are not substitutes for authorization or evidence of a completed external action. Graceful shutdown persists results and closes database workers. Use [log repair](session-log-repair.md) only under its documented stopped/offline conditions. Dependency and license gates are part of release validation, not proof of absolute security.
