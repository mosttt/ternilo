# 配置与数据目录

每个实例使用独立目录，根目录的 `config.json` 是启动配置入口。目录按用途分类：`config/` 保存可编辑配置，`secrets/` 保存凭据与授权，`data/` 保存不可丢失的数据，`cache/` 保存可重建索引，`runtime/` 保存服务发现、锁及执行收尾状态。只创建当前程序需要的目录。

## Ternilo 本机与 Node

默认目录为 `$XDG_DATA_HOME/ternilo`，未设置时为 `~/.local/share/ternilo`；通过 `--data-dir` 选择另一实例。桌面、本机网页和 Node 共用同一个本机应用目录。

```text
ternilo/
├── config.json
├── config/
│   ├── providers.json
│   └── agent-presets.json
├── secrets/
│   ├── credentials.json
│   ├── credential-records.json
│   ├── model-connections.json
│   └── node-authorizations.json
├── data/
│   ├── state.json
│   ├── sessions/
│   ├── attachments/objects/
│   ├── extensions/
│   └── db/
│       ├── agent-team.sqlite3
│       ├── inbox.sqlite3
│       ├── preferences.sqlite3
│       └── node-transport.sqlite3
├── cache/
│   ├── session-search.sqlite3
│   └── session-projections.sqlite3
└── runtime/
    ├── service.json
    ├── service.log
    ├── writer.lock
    ├── service-start.lock
    └── execution-resources.json
```

文件按实际使用生成。SQLite 的 `-wal`、`-shm` 与主库一起保存在所属目录。`data/sessions/` 中的 JSONL 是会话事件原本，搜索与投影缓存可重建；队列、运行作者、附件和传输回执不能当作缓存删除。

首次 `ternilo serve` 保存有效启动参数到私有 `config.json`。之后按“命令行 → 环境变量 → 配置文件 → 内置默认值”读取。监听、Profile、执行上限和网页开关的覆盖仅影响本次启动；Server 连接地址、系统分配的电脑 ID、凭据及明文联调开关在连接验证成功后保存，支持恢复接入后的普通重启。未登记时电脑 ID 为空。保存的 Profile 相对路径从实例目录解析；命令行 Profile 路径从启动目录解析。RPC、ACP 与一次性 `run` 保留各自的显式启动参数。

总配置可能包含 Node 连接凭据，属于私密文件。POSIX 新目录使用 `0700`，总配置及凭据文件使用 `0600`；Windows 使用当前用户目录的访问权限。浏览器主题等界面偏好仍由浏览器保存，模型、预设和会话等共享状态由执行端保存。

`runtime/` 不是可以随时清空的临时目录：执行收尾记录用于追踪仍持有资源的任务，服务发现文件用于认证本机服务。正常运行时不要删除它。跨进程工作目录协调使用系统用户状态目录中的 `ternilo/execution-locks/`，独立于实例目录，使多个端口或数据目录仍能协调同一个工作文件夹。

## Ternilo Server

默认实例目录为 `$XDG_DATA_HOME/ternilo-server/`，或 `~/.local/share/ternilo-server/`；`--config-dir` 指定整个目录，程序固定读取其中的 `config.json`。环境变量为 `TERNILO_SERVER_CONFIG_DIR`，命令行优先。Docker 使用 `--config-dir /var/lib/ternilo`。

```text
ternilo-server/
├── config.json
├── config/
│   └── worker-policy.json
└── data/
    ├── db/server.sqlite3
    └── workspaces/
```

网页选择 SQLite 或终端设置未指定数据库时，SQLite 位于实例下的 `data/db/server.sqlite3`；显式 `database_url` 可使用其他 SQLite 路径或 PostgreSQL。账号、模型设置、加密凭据、资源权限、队列、审计和 Gateway 数据仍由同一事务数据库管理，不拆成几份缺少事务一致性的配置文件。部署工具写入的 WorkerPolicy 位于 `config/worker-policy.json`。

Server 的数据库地址、主密钥及身份设置由总配置管理。平台模型 Key 加密保存在数据库中。备份须同时保存总配置、真实数据库及外部持久存储；仅复制 `config.json` 不包含数据库或连接电脑上的文件。

## Worker 与 Work 预留

现有可选 `ternilo-worker` 使用 `--config-dir` 或 `TERNILO_WORKER_CONFIG_DIR` 选择实例目录，固定读取其中的 `config.json`；初始化保存专属 Token、Server 地址、容量和执行策略，工作卷默认为同一实例下的 `data/workspaces/`。Docker 对应 `/var/lib/ternilo/config.json` 与 `/var/lib/ternilo/data/workspaces/`。存储标识及占用／恢复元信息随工作卷保存，保留其文件系统身份约束。

`ternilo-work` 尚未实现独立程序和 Docker 生命周期。其设计采用相同分类：根目录 `config.json`，配置在 `config/`，接入凭据在 `secrets/`，持久工作卷在 `data/`，可重建缓存在 `cache/`，宿主管理锁与运行状态在 `runtime/`。Work 状态目录与用户工作卷分开管理；容器工作目录不作为读取宿主管理凭据的入口。具体接入边界见[Work 设计](../development/execution-coordination.md)。

备份操作见[本机数据与备份](getting-started.md#5-数据与备份)、[Server 备份](server-reference.md#原生-server-备份与恢复)及[Worker 指南](worker.md)。

```bash
ternilo-server serve --config-dir ./server
ternilo-worker init --config-dir ./worker --server-url https://ternilo.example.com
ternilo-worker serve --config-dir ./worker
```

Worker 接入 Token 在初始化时按部署要求提供。本机 Ternilo 的 `--data-dir` 同样指向整个实例目录。
