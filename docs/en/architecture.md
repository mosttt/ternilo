# Architecture

`ternilo` owns the local application and adapts it to CLI, local Web, JSON-RPC, ACP and a remotely connected Node. `ternilo-server` supplies the authenticated remote entry point, accounts, resources, routing and model services. `ternilo-worker` optionally executes managed jobs. Desktop is a native shell around the local application; Work container lifecycle is a reserved design, not an implemented fourth execution product.

## Layers and data ownership

The protocol crate defines product identities, commands, events and validation. The kernel composes capabilities and executes Agent turns. Linorun supplies component lifecycle and service routing. Built-in and external plugins contribute tools, prompts, skills, models and runtimes subject to host policy.

`LocalApplication` is the shared local service boundary. It owns workspaces, sessions, providers, credentials, presets, attachments and extension inventory. Its data directory is exclusively locked by one service. Project directories are separate from that state directory; multiple independently configured services may run on the same computer.

Durable session events are the history source. Search indexes and projection checkpoints accelerate reads without replacing the events. History reads are bounded and support backward pagination; live cursors are independent from historical pagination. Forks copy a completed history prefix and preserve their own identity. Queue submissions and results are persisted so reconnects do not replay completed external actions.

Cloud session statistics use a database checkpoint and the same statistics projection. The first read replays events, later reads consume only appended events, and unchanged reads return the cached result. History regeneration, checkpoint version changes or invalid state rebuild from the event journal. Cached reads still check current view permissions. Full plugin projections continue to replay history; a Live request combining statistics and full projections shares one history read to keep both at the same data cut.

## Committed session facts

The internal Sessions service is `ternilo/sessions@2`. It provides a lightweight summary, per-run boundaries/terminal state/primary input facts, current-run events and exact-sequence reads. Admission, input-author validation and inbox settlement no longer copy the full transcript. Facts are rebuilt after history validation/recovery and updated under the log lock only after durable event commit. Steering does not replace the run's primary author; scheduled work still validates its creation and dispatch events.

Custom core plugins using the Sessions service must adapt to `ternilo/sessions@2` and rebuild. Executor protocol 52 replaces 51; Server, Node and Worker require matching upgrades, while the independent maintenance channel remains available to update older executors. New subagent inputs record `source.kind=subagent` and the parent run. Older binaries cannot safely read histories containing these records; retain a pre-upgrade backup for rollback. The new version reads existing history, with no database migration.

Each automated delegation receives a distinct run ID; explicitly accepted human follow-ups retain their authorized run ID. Model and web-credential requests carry ordered origin proofs. Server verifies synchronized child inputs, registered parent relationships and schedule creation/dispatch events against the original account authority rather than treating child and parent run IDs as interchangeable or inferring the author from the latest input.

## Execution and models

Profiles compose plugin configuration. A host policy defines permissions and maximum limits that a plugin cannot loosen. Workspace access, process sandboxing, approvals and model authorization are distinct checks. Large outputs and attachments are stored as artifacts rather than forced into every model context.

Device providers remain on the execution computer. Account providers and published platform models are resolved through Server. A run records its selected model and authorization; a same-named model from another source is not an automatic fallback. Shared and scheduled work preserves the original submitter identity and rechecks current permissions.

Workspace and session [management handoff](resource-management.md) changes management within a team without rewriting storage/execution identity, Node bindings or historical authors. The current manager controls sharing and resource deletion; original execution configuration and Provider credentials require their own authorization. Sessions can inherit workspace management or retain an independent manager.

## Remote paths

The Node opens an outbound authenticated WebSocket. Server associates the connection with a registered executor and routes authorized workspace/session commands to it. Server stores received metadata/events for remote viewing; actual computer files remain on the Node. Routing and result authorization are checked against current account and resource state, including after remote waits.

Node maintenance uses an independently versioned HTTPS Hello/Poll protocol with the enrolled identity, without requiring a successful execution WebSocket handshake. Server persists the maintenance identity and short-lived command/reply mailbox. Signed updates remain deliverable when execution is incompatible; completion requires restored execution connectivity. The maintenance protocol carries no application operations. See [client updates](client-updates.md).

SQLite and PostgreSQL implement the same product storage contract. Component schemas are initialized explicitly; an incompatible existing schema is rejected rather than guessed or silently rewritten. Transactions protect admission, account status, quotas and immutable audit records. PostgreSQL deployment separates schema ownership from restricted runtime access.

The `resource_live` component records space invalidations in the same transaction as direct sharing, project rules, workspace inheritance, permission groups and space memberships. The `resource_ownership_live` component adds management changes to these notifications. Other Servers using that database revalidate identity, active subscriptions and resource permissions before refreshing their workbench. Rolled-back changes produce no notification. Hints contain no session content or permission details; browsers do not periodically reload the workbench.

With an explicit per-instance `cluster_url`, Servers sharing the database and master key can [forward Node calls](server-cluster.md) to the current connection holder. Signed requests bind the instance, lease fence, request identity, time and original input authority. Transient payloads remain online; uncertain forwarding is not automatically retried. The `gateway_cluster` component coalesces Node change revisions for canonical catch-up across instances. Network, restricted PostgreSQL and two-instance browser model/tool flows have passed acceptance; this does not establish full storage failover or capacity claims.

Managed Workers claim durable work and execute in restricted child processes. Leases, fencing and durable results distinguish retries from uncertain external outcomes. The planned Work container product builds on existing executor/resource boundaries but does not yet supply container lifecycle or quotas. See [Server reference](server-reference.md), [security](security.md) and [Worker](worker.md).
