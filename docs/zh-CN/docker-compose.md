# Docker Compose 部署

[English](../en/docker-compose.md)

设置页和正常工作台首次按浏览器首选语言显示中文或英文；手动选择会保存并覆盖浏览器默认值。

需要 Docker Engine 和 Compose 插件。公开镜像 `ghcr.io/mosttt/ternilo-server:0.2.5` 支持 `linux/amd64` 和 `linux/arm64`，Docker 自动选择架构，不需要 Rust、Node.js、Python 或本地构建。

`ghcr.io/mosttt/ternilo-server:latest` 始终指向最新已发布稳定版，预发行版不会覆盖它。本文默认固定版本；要跟随稳定版，可在部署目录的 `.env` 中设置 `TERNILO_IMAGE=ghcr.io/mosttt/ternilo-server:latest`，更新时运行 `docker compose -f compose.server.yml pull`，再运行 `docker compose -f compose.server.yml up -d`。

使用默认 SQLite 或连接已有 PostgreSQL 时，可以直接启动，不必创建 `.env`。网页保存的数据库连接和对外地址会写入数据卷的 `config.json`。`.env.server.example` 展示可选的 Docker 参数、启动覆盖，以及内置 PostgreSQL 所需的数据库账号密码，按需要使用。

## SQLite：直接启动并在网页设置

```bash
mkdir ternilo-server
cd ternilo-server
curl -fLO https://github.com/mosttt/ternilo/releases/download/v0.2.5/compose.server.yml
docker compose -f compose.server.yml up -d --wait
docker compose -f compose.server.yml logs server
```

首次启动不需要先运行初始化命令，也不需要传入管理员账号或数据库参数。日志输出 `Initialization Key: ter_b_...`。打开 Server 实际访问地址，在设置页填写 Key、选择 SQLite，并创建管理员。数据库连接校验与表结构准备完成后，程序保存配置并切换为登录页。Key 必须保密，设置完成后失效。

默认只发布到**运行 Docker 的服务器**的 `127.0.0.1:4321`。如果是在自己电脑上部署，打开 `http://localhost:4321`。如果是远程服务器，先按下文配置 HTTPS 网关，再从自己电脑或手机打开域名；自己的 `localhost` 不会访问远程服务器。

默认 SQLite 保存为数据卷内 `/var/lib/ternilo/data/db/server.sqlite3`，总配置为 `/var/lib/ternilo/config.json`。管理员账号及密码哈希保存在数据库中，配置文件不保存管理员明文密码。之后重启直接使用保存的配置和账号。

## 服务器上的外部 PostgreSQL

