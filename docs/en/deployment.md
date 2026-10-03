# Server deployment and operations

Deploy `ternilo-server` for accounts, connected computers and shared workspaces. SQLite is the default; PostgreSQL is optional. The owner can switch single-user/multi-user mode without rebuilding the database. Personal computers run `ternilo serve`; OIDC and managed Workers are optional.

Use a single Server instance today. PostgreSQL durability does not supply complete cross-instance Node routing, connection invalidation or transparent failover. Do not infer multi-replica support from a shared database URL.

## Installation

Download the matching Server binary or pull `ghcr.io/mosttt/ternilo-server:0.2.0`. [Docker Compose](docker-compose.md) uses the published image without a local build. The optional `ternilo-deploy` helper requires Python 3.9 or newer and can run from the package's `deploy/docker/` directory.

For native deployment:

```text
ternilo-server init
ternilo-server serve
```

Initialization produces the instance configuration and setup path; retain its secrets privately. Use the actual instance directory with `--config-dir` for subsequent operator commands; the program reads `config.json` inside it. Do not overwrite an existing instance to reset a password; use [account recovery](account-recovery.md).

Expose Server through HTTPS and a reverse proxy that supports WebSocket upgrades and disables inappropriate streaming buffering. Keep personal Ternilo ports on loopback. Docker's default published Server port is also loopback for an external proxy. For accurate session IP metadata, configure only the exact trusted proxy addresses; see [settings](settings.md).

## PostgreSQL and operations

Separate schema ownership/initialization from the restricted `ternilo_runtime` role. Supply the migration connection for first installation of schema components, then run with the restricted account. The runtime must not receive unrestricted schema ownership merely to bypass a grant error. Use the repository's PostgreSQL deployment reference and matched schema contract.

Use `ternilo-deploy check`, `status`, `logs`, `up` and `down` for the initialized deployment directory. Normal `down` preserves volumes. Node offline errors, model authorization errors and database readiness failures are separate diagnoses; deleting data is not a connection repair step.

## Backups and recovery

A complete Server recovery needs database state and the matching configuration/master key. SQLite supports an online operator snapshot through `ternilo-server admin backup-sqlite`; consult its `--help` for the required output argument. Full directory/volume archives require a stopped consistent instance. PostgreSQL backups use database-native tools and must retain the matching application secrets.

Restore to an isolated environment first, validate identities/resources and test a real browser/task before replacing the original deployment. Back up connected computers' state and project files separately; Server does not own those files. Preserve managed Worker storage when recovering managed tasks.

Pin image versions or digests and retain the previous known-good backup before upgrades. An unsupported schema is rejected, not silently migrated. Rotate the instance master key, database password, Node credential and model key through their distinct workflows. See [packaging and acceptance](release-packaging.md), [Worker](worker.md) and [Server reference](server-reference.md).
