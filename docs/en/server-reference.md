# Server reference

Server is the authenticated entry point for remote computers, resources and optional managed execution. It hosts native/OIDC login, account and tenant policy, resource sharing, model services, durable routing records and retained events. Personal project files remain on their Nodes; managed files remain in Worker storage.

## Routing and authorization

Workspace/session placement resolves a target before dispatch. A Node authenticates an outbound WebSocket using its registered identity and credential. Commands are scoped to the actor and resource, with durable results preventing ordinary reconnect replay. Account status, tenant membership and current sharing remain authoritative after delayed reads. Workspace preview requires workspace view permission independently from a conversation-only share.

Cloud jobs use admission, leases and fencing to prevent stale executors from publishing as the current owner. An uncertain external effect is not automatically replayed as a new task. WorkerPolicy defines host execution limits, allowed capabilities and quotas; model provider secrets remain in the Server-side model service.

## Storage and deployment

SQLite and PostgreSQL share the product contract. Component schema initialization is versioned, and an incompatible schema is rejected. PostgreSQL uses schema-owner initialization plus explicit `ternilo_runtime` grants and tenant-aware access. A shared PostgreSQL database does not provide complete multi-Server Node connection routing; deploy a single Server unless the missing coordination is implemented and validated.

Backups must retain the database and matching master key/configuration. Online SQLite snapshots and stopped full archives have different consistency boundaries. See [deployment](deployment.md) and [release acceptance](release-packaging.md).

## HTTP surfaces

Authenticated workbench APIs are rooted at `/api/v1`. They cover state, workspaces, sessions, events/history, queues, references, attachments, workspace browsing, sharing and configuration. Tenant-scoped requests select their tenant explicitly; a URL containing an ID is not authorization.

Native browser session management uses `GET /api/v1/auth/sessions`, `DELETE /api/v1/auth/sessions/{session_id}` and `POST /api/v1/auth/sessions/revoke-others`. Public IDs cannot be used as bearer credentials. Details include first browser identifier, first/latest observed IP and last authenticated HTTP activity. They manage native sessions only, including when the caller authenticated through OIDC.

Authentication settings use owner-only `GET/PUT /api/v1/admin/instance/authentication`. `/auth/config` returns public login metadata. Administrative account, team, enrollment and model APIs enforce their own roles. Model device authorization and model grants are separate from browser login sessions and Node credentials.

Live transport supplies authenticated state/events and uses independent cursors from bounded history paging. SDKs should follow [remote SDKs](remote-sdks.md) rather than assume mutations are safely retryable. For complete wire definitions consult `crates/ternilo-protocol/` and the route modules under `apps/ternilo-server/src/platform/` in the source tree.
