# Configuration and data directories

Each instance has a separate directory with `config.json` as its startup entry point. `config/` holds editable configuration, `secrets/` holds credentials and authorization, `data/` holds durable data, `cache/` holds rebuildable indexes, and `runtime/` holds discovery, locks and execution cleanup state. Programs create the directories they actually need.

## Local Ternilo and Node

The root is `$XDG_DATA_HOME/ternilo`, otherwise `~/.local/share/ternilo`; `--data-dir` selects another instance. Desktop, local browser and Node share the same local application stores.

| Location | Contents |
|---|---|
| `config.json` | Listener, profile layers, execution limits, Node identity and Server connection |
| `config/` | `providers.json`, `agent-presets.json` |
| `secrets/` | `credentials.json`, `credential-records.json`, `model-connections.json`, `node-authorizations.json` |
| `data/state.json`, `data/sessions/` | Workspace/session registrations and canonical JSONL event logs |
| `data/attachments/objects/`, `data/extensions/` | Attachment objects and installed extensions |
| `data/db/` | Agent Team, inbox, preferences and Node transport SQLite databases |
| `cache/` | Session search and projection SQLite indexes |
| `runtime/` | Service discovery, writer/startup locks and execution resource cleanup records |

SQLite `-wal` and `-shm` files stay beside their database. Queues, input authors, attachments and transport receipts are durable data, not disposable caches.

The first `ternilo serve` saves its effective startup settings privately. Subsequent launches resolve CLI, environment, saved configuration and built-in defaults in that order; overrides do not rewrite existing configuration. Saved relative profile paths resolve from the instance root, while CLI paths resolve from the launch directory. RPC, ACP and one-shot `run` retain their explicit startup options.

Root configuration may contain a Node credential and must remain private. New POSIX directories use `0700`; configuration and credential files use `0600`. Windows uses the current user's directory access controls. Browser appearance preferences remain browser settings. Do not clear `runtime/` while running: cleanup records and authenticated service discovery have operational meaning. Cross-process directory coordination lives separately under the system user's `ternilo/execution-locks/` state directory so different ports and data roots coordinate the same workspace.

## Server

The default instance directory is `$XDG_DATA_HOME/ternilo-server/`, otherwise `~/.local/share/ternilo-server/`. `--config-dir` or `TERNILO_SERVER_CONFIG_DIR` selects the directory; Server always reads its `config.json`. Docker uses `--config-dir /var/lib/ternilo`.

Without an explicit database URL, initialization creates `data/db/server.sqlite3` beneath the instance directory. An explicit URL can select another SQLite location or PostgreSQL. Accounts, encrypted credentials, resource authorization, queues, audit and Gateway state retain one transactional database. The deployment tool saves execution policy as `config/worker-policy.json`. The default configured workspace root is `data/workspaces/`.

The root configuration holds database, master-key and identity settings. Platform model keys remain encrypted in the database. Back up the configuration, actual database and persistent files, including any external stores.

## Worker and reserved Work design

Optional `ternilo-worker` selects its instance directory with `--config-dir` or `TERNILO_WORKER_CONFIG_DIR`, uses root `config.json` and defaults its persistent volume to `data/workspaces/`. Docker uses `/var/lib/ternilo/config.json` and `/var/lib/ternilo/data/workspaces/`. Storage identity and occupancy/recovery metadata stay with the volume they identify.

`ternilo-work` has no implemented standalone program or Docker lifecycle yet. Its reserved design uses root `config.json`, editable `config/`, private `secrets/`, persistent `data/`, rebuildable `cache/` and host-managed `runtime/`. Work state and user workspace volumes are separate; a container's working directory is not an entry point to host management credentials. See the [Work boundary design](../development/execution-coordination.md).

Use the [local backup procedure](getting-started.md), [Server backup procedure](server-reference.md) and [Worker guide](worker.md) for consistent backups.