PostgreSQL 可以在 Docker 宿主机上，也可以在另一台数据库服务器。先按[数据库账号准备](https://github.com/mosttt/ternilo/blob/main/docs/zh-CN/deployment.md#选择-postgresql)建立空数据库、建表账号与受限运行账号，再使用上面的 Compose 启动 Server，在网页选择 PostgreSQL。

例如数据库服务器私网地址为 `10.0.0.20`，端口 `5432`，数据库为 `ternilo`，网页填写：

```text
PostgreSQL 连接地址：
postgresql://ternilo_app:APP_PASSWORD@10.0.0.20:5432/ternilo

建表账号连接地址：
postgresql://ternilo_owner:OWNER_PASSWORD@10.0.0.20:5432/ternilo
```

两个连接必须指向同一数据库。将示例密码替换为实际密码，并对密码中的 `@`、`:`、`/` 等字符做 URL 编码。需要 TLS 时使用数据库提供的参数，例如 `?sslmode=require`；证书校验按数据库服务要求配置。

如果 PostgreSQL 安装在 Docker 宿主机上，连接地址也必须能从 Server 容器访问。Linux 可新建 `compose.host.yml`：

```yaml
services:
  server:
    extra_hosts:
      - "host.docker.internal:host-gateway"
```

然后启动：

```bash
docker compose -f compose.server.yml -f compose.host.yml up -d --wait
```

网页连接地址的主机改成 `host.docker.internal`。数据库须监听容器可达的宿主机地址，并在 `pg_hba.conf` 和防火墙中允许实际 Docker 网络来源；只监听宿主机 `127.0.0.1` 不够。`host-gateway` 只解决地址解析，不会改变数据库监听和访问权限。不要为此把数据库无条件暴露到公网。

## Compose 自带 PostgreSQL

在同一个部署目录下载数据库叠加配置和初始化 SQL：

```bash
curl -fLO https://github.com/mosttt/ternilo/releases/download/v0.2.5/compose.server.postgres.yml
curl -fLO https://github.com/mosttt/ternilo/releases/download/v0.2.5/postgres-init.sql
```

在该目录创建 `.env`，设置**数据库账号密码**，这与 Ternilo 管理员密码不同：

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

在网页选择 PostgreSQL，填写：

```text
PostgreSQL 连接地址：
postgresql://ternilo_app:APP_PASSWORD@postgres:5432/ternilo

建表账号连接地址：
postgresql://ternilo_owner:OWNER_PASSWORD@postgres:5432/ternilo
```

连接密码分别对应 `.env` 的 owner 和 app 两项；admin 密码仅用于 PostgreSQL 的数据库管理员，特殊字符仍需 URL 编码。`postgres` 是 Compose 数据库服务名，使用容器端口 `5432`；数据库没有发布宿主机端口。初始化 SQL 创建 `ternilo_runtime` 权限组和受限账号，Server 使用建表账号准备完整 schema，再使用运行账号处理请求。

PostgreSQL 将数据保存在独立的 `postgres-data` 卷；Server 的 `server-data` 卷仍需保留配置与主密钥。PostgreSQL 初始化脚本只对空数据库卷执行，修改 `.env` 不会更改已创建的数据库密码。

## 远程服务器与 HTTPS 网关

网关直接运行在 Server 宿主机时，Compose 保持默认回环端口发布。例如 Caddy：

```caddyfile
ternilo.example.com {
    reverse_proxy 127.0.0.1:4321 {
        flush_interval -1
    }
}
```

将域名解析到网关，准备证书签发所需的网络入口，然后从自己的浏览器打开 `https://ternilo.example.com`，输入日志 Key。设置页的 Server 对外地址填写这个根地址，不包含 `/api` 等路径。Server 内部仍然提供 HTTP；HTTPS 由网关处理。

使用 Nginx 或反代面板时，同样代理整个根路径，开启 WebSocket 并关闭流式响应缓冲，覆盖 `/api/v1/live` 和 `/api/v1/executors/connect`。完整 Nginx 示例与可信代理说明见[Server 部署与运维](https://github.com/mosttt/ternilo/blob/main/docs/zh-CN/deployment.md#反向代理与-tls)。

### 网关也在 Docker 容器中

网关容器与 Server 必须加入同一 Docker 网络。不能把上游填成网关自己的 `127.0.0.1:4321`。例如先创建网关共享网络：

```bash
docker network create ternilo-gateway
```

新建 `compose.gateway.yml`：

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

将网关容器也接入 `ternilo-gateway`，上游填写 `http://ternilo-server:4321`。启动时带上叠加配置：

```bash
docker compose -f compose.server.yml -f compose.gateway.yml up -d --wait
```

使用自带 PostgreSQL 时，在同一命令中同时加入 `-f compose.server.postgres.yml`，保留默认网络上的数据库连接。每个部署使用独立且不会冲突的网关网络别名。

### 本机体验与临时 SSH 访问

临时访问远程服务器可在自己的电脑建立 SSH 转发：

```bash
ssh -L 4321:127.0.0.1:4321 user@server.example.com
```

此时自己电脑的 `http://localhost:4321` 才会经 SSH 到达远程 Server。长期部署和 OIDC、邮件链接使用真实 HTTPS 域名；它们不能依赖临时 SSH 通道。

## 保存配置与启动覆盖

网页设置将数据库连接、主密钥和运行设置写入数据卷的 `config.json`。也可以停止服务后，将已准备好的完整私有配置放入同一实例目录，再启动；必须保留数据库与配置配套的主密钥。使用 bind mount 时，文件须能被容器运行账号 UID/GID `10001:10001` 读取，配置权限应为 `0600`，目录为 `0700`。

不需要把网页已保存的数据库密码再写进环境变量。自动化可以通过 `.env` 的 `TERNILO_DATABASE_URL`、`TERNILO_MIGRATION_DATABASE_URL`、`TERNILO_SERVER_PUBLIC_URL` 预设／覆盖对应启动选项；数据库已预设时，网页会明确显示使用启动配置。

优先级为命令行参数 > 环境变量 > 配置文件 > 默认值。已有实例的启动覆盖不回写配置，也不会重建管理员。数据库覆盖只切换连接目标，不会迁移数据。修改 Compose 或 `.env` 后执行 `up -d --wait`；单纯 `restart` 不会读取这些文件的修改。

自动化首次创建管理员使用单独的 `ternilo-server setup --non-interactive --owner-username ... --owner-email ... --owner-password ...`，也可用其对应环境变量；这是一次性设置命令。日常 `serve` 不接受管理员账号参数，不会因容器重启创建或重置账号。需要隐藏输入时使用交互式 `ternilo-server setup`。

## 日常运行

```bash
docker compose -f compose.server.yml logs -f server
docker compose -f compose.server.yml down
docker compose -f compose.server.yml up -d --wait
```

采用叠加配置时，每次操作都带上相同的 `-f` 文件。`down` 保留数据卷，`down -v` 会删除数据库与配置。备份 SQLite 时先正常停止服务再保存完整卷；PostgreSQL 还需数据库转储，并同时保留 Server 配置和主密钥。详见[备份与恢复](https://github.com/mosttt/ternilo/blob/main/docs/zh-CN/deployment.md#备份恢复升级与轮换)。
