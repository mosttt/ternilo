# 部署托管 Worker

本文说明现有 `ternilo-worker` 的接口和部署方式，并非后续“一 Work 一 Docker 容器”的实现。Work 是用户的托管电脑，计划在一个容器内容纳多个工作区；默认每用户一个，可配置数量上限，第一版不提供 Work 内 Docker。当前只预留现有 executor、资源路由和授权接入点，尚无 Work 容器管理或配额产品。Ternilo 客户端与 Server 可以不部署 Worker。

Worker 用来提供托管执行服务。如果只是把自己的电脑或 VPS 接入 Server，安装完整的 `ternilo` 即可，见[远程访问](remote-access.md)。单用户和多用户 Server 都可以使用 Worker，SQLite 和 PostgreSQL 的功能相同。

Worker 只需要 Server 地址和一份专属接入凭据。模型、用户权限、队列和数据库都由 Server 管理；Worker 保存真实项目文件，并在独立 child 中执行任务。

## Docker 与任务隔离

Docker 是 Worker 的可选部署外壳，不是每个 Harness 的独立容器。每次任务运行由 Worker 启动独立子进程，并使用 Bubblewrap 建立文件挂载、进程、网络等命名空间；宿主部署使用 `bubblewrap` 模式，Docker 内使用 `container` 模式，两者都不是为每个任务调用 Docker daemon。Linorun 负责组件生命周期与服务路由，操作系统隔离由 Worker 执行层负责。

任务环境默认无网络，程序和执行配置只读，临时目录独立，只挂载获准的工作目录。Docker 模式下子进程降为 UID/GID 10001 并清除 capability；模型由父进程转发给 Server，子进程不取得模型 Key 或数据库凭据。外层 Worker 需要创建沙箱的权限，不能把父进程权限当成任务进程权限。

