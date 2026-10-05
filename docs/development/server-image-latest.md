# Server 稳定版镜像别名

发行流程同时保留版本与源码 SHA 标签；稳定版公开后，单独的 `Update Server image latest` 工作流更新 `latest`。手动运行也只选择 GitHub 最新已发布稳定版，不接受任意镜像地址。它验证发行附件校验和、版本标签摘要及两种架构，再复制同一份索引，最后确认摘要保持不变。

2026-10-04：为 v0.2.3 补齐别名，[工作流 37212740379](https://github.com/mosttt/ternilo/actions/runs/37212740379) 成功。空 Docker 配置的匿名 `docker pull ghcr.io/mosttt/ternilo-server:latest` 通过，索引摘要为 `sha256:5e48c4f31f1f5628774505d8f33304a76aaa6fa6b7e8352f25a956f5c1c409ca`，与 v0.2.3 及其 linux/amd64、linux/arm64 摘要完全一致。

2026-10-05：v0.2.6 稳定版公开后，[发行流程](https://github.com/mosttt/ternilo/actions/runs/37221120329)成功更新 `latest`。匿名验证完整索引与 `:0.2.6` 及公开附件完全相同，摘要为 `sha256:64b4ad09c80adf81900d88af64a4e6ba54fe5f63bd80a00b00ad8fdf8616659a`，同时保留 linux/amd64 与 linux/arm64。匿名镜像、公开 SQLite／PostgreSQL Compose 实际浏览器和持久重启验收通过。
