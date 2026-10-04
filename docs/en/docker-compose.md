# Docker Compose deployment

[简体中文](../zh-CN/docker-compose.md)

Setup and the regular workbench initially follow the browser’s preferred Chinese/English language. A saved manual choice overrides that default.

Requires Docker Engine and the Compose plugin. The public image `ghcr.io/mosttt/ternilo-server:0.2.4` supports `linux/amd64` and `linux/arm64`; Docker selects the host architecture. Rust, Node.js, Python and local builds are unnecessary.

`ghcr.io/mosttt/ternilo-server:latest` follows the latest published stable release; prereleases never replace it. These instructions pin a version by default. To follow stable releases, set `TERNILO_IMAGE=ghcr.io/mosttt/ternilo-server:latest` in the deployment directory's `.env`. Update with `docker compose -f compose.server.yml pull`, followed by `docker compose -f compose.server.yml up -d`.

Default SQLite and an existing PostgreSQL database can start without a `.env` file. Web setup saves database connections and the public URL in the volume's `config.json`. `.env.server.example` documents optional Docker settings, startup overrides and the database-role passwords required by included PostgreSQL; use the sections you need.

## SQLite: start and configure in the browser

```bash
mkdir ternilo-server
cd ternilo-server
curl -fLO https://github.com/mosttt/ternilo/releases/download/v0.2.4/compose.server.yml
docker compose -f compose.server.yml up -d --wait
docker compose -f compose.server.yml logs server
```

First start does not require a separate initialization command, administrator credentials or database options. Logs print `Initialization Key: ter_b_...`. Open the actual Server address, enter the Key, choose SQLite and create the administrator. After checking the connection and preparing schemas, Server saves its configuration and switches to the login page. Keep the Key private; it expires when setup completes.

The default published address is `127.0.0.1:4321` **on the Docker host**. For deployment on your own computer, open `http://localhost:4321`. For a remote server, configure the HTTPS gateway below and open its domain from your computer or phone. Your computer's localhost does not reach a remote server.

SQLite is stored at `/var/lib/ternilo/data/db/server.sqlite3` in the persistent data volume; configuration is `/var/lib/ternilo/config.json`. The database stores the administrator account and password hash. Configuration does not store the plaintext administrator password. Restarts use the saved configuration and existing account.

## External PostgreSQL

