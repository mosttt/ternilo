# Docker Compose 快速部署

[English](../en/docker-compose.md)

需要 Docker Engine 和 Compose 插件。默认直接拉取 `ghcr.io/mosttt/ternilo-server:0.2.0`，无需 Rust、Node.js、Python、源码检出或本地构建。当前镜像平台为 `linux/amd64`。

创建一个部署目录，下载已发布的 Compose 文件：

```sh
mkdir ternilo-server
cd ternilo-server
curl -fLO https://github.com/mosttt/ternilo/releases/download/v0.2.0/compose.server.yml
docker compose -f compose.server.yml pull
```

首次初始化配置、SQLite 数据库和管理员设置链接：

```sh
docker compose -f compose.server.yml run --rm server server init \
  --non-interactive --config-dir /var/lib/ternilo \
  --listen 0.0.0.0:4321 --public-url http://localhost:4321
```

保存命令输出的一次性设置链接，然后启动服务：

```sh
docker compose -f compose.server.yml up -d --wait
docker compose -f compose.server.yml ps
```

打开设置链接，创建管理员账号，再用用户名和密码登录。服务默认只映射至 `127.0.0.1:4321`。远程部署时，将初始化命令中的公开地址替换为自己的 HTTPS 域名，并配置支持 WebSocket 的反向代理。公网地址应在初始化前确定。

日常查看日志、停止和重启：

```sh
docker compose -f compose.server.yml logs -f server
docker compose -f compose.server.yml down
docker compose -f compose.server.yml up -d --wait
```

`down` 保留数据卷；不要附加 `-v`，它会删除配置、密钥和数据库。已有配置不要重复初始化。需要重建管理员访问权限时使用账号维护流程，不要删除数据卷。

Compose 中没有 `build`，默认镜像已固定版本。需要覆盖版本、摘要、端口或部署项目名时，在同目录创建 `.env`，例如：

```dotenv
TERNILO_IMAGE=ghcr.io/mosttt/ternilo-server:0.2.0
TERNILO_DEPLOY_PROJECT=ternilo-server
TERNILO_SERVER_HTTP_BIND_ADDRESS=127.0.0.1
TERNILO_SERVER_HTTP_PORT=4321
```

裸名称 `ternilo-server:0.2.0` 会指向 Docker Hub 的默认命名空间；公开发布位于 GHCR，因此使用完整地址。多个独立部署应设置不同的 `TERNILO_DEPLOY_PROJECT` 和宿主机端口，避免共用卷。更换镜像前保留配置与数据库备份，并核对版本的 schema 兼容说明。
