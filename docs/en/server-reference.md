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

`ResourceAccess.owner_user_id` and `is_owner` describe management, while `storage_user_id` and `is_execution_owner` describe original storage/execution identity. `ownership_revision` versions management changes. Global configuration requires the original execution identity and configuration permission. The `resource_ownership` and `resource_ownership_live` components use schema `1`; executor protocol remains `48`. Workspace details display the current manager and original storage account without changing Node bindings.

Unified browser session management uses `GET /api/v1/auth/sessions`, `DELETE /api/v1/auth/sessions/{session_id}` and `POST /api/v1/auth/sessions/revoke-others`. Public IDs cannot be used as bearer credentials. The list includes password sign-ins and locally issued OIDC sessions, with login kind, OIDC issuer, first sign-in time, session and access expiry, browser identifier, first/latest observed IP and last authenticated HTTP activity. Refresh retains the OIDC session ID, first sign-in and activity metadata without extending its original lifetime. Revocation invalidates both local access and refresh credentials; a late refresh cannot recreate the revoked session. Refreshable sessions remain visible after access expiry. `current_session_managed` distinguishes locally issued sessions from direct external IdP credentials. Revoke-others keeps the current local session across both kinds; a direct external credential can revoke all owned local sessions without revoking that credential or ending an external IdP session.

Authentication settings use owner-only `GET/PUT /api/v1/admin/instance/authentication`. `/auth/config` returns public login metadata. Administrative account, team, enrollment and model APIs enforce their own roles. Model device authorization and model grants are separate from browser login sessions and Node credentials.

Computer management uses an independent `computer_management` component. The owned `/api/v1/tenants/{tenant}/my-computers/{id}` and administrative `/api/v1/tenants/{tenant}/executors/{id}` routes support `GET` details and `PATCH {name, notes, expected_revision}`. `PUT /suspension` accepts `{suspended, expected_revision}`; `DELETE /registration` accepts `{expected_revision}` and removes registration while retaining resource bindings and history. The existing base `DELETE` revokes access. Updates reject stale versions. Suspension blocks new connections; resuming retains the original credential. Last contact uses the latest Server-observed heartbeat or authentication time. Registration accepts `{name, project_id?, ttl_seconds?}` and generates an immutable ID; names are unique per account and space. Lists omit removed entries unless `?include_removed=true` is requested. `POST /recovery` accepts `{name, expected_revision}` for revoked or removed registrations, preserving the original identity, owner, project and storage binding. See [computer management](computers.md).

Live transport supplies authenticated state/events and uses independent cursors from bounded history paging. SDKs should follow [remote SDKs](remote-sdks.md) rather than assume mutations are safely retryable. For complete wire definitions consult `crates/ternilo-protocol/` and the route modules under `apps/ternilo-server/src/platform/` in the source tree.

## Node cleanup confirmation

`POST /api/v1/executors/cleanup/sync` synchronizes account authority and cleanup requests using the Node credential and its local `storage_instance_id`. `GET /api/v1/executors/cleanup` reads that credential's snapshot; `POST` submits a receipt matching request, revocation revision and storage identity. This channel does not restore normal access for revoked credentials.

`GET /api/v1/admin/accounts/{user_id}/node-cleanup` exposes cleanup status to authorized account readers. `pending` remains unconfirmed; `confirmed` records the Node's durable receipt for supervised work. `detail` is null or one of `process_state_unknown`, `process_exit_pending`, `session_busy`, `cleanup_failed`. Raw diagnostics stay in Node logs instead of uploading machine directory paths.

## Service accounts

Space administrators can create independent service identities, issue space-scoped read or execute HTTP credentials, disable accounts and revoke individual credentials. See [service accounts](service-accounts.md) for endpoints, expiry and resource boundaries. Browser sessions and service credentials are managed separately; service identities do not inherit the creator’s private resources.

`GET /api/v1/computer-model-requests` lists cross-computer model requests related to the current account in the selected space. It accepts `query`, `cursor` and `limit`, returning `requests` and `next_cursor`. Records contain execution/source computers, the actual submitter, model owner, original resource owner and per-attempt device reports, without prompts or keys. Only the submitter or model owner can read a record, subject to current account and space access.

For managed sessions, `/api/v1/model-options` returns the saved delegated source and the current account's available grants. `current.owner_user_id` identifies the model provider; account selections use `account_provider` with `owner_user_id`. Configuration permission permits selecting one's own models or retaining the existing source, not choosing unrelated private models belonging to another account. Model-owner configuration authority is checked during request admission, retries and active execution. Execution and resource ownership remain unchanged.
