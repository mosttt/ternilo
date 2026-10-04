# Security model

Security checks are enforced by the host and Server, not delegated to model instructions or plugin claims. Authentication, account status, resource sharing, model authorization, workspace boundaries, tool approvals and operating-system isolation serve different purposes. Passing one does not imply permission for the others.

## Identity and secrets

Local browser access is loopback-only with authenticated service discovery. Remote users enter through Server. Native login, OIDC sessions, Node credentials, model API keys and the instance master key have different scopes and lifecycles. Secrets are not returned as ordinary settings fields. Backups containing configuration or keys require the same protection as the live instance.

The account session list combines password and site-issued OIDC sessions, with individual revocation, revoking other sessions and signing out of the current session. External IdP tokens and organization sessions remain managed by that IdP. A browser session list exposes public IDs and descriptive device/IP metadata, not credentials or token hashes. User-Agent is unverified, and a normal browser cannot read the host's real name. Forwarded source IPs are accepted only from explicitly configured trusted proxies. See [authentication](server-authentication.md) and [account recovery](account-recovery.md).

Native accounts support [password changes, verified email recovery, authenticators and recovery codes](account-recovery.md), with private operator recovery. Password updates revoke site sessions atomically. Session issuance rechecks the password and MFA generation. Email recovery retains MFA; linked OIDC sign-ins also require the site second factor and cannot bypass it with a raw upstream token. OIDC-only accounts use the IdP policy. Passkeys are not available.

## Files and previews

Workspace browsing requires workspace view permission; sharing only a conversation does not expose the directory. Requests identify an authorized session/workspace, then validate relative paths. Local preview access rejects traversal, links escaping the intended boundary and special files. Server rechecks workspace access after a delayed remote read before returning bytes.

Preview/download controls are not public filesystem URLs. File bytes are obtained through authenticated APIs and converted into browser-local objects. HTML/SVG/Markdown rendering is sanitized and isolated in a sandboxed frame with a restrictive policy. Image previews use permitted local blob URLs; enabling those does not make arbitrary external images or scripts trusted. A person granted workspace view permission can read/download files within that granted scope, so do not share a directory containing unrelated secrets.

## Execution

Host ceilings cannot be relaxed by presets or plugins. Read-only, workspace-write and full-access permissions govern tool behavior alongside one-time approval. Linux restricted execution uses Bubblewrap. Windows packages include a dedicated sandbox helper; keep the matching helper with the application. The policy is not a substitute for a separate VM against every host-kernel vulnerability.

Managed Worker tasks run in restricted child processes with scoped mounts and no network by default. Parent process privileges required to create the sandbox are distinct from child privileges. Model secrets remain in Server/the parent broker, not the task child. Jobs intentionally sharing a workspace still share its files.

Account bans and removal persist cleanup requests for affected Nodes. Cleanup follows the authenticated input author and account revision, preserving unrelated accounts and local inputs. Re-enabling an account does not authorize its old work. Node startup synchronizes account authority before restoring remote work, even if gateway arguments are omitted.

The account administration page shows pending cleanup, confirmation times and outstanding errors. Offline Nodes, unfinished supervised processes and unknown process state after a restart remain pending. Shell, jobs, terminals and external ACP tasks use process supervision before releasing workspace ownership. Confirmation covers registered execution resources; it does not roll back external effects.

A revoked Node credential can only synchronize its own cleanup requests and submit receipts. It cannot reconnect normally or access application data. Each credential binds one local storage instance; another data directory cannot confirm cleanup for the original. Receipts validate credential, storage identity and account revision, and repeated confirmation is idempotent.

## Extensions and durable data

Extension signatures establish publisher/package integrity; requested capabilities must still be allowed by host policy. Rhai and WASM use the same package lifecycle and capability rules. A signature alone does not authorize arbitrary network, process or credential access.

Session events and audit records preserve execution facts. Read caches and telemetry are not substitutes for authorization or evidence of a completed external action. Graceful shutdown persists results and closes database workers. Use [log repair](session-log-repair.md) only under its documented stopped/offline conditions. Dependency and license gates are part of release validation, not proof of absolute security.
