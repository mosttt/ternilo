# Server deployment and operations

Deploy `ternilo-server` for accounts, connected computers and shared workspaces. SQLite is the default; PostgreSQL is optional. The owner switches single-user/multi-user mode in Platform administration → Instance settings without recreating accounts or resources. Personal computers run `ternilo serve`, retaining their own files, models, extensions and sessions. OIDC and managed Workers are optional.

Choose [native binaries](#native-deployment) or [Docker Compose](docker-compose.md). They run the same Server. Docker pulls `ghcr.io/mosttt/ternilo-server:0.2.10` without a local build; native deployment needs no Docker, Python, Rust or Node.js. For multiple Servers, see the explicit shared-database/master-key and routing configuration in [Server clustering](server-cluster.md). Cross-host storage takeover and transparent failover remain separate capabilities.

## Native deployment

Download the matching Server archive and install its executable on `PATH`. Examples use the installed `ternilo-server` name on every platform. On Linux or macOS:

```bash
ternilo-server serve \
  --config-dir "$HOME/.local/share/ternilo-server" \
  --listen 127.0.0.1:4321
```

Windows PowerShell:

```powershell
ternilo-server serve --config-dir "$env:LOCALAPPDATA\ternilo-server" --listen 127.0.0.1:4321
```

For a remote server, first configure an HTTPS reverse proxy using upstream `http://127.0.0.1:4321`. Start Server, copy its Initialization Key from the logs, open the actual domain from your computer or phone, choose a database and create the administrator. The server does not need a desktop or browser. First start directly serves the setup page; no separate setup command is required. Setup, login and the workbench follow the browser’s preferred Chinese/English language, with saved manual choices taking precedence.

`--public-url https://ternilo.example.com` optionally presets the public origin for login callbacks and email links. It does not configure HTTPS, forwarding or listening. Web setup and later instance settings can save it instead.

For terminal-only setup, run `ternilo-server setup --config-dir <instance-directory>` and enter database credentials and the administrator password through hidden prompts. Automation uses `setup --non-interactive --owner-username ... --owner-email ... --owner-password ...` or its environment variables. This is a one-time command and refuses to overwrite an existing configuration. Daily `serve` does not accept administrator credentials and never resets accounts on restart.

`--config-dir` selects an instance directory containing `config.json`. Defaults are `$XDG_DATA_HOME/ternilo-server/config.json`, or `$HOME/.local/share/ternilo-server/config.json`. The default SQLite file is `data/db/server.sqlite3` inside that directory. Running without a subcommand also starts Server.

CLI options take precedence over environment, saved configuration and defaults. Startup overrides do not rewrite configuration or migrate database data. Keep long-lived overrides in the service command or Compose environment. OIDC, Turnstile and email settings can be changed by the owner in the [web administration page](server-authentication.md).

## Docker deployment and web setup

The [Compose guide](docker-compose.md) provides complete SQLite, external PostgreSQL and included PostgreSQL examples. The default path is `docker compose up -d --wait`, then read the log Key and finish setup through the actual browser address. No separate initialization container is needed.

`/var/lib/ternilo/config.json` in the Server data volume stores database connections and the master key. Administrator identity and password hashes are stored in the database. Preserve both. A configuration file alone does not prove setup is complete: the database's instance-owner record is authoritative. If configuration exists but owner creation was interrupted, the protected account setup page remains available. Before owner creation, each start logs a fresh Key; after creation, setup is disabled.

The optional `ternilo-deploy` helper requires Python 3.9+. Its `init` action prepares a deployment directory and persistent configuration through Server's `setup`; `up` starts it. Use `status`, `logs`, `backup` and `restore` from that helper's generated directory. Ordinary Compose deployment does not need the helper.

## PostgreSQL

First create an empty database, a schema owner and a restricted runtime account. A database administrator can run:

```sql
CREATE ROLE ternilo_runtime NOLOGIN;
CREATE ROLE ternilo_owner LOGIN;
\password ternilo_owner
CREATE ROLE ternilo_app LOGIN;
\password ternilo_app
GRANT ternilo_runtime TO ternilo_app;
CREATE DATABASE ternilo OWNER ternilo_owner;
\connect ternilo
REVOKE CREATE ON SCHEMA public FROM PUBLIC;
GRANT USAGE ON SCHEMA public TO ternilo_runtime;
```

Do not recreate roles that already exist. `ternilo_runtime` is the fixed group granted access by schema initialization. Your runtime login name may vary but must inherit this group. It must not own the database, tables or schema, or have superuser/BYPASSRLS rights. The schema owner prepares tables and functions; the runtime account handles requests.

Enter both URLs in web setup, targeting the same database:

```text
postgresql://ternilo_app:APP_PASSWORD@10.0.0.20:5432/ternilo
postgresql://ternilo_owner:OWNER_PASSWORD@10.0.0.20:5432/ternilo
```

URL-encode special password characters. Native binaries use a hostname reachable from the host; Docker uses one reachable from its container. External-host and Compose service-name examples are in the [database guide](docker-compose.md). Connections and the master key are persisted in configuration; environment variables are optional.

Schemas are versioned; incompatible existing schemas are rejected rather than silently deleted or rebuilt. Back up before upgrades and validate against a separate database. Same-version recovery and cross-version data migration are different operations.

## Reverse proxy and TLS

Set the public URL to the browser's final HTTP/HTTPS root origin, such as `https://ternilo.example.com`, without a path, query or fragment. Server serves HTTP internally; an external gateway handles certificates and HTTPS. Point DNS at the gateway.

| Gateway location | HTTP upstream | Server listening / publication |
|---|---|---|
| Directly on the Server host | `http://127.0.0.1:4321` | Native loopback listener or default Docker loopback publication |
| Container on Server's Docker network | `http://server:4321`, or a unique network alias | Container port `4321`; gateway localhost refers to the gateway itself |
| Another gateway machine | `http://SERVER_PRIVATE_IP:4321` | Listen/publish on the host's private address and restrict upstream access to the gateway |

Caddy on the Server host:

```caddyfile
ternilo.example.com {
    reverse_proxy 127.0.0.1:4321 {
        flush_interval -1
    }
}
```

Caddy manages HTTPS for the domain and automatically proxies WebSockets when DNS and certificate issuance connectivity are configured. See the [official reverse-proxy documentation](https://caddyserver.com/docs/caddyfile/directives/reverse_proxy).

For Nginx, place the following in the `http` configuration context, replacing domain and certificate paths:

```nginx
map $http_upgrade $connection_upgrade {
    default upgrade;
    ''      close;
}
server {
    listen 443 ssl;
    server_name ternilo.example.com;
    ssl_certificate /path/to/fullchain.pem;
    ssl_certificate_key /path/to/privkey.pem;
    location / {
        proxy_pass http://127.0.0.1:4321;
        proxy_http_version 1.1;
        proxy_set_header Host $host;
        proxy_set_header X-Forwarded-Proto $scheme;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header Upgrade $http_upgrade;
        proxy_set_header Connection $connection_upgrade;
        proxy_buffering off;
        proxy_read_timeout 3600s;
        proxy_send_timeout 3600s;
    }
}
```

Proxy the entire root, including `/api/v1/live` and `/api/v1/executors/connect`. Nginx requires explicit upgrade headers; see its [official WebSocket guidance](https://nginx.org/en/docs/http/websocket.html). Configure the exact trusted proxy addresses for accurate client IP metadata, as described in [settings](settings.md).

For a container gateway, join it and Server to the same network and use the Server service name/alias rather than `127.0.0.1`. [Compose deployment](docker-compose.md) includes a complete network overlay and temporary SSH forwarding example. Opening localhost on your own computer cannot reach a remote server without a tunnel.

## Backups and recovery

Normal Compose `down` retains volumes; `down -v` deletes them. A complete Server recovery needs database state and its matching configuration/master key. Stop Server before archiving SQLite files or the complete instance volume, and keep any database configured outside the instance directory as well. PostgreSQL uses database-native dumps plus the matching private configuration.

SQLite also supports an online operator snapshot through `ternilo-server admin backup-sqlite`; use `--help` for its output argument. The optional deployment helper supports `backup`, `verify` and `restore`; supply a PostgreSQL custom-format dump with `--database-dump` while Server is stopped, using matched `pg_dump`/`pg_restore` tools.

Restore to an isolated environment first, verify identities and resources, and perform a real browser/task check before replacing the original instance. Back up connected computers' local state and project files separately. Preserve managed Worker storage consistently with Server state. See [Worker backup](worker.md), [account recovery](account-recovery.md), [packaging](release-packaging.md) and [Server reference](server-reference.md).

Pin image versions or digests. Apply changes to Compose or `.env` with `up -d --wait`; `restart` does not reread those files. Configuration overlays do not migrate databases. Do not create a new identity or master key to repair an existing instance.
