# Server reference

Server is the authenticated entry point for remote computers, resources and optional managed execution. It hosts native/OIDC login, account and tenant policy, resource sharing, model services, durable routing records and retained events. Personal project files remain on their Nodes; managed files remain in Worker storage.

## Routing and authorization

Workspace/session placement resolves a target before dispatch. A Node authenticates an outbound WebSocket using its registered identity and credential. Commands are scoped to the actor and resource, with durable results preventing ordinary reconnect replay. Account status, tenant membership and current sharing remain authoritative after delayed reads. Workspace preview requires workspace view permission independently from a conversation-only share.

Cloud jobs use admission, leases and fencing to prevent stale executors from publishing as the current owner. An uncertain external effect is not automatically replayed as a new task. WorkerPolicy defines host execution limits, allowed capabilities and quotas; model provider secrets remain in the Server-side model service.

## Storage and deployment

SQLite and PostgreSQL share the product contract. Component schema initialization is versioned, and an incompatible schema is rejected. PostgreSQL uses schema-owner initialization plus explicit `ternilo_runtime` grants and tenant-aware access. [Cross-Server Node routing](server-cluster.md) requires explicit instance origins and a shared master key; its network/browser and restricted PostgreSQL acceptance remains pending, so released production deployments continue using one Server.

Backups must retain the database and matching master key/configuration. Online SQLite snapshots and stopped full archives have different consistency boundaries. See [deployment](deployment.md) and [release acceptance](release-packaging.md).

## HTTP surfaces

Authenticated workbench APIs are rooted at `/api/v1`. They cover state, workspaces, sessions, events/history, queues, references, attachments, workspace browsing, sharing and configuration. Tenant-scoped requests select their tenant explicitly; a URL containing an ID is not authorization.

Workspace/session `/sharing/ownership` supports `GET` for the current manager and revision, `GET /candidates` for paginated recipient search, and `PUT` for [management handoff](resource-management.md). Handoff requires an active writable recipient in the same team and the fields `owner_user_id`, `expected_owner_user_id`, `expected_revision` and `retain_previous_owner`; stale revisions return `409 conflict`. Retaining the former manager grants editing access without share management, deletion or further handoff. Projects continue to follow space roles.

`ResourceAccess.owner_user_id` and `is_owner` describe management, while `storage_user_id` and `is_execution_owner` describe original storage/execution identity. `ownership_revision` versions management changes. Global configuration requires the original execution identity and configuration permission. The `resource_ownership` and `resource_ownership_live` components use schema `1`; executor protocol remains `45`. Workspace details display the current manager and original storage account without changing Node bindings.

Native browser session management uses `GET /api/v1/auth/sessions`, `DELETE /api/v1/auth/sessions/{session_id}` and `POST /api/v1/auth/sessions/revoke-others`. Public IDs cannot be used as bearer credentials. Details include first browser identifier, first/latest observed IP and last authenticated HTTP activity. They manage native sessions only, including when the caller authenticated through OIDC.

Authentication settings use owner-only `GET/PUT /api/v1/admin/instance/authentication`. `/auth/config` returns public login metadata. Administrative account, team, enrollment and model APIs enforce their own roles. Model device authorization and model grants are separate from browser login sessions and Node credentials.

Computer management uses an independent `computer_management` component. The owned `/api/v1/tenants/{tenant}/my-computers/{id}` and administrative `/api/v1/tenants/{tenant}/executors/{id}` routes support `GET` details and `PATCH {display_name, notes, expected_revision}`. `PUT /suspension` accepts `{suspended, expected_revision}`; `DELETE /registration` accepts `{expected_revision}` and removes registration while retaining resource bindings and history. The existing base `DELETE` revokes access. Updates reject stale versions. Suspension blocks new connections; resuming retains the original credential. Last contact uses the latest Server-observed heartbeat or authentication time. See [computer management](computers.md).

Live transport supplies authenticated state/events and uses independent cursors from bounded history paging. SDKs should follow [remote SDKs](remote-sdks.md) rather than assume mutations are safely retryable. For complete wire definitions consult `crates/ternilo-protocol/` and the route modules under `apps/ternilo-server/src/platform/` in the source tree.

## Node cleanup confirmation

`POST /api/v1/executors/cleanup/sync` synchronizes account authority and cleanup requests using the Node credential and its local `storage_instance_id`. `GET /api/v1/executors/cleanup` reads that credential's snapshot; `POST` submits a receipt matching request, revocation revision and storage identity. This channel does not restore normal access for revoked credentials.

`GET /api/v1/admin/accounts/{user_id}/node-cleanup` exposes cleanup status to authorized account readers. `pending` remains unconfirmed; `confirmed` records the Node's durable receipt for supervised work. `detail` is null or one of `process_state_unknown`, `process_exit_pending`, `session_busy`, `cleanup_failed`. Raw diagnostics stay in Node logs instead of uploading machine directory paths.
