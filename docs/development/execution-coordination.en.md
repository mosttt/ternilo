# Work resources and execution boundaries

English · [简体中文](execution-coordination.md)

Status: design reservation, not implemented. Existing executor identities, versioned transport, workspace routing and model authorization are integration boundaries. There is no Work executable, management page, database resource or Docker scheduler.

## Resource model

A Work represents a user's hosted computer, not a project, workspace or Harness run. The proposed default is one Work per user, adjustable by quota; stopped Works still count. Each Work maps to one persistent Docker container with multiple workspaces, sessions and Harness processes. Workspaces inside it do not automatically receive separate isolation or file copies.

Work IDs are independent of container IDs. Creation binds a Work to an execution node. A host management process owns Docker lifecycle operations; Server owns identity, admission and routing. Reuse project ownership and executor routing without impersonating registered Nodes; validate credential types separately.

## Container boundaries

Work containers are peers. They do not run Docker daemons, mount the host Docker socket, use privileged mode or join host namespaces. Docker inside Work is outside this design.

The base image supplies initial tools without requiring every user program to be preinstalled. User privileges, installation policy and software persistence are not yet defined. Workspace volumes, container writable layers and rebuilt software environments are separate concerns; rebuilding does not guarantee preservation of unpersisted content.

A Work is a personal execution trust boundary. Workspaces within one container are not tenant isolation boundaries.

## Contracts

Retain actual actors, resource owners, authorization sources, model bindings and settlement. Clients, Server, Nodes and execution hosts must use compatible protocol versions.

Stopping a task, proving process exit, releasing execution capacity and deleting persistent storage are distinct operations. Lease expiry is not exit proof; reconnect must not replay unknown side effects; unknown model usage must not be refunded automatically.

The existing optional `ternilo-worker` isolates individual tasks using child processes and Bubblewrap. It does not implement persistent Work containers. See the [Worker guide](../worker.md).

Node inventory, Work lifecycle, per-user count quotas, image management and storage recovery require separate implementation and platform acceptance before becoming supported APIs.

## Integration seams

- `crates/ternilo-transport`: keep versioned handshakes and capability negotiation. Adding Work requires an explicit executor kind, credential validation and a protocol version change; do not reinterpret `EdgeNode` or `CloudWorker`.
- `crates/ternilo-control`: retain separate account, owner, actor and model-beneficiary identities. Enforce Work quotas and lifecycle admission in Server transactions, not client-side counts.
- `apps/ternilo-server/src/platform/workbench`: preserve common workspace/session APIs and add an execution-placement adapter. Container IDs, host paths and Docker credentials are not client authorization inputs.
- `crates/ternilo-local::LocalApplication`: reuse session, tool, queue and cancellation behavior. Container creation, start, stop and rebuild belong to the external host manager.

The first implementation acceptance flow should create a persistent Work, bind a workspace, complete a file task, stop and restart with the same resources, and reject access after revocation. Rebuild and volume deletion require separate acceptance. Reject incompatible protocols explicitly. These are design constraints, not currently available Work APIs.
