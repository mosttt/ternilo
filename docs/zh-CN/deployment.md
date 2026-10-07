# Server 部署与运维

服务器只运行 `ternilo-server`，提供账号登录、电脑连接和共享工作台。默认使用 SQLite；也可使用 PostgreSQL。单用户／多用户由所有者在网页“平台管理 → 实例设置”切换，沿用同一数据库和原资源，不需要换程序或重建管理员。

电脑另运行 `ternilo serve`，保留原来的文件、模型、插件和会话。Server 与 Ternilo 可以在同一台机器，也可以分别部署；电脑连接步骤见[远程访问](remote-access.md)。OIDC 和托管 Worker 都是可选能力。需要提供托管执行时，按[Worker 部署](worker.md)开启功能并接入独立 Worker；Worker 不连接数据库，不接收实例 master key 或模型密钥。

本文的启动步骤针对单个 Server。需要多个实例时，按[Server 集群](server-cluster.md)配置共享 PostgreSQL、主密钥及各自的 `cluster_url`，使用已实现的 Node 跨实例转发和实时补读；这不包含跨主机存储接管或透明故障转移。

可以选择[直接运行二进制](#非容器部署)或[Docker 部署](#docker网页设置与启动)。两种方式运行同一个 `ternilo-server`，都需要选择数据库，并为远程浏览器提供 HTTPS 入口；只有 Docker 方式需要 Docker／Compose。

## 通用前提

- 从 [GitHub Release](https://github.com/mosttt/ternilo/releases) 下载对应平台二进制，或直接使用公开镜像 `ghcr.io/mosttt/ternilo-server:0.2.8`。Docker 部署不需要本地构建。
- Docker 方式需要 Docker Engine 和 Compose 插件；使用 `ternilo-deploy` 辅助工具时另需 Python 3.9 以上。发行包中的 `deploy/docker/` 可复制到服务器独立使用，不依赖源码目录。
- 远程访问使用域名和外部 HTTPS 反向代理。默认只把容器端口映射到服务器 `127.0.0.1:4321`；电脑的本地 `3210` 端口无需对外开放。
- Server 数据卷包含私有配置、加密密钥及 SQLite 数据。PostgreSQL 数据库另行持久化，但仍需同时保存 Server 配置。

## 非容器部署

从 Release 下载 Server 归档，将可执行程序安装到 `PATH`。以下命令统一使用 `ternilo-server`，Windows 使用同一程序名。运行现成二进制只需程序及其配置；Linux／macOS 示例：

```bash
ternilo-server serve \
  --config-dir "$HOME/.local/share/ternilo-server" \
  --listen 127.0.0.1:4321
```

先按[反向代理与 TLS](#反向代理与-tls)配置网关，上游为 `http://127.0.0.1:4321`。Server 首次启动直接输出初始化 Key，从自己的电脑或手机打开实际访问的域名，输入 Key，选择 SQLite 或 PostgreSQL，填写对外地址并创建管理员。服务器不需要桌面或浏览器，也不需要先执行设置命令。初始化、登录和工作台按浏览器首选中文／英文显示，手动选择优先并保存到当前浏览器。

Windows PowerShell 使用相同选项：

```powershell
ternilo-server serve --config-dir "$env:LOCALAPPDATA\ternilo-server" --listen 127.0.0.1:4321
```

`--public-url https://ternilo.example.com` 可预设网页的对外地址，但不是打开网页的必要参数。它用于登录回调和邮件链接，不负责 HTTPS 或反向代理。也可在首次网页设置及之后的实例设置中保存它。

默认 SQLite 位于实例目录的 `data/db/server.sqlite3`。PostgreSQL 先按[准备步骤](#选择-postgresql)建立账号与数据库，再在网页填写连接地址。原生程序填写宿主机可达的地址；Docker 填容器可达的地址，见 [Compose 数据库示例](docker-compose.md)。

不适合使用网页时，运行 `ternilo-server setup --config-dir <实例目录>` 进入终端向导，选择数据库并隐藏输入连接串和管理员密码。自动化使用 `setup --non-interactive --owner-username ... --owner-email ... --owner-password ...` 或对应环境变量；这只用于首次设置。已有配置时 `setup` 拒绝覆盖，日常 `serve` 不接受管理员账号参数，容器重启不会重置账号。`serve` 的 CLI > 环境变量 > 配置 > 默认值；启动覆盖不回写配置，也不搬移数据库数据。

`--config-dir` 指整个实例目录，固定读取其中的 `config.json`，环境变量为 `TERNILO_SERVER_CONFIG_DIR`。默认目录为 `$XDG_DATA_HOME/ternilo-server`，未设置 XDG 时为 `$HOME/.local/share/ternilo-server`。无子命令也会启动服务。

后续 `serve --config-dir ./server` 读取保存的数据库连接，不必重复指定。`--database-url`／`TERNILO_DATABASE_URL` 可在本次启动覆盖连接，`--migration-database-url`／`TERNILO_MIGRATION_DATABASE_URL` 指定建表连接；它们不会改写配置或迁移原数据库。选择另一实例目录也不会改动原实例数据。

实例所有者可以在“平台管理 → 实例设置”配置 OIDC、Turnstile 和邮件服务，见[登录与人机验证](server-authentication.md)。

长期运行交由 systemd 或你的服务管理器，使用独立的服务账号和相同配置路径。服务监听及公开 URL 分别由 `--listen`／`TERNILO_SERVER_LISTEN`、`--public-url`／`TERNILO_SERVER_PUBLIC_URL` 设置。可选 OIDC 和托管执行配置保留独立参数；更完整的软件分层见[架构说明](architecture.md)。

原生程序的完整备份同样先停止服务，再保存私有配置和实际数据库。SQLite 若通过 `--database-url` 放在配置目录外，也必须备份那个位置；PostgreSQL 则另做转储。SQLite 也可单独使用[在线数据库快照](#在线保存-sqlite-数据库快照)。恢复到新目录后，确认配置中的数据库连接指向恢复后的数据库；不要混用另一份快照的 WAL／SHM 文件。主密钥轮换使用 `ternilo-server admin rotate-secret-master-key --help` 所列参数，完成后把新密钥保存到同一配置。不要混用另一个实例的配置或密钥。

## Docker：网页设置与启动

[Docker Compose 部署](docker-compose.md)提供三种完整示例：默认 SQLite、服务器上的外部 PostgreSQL，以及 Compose 自带 PostgreSQL。默认流程只需要 `docker compose up -d --wait`，查看日志 Key，再通过真实访问地址完成网页设置；不需要单独运行初始化命令。

数据卷的 `/var/lib/ternilo/config.json` 保存数据库连接与主密钥，管理员账号及密码哈希在数据库中。后续启动读取保存的配置。首次初始化完成以数据库的实例所有者记录为准；已有配置但管理员创建中断时，仍可通过受 Key 保护的管理员设置页完成。初始化 Key 不写入明文配置，只保存摘要，完成后不能再次创建管理员。

发行包还提供 `ternilo-deploy` 运维辅助工具，需要 Python 3.9 以上。它的 `init` 动作用于准备独立部署目录、Compose 项目和持久配置，内部调用 Server 的 `setup`，然后通过 `up` 启动；这是可选的自动化途径。日常 `status`、`logs`、`backup`、`restore` 等操作在该工具生成的部署目录执行。

### 选择 PostgreSQL

当前发行的 Control 基础 schema 为 14，认证、模型限流等组件另有匹配的独立 schema。SQLite 与 PostgreSQL 都只初始化匹配版本的最终 schema，不自动迁移历史结构；版本不匹配会明确拒绝启动，不会删除或重建已有数据。请先保留完整备份，在独立新数据库验收当前开发版；不要通过删除日常数据库来绕过版本检查。同版本备份恢复与跨 schema 数据迁移是不同操作，后者目前没有自动流程。

使用已有 PostgreSQL，先建立空数据库和可连接账号。初始化会实际校验连接和 schema，失败不会发布一份可用配置。建议分别使用负责建表的 owner 账号和日常运行的受限账号；新安装时，由数据库管理员在 `psql` 中执行以下示例，并通过 `\password` 隐藏设置密码：

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

已有相应角色时无需重复创建。`ternilo_runtime` 是程序初始化 schema 时授予表和函数访问权的固定组角色，必须先存在；实际登录账号的名称可以自定，但要继承该角色。日常运行账号不应拥有数据库、表或 schema，也不应具有 superuser／`BYPASSRLS` 权限。`ternilo_owner` 拥有数据库并负责初始化，`ternilo_app` 仅用于正常运行。

网页选择 PostgreSQL 后分别填写运行账号连接和建表账号连接，例如：

```text
postgresql://ternilo_app:APP_PASSWORD@10.0.0.20:5432/ternilo
postgresql://ternilo_owner:OWNER_PASSWORD@10.0.0.20:5432/ternilo
```

两个地址必须连接同一数据库，密码中的特殊字符需 URL 编码。数据库连接与主密钥保存到 `config.json`，不必重复设置环境变量。外部数据库、Docker 宿主机数据库与 Compose 服务名的具体选择见[数据库部署示例](docker-compose.md)。

### 反向代理与 TLS

网页或可选 `--public-url` 填浏览器最终访问的根地址，例如 `https://ternilo.example.com`；它用于登录回调和邮件链接。Server 本身提供 HTTP，HTTPS 和证书由网关处理。域名的 A／AAAA 记录应指向网关，网关的 HTTPS 入口应能从用户设备访问。

| 网关位置 | 网关填写的 HTTP 上游 | Server 监听／端口发布 |
|---|---|---|
| 直接运行在 Server 宿主机 | `http://127.0.0.1:4321` | 二进制监听 `127.0.0.1:4321`；Docker 保持默认的宿主机回环端口映射 |
| 与 Server 在同一 Docker 网络的容器 | `http://server:4321`，或为 Server 配置的网络别名 | 使用容器端口 `4321`；网关容器的 `127.0.0.1` 指它自身 |
| 另一台机器上的网关 | `http://Server私网IP:4321` | 二进制监听该私网地址；Docker 的 `TERNILO_SERVER_HTTP_BIND_ADDRESS` 设为宿主机私网 IP，并限制上游只允许网关访问 |

Docker 网关与 Server 需要加入同一网络，具体连接步骤见[容器中的网关](docker-compose.md#网关也在-docker-容器中)。不能直接在另一个容器里填 `127.0.0.1:4321`。使用反代面板时，域名填 `ternilo.example.com`，上游按表填写，开启 HTTPS 和 WebSocket，关闭流式响应缓冲。

Caddy 直接安装在 Server 宿主机时，Caddyfile 可以写为：

```caddyfile
ternilo.example.com {
    reverse_proxy 127.0.0.1:4321 {
        flush_interval -1
    }
}
```

Caddy 根据域名管理 HTTPS，前提是 DNS 和证书签发所需的网络入口可用；WebSocket 自动代理。见 [Caddy 反代文档](https://caddyserver.com/docs/caddyfile/directives/reverse_proxy)。

Nginx 的以下 `map` 和 `server` 块放在 `http` 配置上下文中，证书路径替换为网关实际证书：

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

整个根路径都应经过反代，包括 `/api/v1/live` 和 `/api/v1/executors/connect`；不只代理首页。Nginx 必须显式转发升级头，见[官方 WebSocket 文档](https://nginx.org/en/docs/http/websocket.html)。需要准确的客户端 IP 时，按[可信代理设置](settings.md)配置网关的实际连接 IP；转发头本身不建立信任。

`localhost`／`127.0.0.1` 始终指运行浏览器或进程的当前机器。从自己的电脑打开 `http://localhost:4321` 不会访问远程服务器；只有建立[SSH 本地端口转发](docker-compose.md#本机体验与临时-ssh-访问)后才能通过它访问服务器。长期远程访问使用公开 HTTPS 域名，Node 使用同一入口派生的 WSS 地址。

## 日常运行与诊断

```bash
./ternilo-deploy status
./ternilo-deploy logs
./ternilo-deploy down
./ternilo-deploy up
```

`down` 正常停止并保留数据卷。不要加 `docker compose down -v`，那会删除数据库与配置。`up` 等待容器健康检查通过；`status` 检查 Server 就绪状态。网页能打开后，还应在“我的机器”确认电脑在线，实际发送一个任务并检查停止和重连。

`check --offline` 只检查部署文件，不表示数据库已连通。普通 `check` 额外检查 Compose、Docker 和持久配置存在；运行时数据库是否就绪由 `/readyz` 与容器健康检查反映。`/livez` 用于进程存活，`/metrics` 提供服务器连接与持久队列统计。

### 初始化与诊断参数

显式 CLI 优先于同名环境变量。参数不按单用户／多用户重复配置：

| CLI | 环境变量 | 用途 |
|---|---|---|
| `--component` | `TERNILO_DEPLOY_COMPONENT` | init 默认 server，也可选择 worker；其他操作读取已保存的部署类型 |
| `--server-url` | `TERNILO_WORKER_SERVER_URL` | Worker 接入和执行维护使用的 Server 地址 |
| `--managed-execution-enabled`／`--no-managed-execution-enabled` | — | init／configure 时开启或关闭 Server 托管执行 |
| `--directory` | `TERNILO_DEPLOY_DIR` | 部署目录，默认当前目录 |
| `--image` | `TERNILO_IMAGE` | 明确版本或 digest，不接受 latest |
| `--public-url` | `TERNILO_SERVER_PUBLIC_URL` | 初始化／恢复的浏览器公开地址 |
| `--project-name` | `TERNILO_DEPLOY_PROJECT` | Compose 项目名，初始化默认生成独立名称 |
| `--database-url` | `TERNILO_DATABASE_URL` | 初始化或 PostgreSQL 恢复的数据库连接 |
| `--migration-database-url` | `TERNILO_MIGRATION_DATABASE_URL` | 可选 PostgreSQL schema-owner 连接 |
| `--docker` | `TERNILO_DOCKER` | Docker 命令路径 |
| `--offline`／`--no-offline` | `TERNILO_DEPLOY_OFFLINE` | 仅影响 check 是否访问 Docker |
| `--output-dir` | `TERNILO_DEPLOY_BACKUP_DIR` | 备份输出目录 |
| `--archive` | `TERNILO_DEPLOY_BACKUP_ARCHIVE` | 待恢复的备份文件 |
| `--database-dump` | `TERNILO_DEPLOY_DATABASE_DUMP` | 停机时生成的 PostgreSQL dump |
| `--pg-restore` | `TERNILO_PG_RESTORE` | 校验 PostgreSQL 转储的客户端程序，默认 `pg_restore` |
| `--include-image`／`--no-include-image` | `TERNILO_DEPLOY_BACKUP_IMAGE` | 备份是否包含镜像，默认 false |
| `--next-key` | `TERNILO_NEXT_SECRET_MASTER_KEY` | 轮换的可选新主密钥，优先隐藏输入环境变量 |

`.env` 还可设置 `TERNILO_SERVER_HTTP_BIND_ADDRESS` 和 `TERNILO_SERVER_HTTP_PORT`。修改端口、镜像后重新 `up`；公开 URL 同时保存在 Server 配置中，变更域名时也要更新配置的 `public_url`。

## 备份、恢复、升级与轮换

### 一致性备份

当前 Server 备份采用停机快照。先正常停止；不要在 SQLite 写入期间只复制主数据库文件。工具检查实际命名数据卷是否仍被其他运行容器使用，不只检查当前项目；自定义 bind mount 需要另行安排运维。所有备份包含私有配置和密钥，输出文件权限为 0600。

```bash
./ternilo-deploy down
./ternilo-deploy backup --output-dir "$HOME/ternilo-backups"
./ternilo-deploy up
```

产物是 `ternilo-server-时间.tar.gz` 及 `.sha256`。它包含整个 Server 数据卷、部署及维护工具、镜像引用和准确的镜像 ID。加 `--include-image` 可一并保存 Docker 镜像，便于离线恢复。Worker 在自己的部署目录使用同一个 `backup` 命令保存完整私有卷。

PostgreSQL 需要在 Server 停止期间先用匹配版本的 `pg_dump` 生成 custom-format 转储，再通过 `--database-dump /path/to/database.dump` 收录。备份与校验还需要能读取该转储版本的 `pg_restore`；这些程序通常来自同一套 PostgreSQL 客户端工具。连接参数使用受保护的 PostgreSQL service／passfile 或环境配置，不在日志打印密码。例如已配置 `PGSERVICE=ternilo-backup`：

```bash
./ternilo-deploy down
umask 077
pg_dump --format=custom --file=database.dump
./ternilo-deploy backup --output-dir "$HOME/ternilo-backups" --database-dump database.dump
./ternilo-deploy up
```

该命令只备份 Server。电脑上的文件与本地会话需按[本地数据说明](getting-started.md)另行备份。托管 Worker 要先暂停领取并等待活动任务归零，再停机保存每台 Worker 的完整数据卷；逐主机步骤见[Worker 备份与恢复](worker.md#备份与恢复)。

### 检查备份

复制备份时同时保留同名 `.sha256`，使用统一工具检查 Server 或 Worker 归档：

```bash
./ternilo-deploy verify --archive /private/backups/ternilo-server-时间.tar.gz
```

校验检查归档完整性、必要配置、数据卷结构及所附镜像内容；PostgreSQL 还用 `pg_restore --list` 检查转储是否可读。默认只读取本地文件，不连接数据库、不启动服务，也不创建或修改数据卷。校验通过说明备份可读，仍需通过新环境恢复和实际任务确认应用可用。

### 在线保存 SQLite 数据库快照

只需要一份在线一致的 SQLite 数据库副本时，可使用：

```bash
ternilo-server admin backup-sqlite --config-dir /path/to \
  --output /private/backups/server-snapshot.sqlite3
```

Server 可以继续运行。命令保存已提交的数据，目标文件权限为 0600，拒绝覆盖已有文件；未提交的写入不会混入快照。它只生成数据库副本，不包含 Server 配置、主密钥或 Worker 文件，不能替代上面的多卷一致备份。复制并保存匹配的私有配置，才能恢复加密凭据。

### 在新环境恢复

复制备份和同名 `.sha256`，准备备份记录中的相同镜像。工具按准确镜像 ID 检查本机内容；相同版本标签若已经指向其他镜像，不能冒充原镜像。恢复要求空目录，并创建新的数据卷；不会覆盖正在运行的服务或已有目录：

```bash
./ternilo-deploy restore \
  --archive "$HOME/ternilo-backups/ternilo-server-时间.tar.gz" \
  --directory "$HOME/ternilo-server-restored"
cd "$HOME/ternilo-server-restored"
./ternilo-deploy check
./ternilo-deploy up
./ternilo-deploy status
```

恢复不会自动启动服务。若原服务仍占用 4321，先在新目录 `.env` 修改端口，或停止原服务。更换公开地址时给 restore 增加 `--public-url`，它会同步恢复后的 Server 配置。

恢复后的 `.env` 固定到备份记录的镜像 ID，避免标签后来变化影响重启。若提供 `--image`，该名称也必须指向同一镜像内容。升级到新镜像在完成原版本恢复验证后单独进行。

PostgreSQL 恢复先从备份提取 `database.dump`，用 `pg_restore --exit-on-error --dbname=目标数据库 database.dump` 写入一个新建的空库；再给上述 restore 命令提供新库的 `TERNILO_DATABASE_URL`。工具不会对 PostgreSQL 自动执行覆盖恢复，也不会默认连回原库；原主密钥随 Server 配置恢复，用于读取还原后的机密。需要受限 runtime 时同时设置目标库的 migration URL，并按原角色策略恢复权限。

备份含 `image.tar` 时，可提取后显式执行 `docker image load --input image.tar`。恢复后确认管理员身份、机器记录、共享权限、历史及真实任务；不要只看首页是否返回 200。

在线 SQLite 快照可能保留尚未到期的旧网关租约。恢复时先停止原实例及使用同一电脑身份的旧连接；新实例可能需要等待旧租约到期，再加上电脑的重连间隔才显示在线。当前网关租约为 60 秒、电脑重连退避最多 30 秒，这不是固定恢复时限。等待期间明确的电脑离线响应不表示历史丢失，也不能记为恢复后的任务成功。

### 升级与回滚

先备份，再把 `.env` 中的 `TERNILO_IMAGE` 改为已经取得的明确新版本，执行 `./ternilo-deploy up`。遇到需要回退的情况，使用备份在新卷恢复原版本与匹配数据；不能假设旧程序能直接读取任意更新后的数据库。

当前执行器协议为 `48`，包含账号输入授权证明、设备 Provider 用量事件、有界历史分页、队列编辑版本条件和归档恢复操作。Server、连接电脑上的 Ternilo 与独立 Worker 使用同一发行版本。执行器协议与自动化 JSON-RPC v1、ACP v1 分别管理。

### 轮换 secret master key

主密钥轮换调用现有原子重加密接口，保持资源和账号不变。先备份并停止 Server，再执行：

```bash
./ternilo-deploy down
./ternilo-deploy rotate-key
./ternilo-deploy up
```

工具先在同一私有卷保存并同步落盘 `config.next.json`，再调用 `ternilo-server admin rotate-secret-master-key`；成功后原子替换并同步当前配置。默认生成随机密钥，也可提供 `TERNILO_NEXT_SECRET_MASTER_KEY`。如果轮换中断，`up` 和普通备份会拒绝继续；保持停止并保留新旧配置，检查数据库实际状态后恢复，不要直接删除下一把密钥。旧备份必须继续与其原密钥一起保存。

### 轮换数据库密码

使用分开的 runtime 和 schema-owner 数据库账号时，可以由工具同时轮换密码和私有连接配置。下面的命令隐藏输入新密码；工具先暂停托管执行、等待活动任务结束，再停止 Server，完成后重新启动并恢复执行：

```bash
read -rsp 'New runtime database password: ' TERNILO_RUNTIME_DB_PASSWORD_NEXT; echo
read -rsp 'New schema-owner database password: ' TERNILO_MIGRATOR_DB_PASSWORD_NEXT; echo
export TERNILO_RUNTIME_DB_PASSWORD_NEXT TERNILO_MIGRATOR_DB_PASSWORD_NEXT
./cloud-rotate-credentials.sh --server-url https://agent.example.com database-passwords
unset TERNILO_RUNTIME_DB_PASSWORD_NEXT TERNILO_MIGRATOR_DB_PASSWORD_NEXT
```

运行前先保存匹配的备份。维护接口需要平台管理身份；交互运行可隐藏输入管理员密码，自动化可提供 `TERNILO_SERVER_ACCESS_TOKEN`。数据库账号必须有修改相应角色密码的权限。失败时 Server 保持停止，保留 `config.json` 和待确认的 `config.next.json`，先确认数据库事务结果，再处理配置；不要直接删除候选文件继续启动。

### 更新与轮换模型 Key

日常更新平台上游 Key，从“平台管理 → 模型服务 → 上游接入”编辑；自己的模型从“用户设置 → 模型”编辑。Worker 不保存上游 Key，这两类更新都不要求给 Worker 分发密钥文件。

需要验证失败后自动恢复旧 Key 时，使用部署目录自带的轮换工具。`--provider-id` 填平台上游接入的标识；`--` 后的命令必须实际通过这个 Provider 调用模型，退出码为零才表示验证成功：

```bash
read -rsp 'New model API key: ' TERNILO_MODEL_API_KEY_NEXT; echo
export TERNILO_MODEL_API_KEY_NEXT
./cloud-rotate-credentials.sh --server-url https://agent.example.com \
  --provider-id my-provider model-key -- /absolute/path/to/check-model
unset TERNILO_MODEL_API_KEY_NEXT
```

工具暂存加密的旧 Key，启用新 Key 后运行验证命令。验证期间，该 Provider 的新请求已经使用候选 Key；验证失败，包括命令不存在或不可执行，会恢复旧 Key，成功则提交。轮换复用一次管理员登录，操作结束后注销临时会话。上例的 `check-model` 是你为该模型准备的实际调用脚本，不是 Ternilo 内置命令。初始化和备份恢复目录均携带轮换脚本及依赖，不需要旧 Key 明文文件。

进程被中断时，先查询待确认操作，再根据结果明确提交或回滚：

```bash
./cloud-rotate-credentials.sh --provider-id my-provider model-key-status
./cloud-rotate-credentials.sh --provider-id my-provider \
  --rotation-id 查询返回的操作ID model-key-rollback
```

确认新 Key 已验证成功时，将最后的 `model-key-rollback` 换为 `model-key-commit`。平台 Key 的暂存旧版本也包含在数据库备份和主密钥轮换中。电脑连接凭据从“我的机器”撤销并重新登记；原生账号可退出当前登录，OIDC 按身份提供方策略管理。

### 可复现 release gate

先验证相关 Rust 与前端测试，再对实际发行物验证初始化、登录、电脑连接、共享、审批、实时重连、停机备份、新卷恢复及主密钥轮换。仓库已有完整门禁脚本，入口与覆盖范围见[发布说明](release-packaging.md#完整发行门禁)。脚本存在或脚本单元测试通过，不能替代当前发行物的实际验收。

Local／Server 组件包的[安装恢复验证](release-packaging.md#客户端与-server-安装恢复验证)可以独立运行，覆盖两种 SQLite 恢复及受限 PostgreSQL runtime 的配置／转储匹配恢复。检查包括凭据、模型与设备限制、正常／归档历史、账本及恢复后真实任务。该入口不覆盖各系统安装器、Docker 完整发行门禁、Worker 活动任务恢复或跨版本迁移；不能直接替换旧 schema 日常实例。
