# Architecture

`ternilo` owns the local application and adapts it to CLI, local Web, JSON-RPC, ACP and a remotely connected Node. `ternilo-server` supplies the authenticated remote entry point, accounts, resources, routing and model services. `ternilo-worker` optionally executes managed jobs. Desktop is a native shell around the local application; Work container lifecycle is a reserved design, not an implemented fourth execution product.

## Layers and data ownership

The protocol crate defines product identities, commands, events and validation. The kernel composes capabilities and executes Agent turns. Linorun supplies component lifecycle and service routing. Built-in and external plugins contribute tools, prompts, skills, models and runtimes subject to host policy.

`LocalApplication` is the shared local service boundary. It owns workspaces, sessions, providers, credentials, presets, attachments and extension inventory. Its data directory is exclusively locked by one service. Project directories are separate from that state directory; multiple independently configured services may run on the same computer.

Durable session events are the history source. Search indexes and projection checkpoints accelerate reads without replacing the events. History reads are bounded and support backward pagination; live cursors are independent from historical pagination. Forks copy a completed history prefix and preserve their own identity. Queue submissions and results are persisted so reconnects do not replay completed external actions.

## Execution and models

Profiles compose plugin configuration. A host policy defines permissions and maximum limits that a plugin cannot loosen. Workspace access, process sandboxing, approvals and model authorization are distinct checks. Large outputs and attachments are stored as artifacts rather than forced into every model context.

Device providers remain on the execution computer. Account providers and published platform models are resolved through Server. A run records its selected model and authorization; a same-named model from another source is not an automatic fallback. Shared and scheduled work preserves the original submitter identity and rechecks current permissions.

Workspace and session [management handoff](resource-management.md) changes management within a team without rewriting storage/execution identity, Node bindings or historical authors. The current manager controls sharing and resource deletion; original execution configuration and Provider credentials require their own authorization. Sessions can inherit workspace management or retain an independent manager.

## Remote paths

The Node opens an outbound authenticated WebSocket. Server associates the connection with a registered executor and routes authorized workspace/session commands to it. Server stores received metadata/events for remote viewing; actual computer files remain on the Node. Routing and result authorization are checked against current account and resource state, including after remote waits.

SQLite and PostgreSQL implement the same product storage contract. Component schemas are initialized explicitly; an incompatible existing schema is rejected rather than guessed or silently rewritten. Transactions protect admission, account status, quotas and immutable audit records. PostgreSQL deployment separates schema ownership from restricted runtime access.

The `resource_live` component records space invalidations in the same transaction as direct sharing, project rules, workspace inheritance, permission groups and space memberships. The `resource_ownership_live` component adds management changes to these notifications. Other Servers using that database revalidate identity, active subscriptions and resource permissions before refreshing their workbench. Rolled-back changes produce no notification. Hints contain no session content or permission details; browsers do not periodically reload the workbench.

With an explicit per-instance `cluster_url`, Servers sharing the database and master key can [forward Node calls](server-cluster.md) to the current connection holder. Signed requests bind the instance, lease fence, request identity, time and original input authority. Transient payloads remain online; uncertain forwarding is not automatically retried. The `gateway_cluster` component coalesces Node change revisions for canonical catch-up across instances. Network, restricted PostgreSQL and two-instance browser model/tool flows have passed acceptance; this does not establish full storage failover or capacity claims.

Managed Workers claim durable work and execute in restricted child processes. Leases, fencing and durable results distinguish retries from uncertain external outcomes. The planned Work container product builds on existing executor/resource boundaries but does not yet supply container lifecycle or quotas. See [Server reference](server-reference.md), [security](security.md) and [Worker](worker.md).
