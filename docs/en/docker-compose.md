# Deploy with Docker Compose

[简体中文](../zh-CN/docker-compose.md)

Install Docker Engine and the Compose plugin. Compose pulls `ghcr.io/mosttt/ternilo-server:0.1.3` directly; no Rust, Node.js, Python, source checkout or local build is required. The current image targets `linux/amd64`.

```sh
mkdir ternilo-server
cd ternilo-server
curl -fLO https://github.com/mosttt/ternilo/releases/download/v0.1.3/compose.server.yml
docker compose -f compose.server.yml pull
```

Initialize the private configuration, SQLite database and administrator setup link once:

```sh
docker compose -f compose.server.yml run --rm server server init \
  --non-interactive --config /var/lib/ternilo/server.json \
  --listen 0.0.0.0:4321 --public-url http://localhost:4321
docker compose -f compose.server.yml up -d --wait
docker compose -f compose.server.yml ps
```

Open the one-time setup link printed during initialization, create the administrator account and sign in. The default host binding is `127.0.0.1:4321`. For a remote deployment, choose your HTTPS public URL before initialization and configure a reverse proxy with WebSocket support.

```sh
docker compose -f compose.server.yml logs -f server
docker compose -f compose.server.yml down
docker compose -f compose.server.yml up -d --wait
```

`down` preserves the volume. Do not add `-v`: it deletes configuration, keys and database contents. Do not initialize existing configuration again or delete a volume to recover administrator access.

There is no Compose `build` step. To override the pinned image, digest, host port or project name, create `.env` beside the Compose file:

```dotenv
TERNILO_IMAGE=ghcr.io/mosttt/ternilo-server:0.1.3
TERNILO_DEPLOY_PROJECT=ternilo-server
TERNILO_SERVER_HTTP_BIND_ADDRESS=127.0.0.1
TERNILO_SERVER_HTTP_PORT=4321
```

The bare name `ternilo-server:0.1.3` resolves to Docker Hub's default namespace; use the full GHCR address. Separate deployments need distinct project names and host ports so their volumes remain independent. Back up configuration and database and check schema compatibility before changing versions.