PostgreSQL may run on the Docker host or another database server. First [prepare the database and accounts](https://github.com/mosttt/ternilo/blob/main/docs/en/deployment.md#postgresql), then start Server with the Compose file above and choose PostgreSQL in web setup.

For a database at private address `10.0.0.20:5432`, database name `ternilo`, enter:

```text
Runtime connection URL:
postgresql://ternilo_app:APP_PASSWORD@10.0.0.20:5432/ternilo

Schema owner connection URL:
postgresql://ternilo_owner:OWNER_PASSWORD@10.0.0.20:5432/ternilo
```

Both connections must target the same database. Replace passwords and URL-encode special characters such as `@`, `:` and `/`. Use the database service's TLS requirements, for example `?sslmode=require`, with certificate verification configured as required by that service.

For PostgreSQL installed on the Docker host, Server's container must be able to reach it. On Linux, create `compose.host.yml`:

```yaml
services:
  server:
    extra_hosts:
      - "host.docker.internal:host-gateway"
```

```bash
docker compose -f compose.server.yml -f compose.host.yml up -d --wait
```

Use `host.docker.internal` as the database hostname in web setup. PostgreSQL must listen on a host address reachable from the container, with `pg_hba.conf` and the firewall allowing the actual Docker network source. Listening only on host `127.0.0.1` is insufficient. `host-gateway` only resolves an address; it does not change database access permissions or listening interfaces. Do not expose the database indiscriminately to the public internet.

## PostgreSQL included in Compose

Download the database overlay and initialization SQL into the same deployment directory:

```bash
curl -fLO https://github.com/mosttt/ternilo/releases/download/v0.2.4/compose.server.postgres.yml
curl -fLO https://github.com/mosttt/ternilo/releases/download/v0.2.4/postgres-init.sql
```

Create `.env` with **database account passwords**, separate from the Ternilo administrator password:

```dotenv
TERNILO_POSTGRES_ADMIN_PASSWORD=replace-with-a-private-database-admin-password
TERNILO_POSTGRES_OWNER_PASSWORD=replace-with-a-private-owner-password
TERNILO_POSTGRES_APP_PASSWORD=replace-with-a-private-runtime-password
```

```bash
chmod 600 .env
docker compose -f compose.server.yml -f compose.server.postgres.yml up -d --wait
docker compose -f compose.server.yml -f compose.server.postgres.yml logs server
```

Choose PostgreSQL in web setup and enter:

```text
Runtime connection URL:
postgresql://ternilo_app:APP_PASSWORD@postgres:5432/ternilo

Schema owner connection URL:
postgresql://ternilo_owner:OWNER_PASSWORD@postgres:5432/ternilo
```

Use the owner and app passwords from `.env`, URL-encoding special characters. The admin password belongs only to PostgreSQL's database administrator. `postgres` is the Compose service name and `5432` is its container port; no database host port is published. The initialization SQL creates the `ternilo_runtime` group and restricted account. Server prepares the complete schema with the owner connection and serves requests with the runtime account.

PostgreSQL persists data in `postgres-data`; retain Server's `server-data` volume for configuration and the master key as well. PostgreSQL initialization scripts run only on an empty database volume. Changing `.env` does not change passwords of existing database accounts.

## Remote servers and HTTPS gateways

For a gateway running directly on the Docker host, keep the default loopback port publication. Example Caddyfile:

```caddyfile
ternilo.example.com {
    reverse_proxy 127.0.0.1:4321 {
        flush_interval -1
    }
}
```

Point DNS at the gateway and provide the network access needed for certificate issuance. Open `https://ternilo.example.com` from your browser and enter the log Key. Set the public Server URL to that origin, without paths such as `/api`. Server provides HTTP internally; the gateway handles HTTPS.

For Nginx or a proxy management panel, proxy the entire root, enable WebSocket upgrades and disable streaming buffering, including `/api/v1/live` and `/api/v1/executors/connect`. See the [complete proxy example](https://github.com/mosttt/ternilo/blob/main/docs/en/deployment.md#reverse-proxy-and-tls) and trusted proxy guidance.

### Gateway in another Docker container

Join the gateway and Server to the same Docker network. The gateway container's `127.0.0.1:4321` refers to itself. For example:

```bash
docker network create ternilo-gateway
```

Create `compose.gateway.yml`:

```yaml
services:
  server:
    networks:
      default: {}
      gateway:
        aliases: [ternilo-server]
networks:
  gateway:
    external: true
    name: ternilo-gateway
```

Join the gateway container to `ternilo-gateway` too and use upstream `http://ternilo-server:4321`:

```bash
docker compose -f compose.server.yml -f compose.gateway.yml up -d --wait
```

For the included database, also pass `-f compose.server.postgres.yml` in the same command, retaining the default network for PostgreSQL. Use a unique gateway network alias for each deployment.

### Local trials and temporary SSH access

To access a remote Server temporarily, run this on your own computer:

```bash
ssh -L 4321:127.0.0.1:4321 user@server.example.com
```

Your computer's `http://localhost:4321` then reaches Server through SSH. Long-term deployments, OIDC callbacks and email links should use the real HTTPS domain rather than a temporary tunnel.

## Saved configuration and startup overrides

Web setup saves database connections, the master key and runtime settings in the volume's `config.json`. You can also stop Server, place a complete private configuration in the instance directory and restart it. Keep the master key matching the database. Bind-mounted configuration must be readable by container UID/GID `10001:10001`; use file mode `0600` and directory mode `0700`.

Database passwords already saved by web setup do not need environment variables. Automation may preset or override startup options through `.env` variables `TERNILO_DATABASE_URL`, `TERNILO_MIGRATION_DATABASE_URL` and `TERNILO_SERVER_PUBLIC_URL`. Web setup explicitly shows when the database is preset by startup options.

Precedence is CLI > environment > saved configuration > defaults. Overrides do not rewrite saved configuration or recreate the administrator. A database override changes the connection target; it does not migrate data. Apply Compose or `.env` changes with `up -d --wait`; `restart` does not reload those files.

For automated first-time administrator creation, use the separate `ternilo-server setup --non-interactive --owner-username ... --owner-email ... --owner-password ...` command or its environment variables. Daily `serve` does not accept administrator credentials or reset accounts on restart. For hidden input, use interactive `ternilo-server setup`.

## Operations

```bash
docker compose -f compose.server.yml logs -f server
docker compose -f compose.server.yml down
docker compose -f compose.server.yml up -d --wait
```

Include the same overlay files in every operation. `down` retains volumes; `down -v` deletes databases and configuration. For SQLite archives, stop Server before saving its complete volume. PostgreSQL also requires a database dump and the matching Server configuration/master key. See [backup and recovery](https://github.com/mosttt/ternilo/blob/main/docs/en/deployment.md#backups-and-recovery).