多个任务若明确使用同一工作区，仍共享其中的项目文件；隔离不会自动创建文件副本。不同 Agent 预设也不会自动获得不同基础镜像或 Python 环境。独立环境镜像、工作区副本和跨主机恢复仍属于 Work 预留范围。现有沙箱共享宿主 Linux 内核，不是虚拟机级隔离；具体边界见[安全模型](security.md#云端隔离)。

## 开启托管执行

新部署 Server 时加上：

```bash
./ternilo-deploy init --directory ./server \
  --image ghcr.io/mosttt/ternilo-server:0.2.1 --managed-execution-enabled
```

已有 Docker Server 可以直接调整部署设置：

```bash
./ternilo-deploy configure --directory ./server --managed-execution-enabled
./ternilo-deploy up --directory ./server
```

使用可执行文件部署时，在 `ternilo-server serve` 后加 `--managed-execution-enabled`，或设置 `TERNILO_SERVER_MANAGED_EXECUTION_ENABLED=true`。

登录 Server，从独立的“平台管理 → Worker 与执行”入口添加 Worker。复制显示一次的接入凭据；每台独立 Worker 使用自己的凭据。

### 可选：提供平台统一模型

打开“平台管理 → 模型服务”，添加上游 Provider、填写模型 Key，再发布用户可见的模型并授予账号或模型组。用户在托管会话中明确选择模型和授权，Server 负责调用及计量；Worker 不保存模型 Key。无需为了新增模型修改 WorkerPolicy 或重启 Worker。

用户也可以在“我的模型 → 模型 → 账号自有”保存自己的 Provider 和 Key。共享会话使用资源所有者选定的配置，不会自动改用协作者的私人 Key；平台授权还可以决定是否允许在共享资源内使用。

`worker-policy.json` 只配置执行限制，例如工具、运行步数、扩展和文件配额。平台模型的连接地址、Key、目录、推理能力和重试次数在模型管理中配置。

模型 Key 轮换由 Server 暂存加密的旧 Key：新 Key 验证成功后提交，失败时回滚。中断的轮换可查询后继续处理，不要求再次填写旧 Key。部署工具入口见[部署说明](deployment.md)。

## 在另一台 Linux 服务器上启动

安装 Docker 和 Compose，将发行包中的 `deploy/docker/` 放到这台服务器。使用明确的 Worker 镜像版本或 digest；Server 和 Worker 是两个独立镜像。

```bash
read -r -s -p "Worker token: " TERNILO_WORKER_TOKEN
export TERNILO_WORKER_TOKEN

./ternilo-deploy init --component worker --directory ./worker \
  --image ternilo-worker:0.2.1 \
  --server-url https://ternilo.example.com

unset TERNILO_WORKER_TOKEN
./ternilo-deploy up --directory ./worker
./ternilo-deploy status --directory ./worker
```

Server 地址必须能从 Worker 容器内访问，同机部署也可以填写 Server 的正常域名。Worker 不需要开放接入端口；就绪检查默认只监听容器内的回环地址。

初始化会创建私有数据卷，里面保存 `config.json`、工作区文件和存储标识。再次执行 init 会保留已有配置和数据。日常只需要：

```bash
./ternilo-deploy check --directory ./worker
./ternilo-deploy logs --directory ./worker
./ternilo-deploy down --directory ./worker
./ternilo-deploy up --directory ./worker
```

`down` 保留数据卷。不要通过删除数据卷来修复连接问题。Worker 凭据、数据库密码、模型密钥和实例 master key 是不同用途的凭据；Worker 只保存第一种。

不使用 Docker 时，Linux 可执行文件提供同一套入口：

```bash
read -r -s -p "Worker token: " TERNILO_WORKER_TOKEN
export TERNILO_WORKER_TOKEN
ternilo-worker init --server-url https://ternilo.example.com --config-dir ./worker
unset TERNILO_WORKER_TOKEN
ternilo-worker serve --config-dir ./worker
```

宿主部署默认使用 bubblewrap；官方 Docker 使用 container sandbox。两者需要支持 pidfd 的 Linux 内核（5.3 或更新版本），用于确认隔离进程已经退出。Worker 在注册并领取任务前会自动检查隔离环境；检查失败时直接报告启动原因，修复后重新启动即可。`process` 仅适合明确的开发验证，会失去 Linux namespace 隔离。

### 同时执行多少任务

每台 Worker 默认允许 4 个任务执行，最多保留 16 个任务进程。父任务确认正在等待已接纳的子任务时，会让出执行名额，但进程和后台服务仍占用驻留名额。子任务完成后，父任务先重新取得执行名额再继续。提交时仍检查并预留模型预算；空间的 `max_concurrent_runs` 限制实际执行名额，不再把排队中的预算预留都算成正在运行。

首次初始化可设置 `--max-active-runs 4 --max-resident-runs 16`，可执行文件与 `ternilo-deploy init --component worker` 使用相同参数。也可以使用 `TERNILO_WORKER_MAX_ACTIVE_RUNS` 和 `TERNILO_WORKER_MAX_RESIDENT_RUNS`；初始化会保存到私有 `config.json` 的 `capacity`。可执行文件 `serve` 上的参数或环境变量只覆盖本次启动，不改写配置。执行名额必须大于 0，驻留名额不能小于执行名额；这里的 0 不表示无限制。

网页会区分“等待子任务完成”和“等待继续执行”，等待时仍可停止任务或保留草稿。同一目录仍有任务运行时，这组父子任务固定到持有目录的 Worker；该 Worker 的驻留名额全部被等待任务占满、没有能够继续推进的依赖时，最深层排队子任务会明确失败，父任务可继续处理结果。其他机器空闲也无法承接仍被占用的目录。这些名额不是 CPU 或内存配额，应按机器资源调整。

同一目录被其他任务占用时，新任务在网页中显示“等待目录空闲”，仍可停止或继续编辑自己的草稿。等待期间不会发起模型调用；其他空闲目录的任务可以继续执行。

任务结束后，Worker 等待隔离环境内的进程全部退出，再释放目录和驻留名额；单纯显示“运行结束”、关闭外层进程或租约过期都不算退出证明。每次重新占用目录都会取得新代次，旧任务凭证不能再次启动。Worker 会定期检查同一存储上的遗留占用。原进程已退出、且隔离环境内的进程确认停止后，会自动释放占用，让排队任务继续；网络短暂断开、租约超时或重新登记 Worker 都不会单独触发释放。旧任务不会自动重跑，不确定的模型用量也不会因目录恢复而退款。

自动核验要求恢复者能观察原 Worker 所在的同一 Linux 启动环境和 PID 视图。当前宿主部署可在进程重启后完成这条核验；不同主机、宿主重启或更换容器 PID namespace 的情况仍会保留未确认占用。不要通过删除 `.ternilo-occupancy` 来强行解锁。已写入磁盘的正常完成记录可以跨 Worker 重试提交，确认丢失不会要求重新执行任务。

第一次运行任务时，Worker 会准备实际工作目录。之前可以查看命令；技能菜单会提示目录尚未就绪，运行任务后重新打开菜单即可读取。查看目录和技能不会悄悄创建持久工作区。

运行中的会话可通过“更多 → 后台服务”查看和控制 MCP/LSP。控制操作按查看、提交、停止权限分别授权，只发送给当前运行及其有效执行租约；空闲会话不会为了查询而启动新的执行环境。托管服务随这次任务的执行环境结束，不跨任务保留进程。

## 文件存在哪里

每个 Worker 数据根保存一个持久标识。同一空间（tenant）的所有托管工作区固定到同一个存储落点，因此空间的总文件配额不会变成“每台 Worker 各一份”。

不同 tenant 可以使用不同服务器。多个 Worker 共同执行同一个 tenant 的任务时，必须明确共享同一持久文件系统，并使用相同存储标识；单纯填写相同 storage ID 不会把两块独立磁盘合并起来。

Worker 离线时，任务等待原存储恢复。把原 token 指向一个空目录会被拒绝；恢复应带回完整数据根。文件引用和本机缺失的历史附件由 Server 授权后交给正确 Worker，不要求 Server 挂载 Worker 的目录。

## 备份与恢复

一个完整恢复点包括 Server 配置及数据库，以及所有 Worker 的完整数据卷。先停止领取新任务并等待现有任务结束，再停止相关进程。跨主机部署需要逐台执行步骤，不会自动通过 SSH 搜索机器。

1. 在能访问 Server 的机器上暂停执行。命令会提示实例管理员账号和密码，并等待活动 run 和命令都归零；临时登录会话在命令结束时注销。

   ```bash
   ./ternilo-deploy pause-execution --server-url https://ternilo.example.com
   ./ternilo-deploy execution-status --server-url https://ternilo.example.com
   ```

   返回的 `claims_paused` 应为 `true`，`active_runs` 和 `active_commands` 都应为 `0`。等待超时会保持暂停，不能据此开始备份。非交互运行可提供 `TERNILO_SERVER_OWNER_USERNAME`／`TERNILO_SERVER_OWNER_PASSWORD`，或现有实例管理员的 `TERNILO_SERVER_ACCESS_TOKEN`。

2. 在每台 Worker 主机上停止服务，并记录各数据卷对应的 Worker。

   ```bash
   ./ternilo-deploy down --directory ./worker
   ./ternilo-deploy backup --directory ./worker --output-dir ./backups
   ```

   备份包含配置、存储标识、隐藏文件、附件对象、符号链接和原文件 UID/GID；不会只复制可见项目文件。备份包含 Worker 凭据，请妥善保管。

3. 停止 Server，再按[部署文档](deployment.md)备份 Server。SQLite 随 Server 数据卷保存。PostgreSQL 应在 Server 停止期间完成 `pg_dump`，通过 `--database-dump` 一并纳入 Server 备份。

4. 按每台 Worker 分目录保存备份和 `.sha256`，与同一恢复点的 Server 备份一起保管。可用 `ternilo-deploy verify --archive 备份文件` 独立检查归档；该操作不启动服务或修改数据卷。重新启动原部署后，执行 `resume-execution` 恢复领取任务。

恢复到新部署目录：

```bash
./ternilo-deploy restore --directory ./restored-worker \
  --archive ./backups/ternilo-worker-<timestamp>.tar.gz
```

恢复前检查本机镜像与备份记录的准确 ID，恢复后的部署固定到该镜像内容。恢复会使用新的空数据卷，保留原存储标识和文件权限。Server 地址变化时加 `--server-url`；Worker token 应与同时恢复的 Server 数据库匹配。先恢复并启动 Server，再启动各 Worker，确认 `status` 就绪后执行：

```bash
./ternilo-deploy resume-execution --server-url https://ternilo.example.com
```

只恢复新建空工作区不能验证旧数据恢复。实际检查至少应包含一个已有文件、一个历史附件，以及 Worker 重启后的继续执行。

## 执行隔离和排查

官方 Worker 容器使用只读根文件系统。父进程保留创建隔离环境、挂载工作区和切换文件所有者所需的 Linux capability；每个 child 使用独立 mount/PID/network/IPC/UTS namespace，无网络，降为 UID/GID 10001，并清空环境及 capability。

“待接入”表示尚未完成第一次注册；“在线”由 Server 的当前租约计算；“离线”表示没有有效租约；“已撤销”表示凭据已停用。注册过不等于当前在线。

连接失败先运行 `logs`，确认 Server 地址可达、凭据未撤销、原数据卷已挂载。接入凭据被撤销时，旧进程不能继续续租或提交结果，正在进行的模型连接也会被关闭。
