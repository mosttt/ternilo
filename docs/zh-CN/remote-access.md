# 从其他设备管理电脑与 VPS

每台要执行任务的机器运行 `ternilo serve`，一台 Server 提供统一入口。你从手机、平板或另一台电脑打开 Server 网页，就能继续使用目标机器上的文件、会话、模型和插件；执行机器无需设置端口映射、VPN 或公网监听。

同一账号可以接入笔记本、台式机、家用服务器和多台 VPS。单用户模式供自己使用；多用户模式再增加其他账号与明确的共享权限。自己的 VPS 与自己的电脑采用相同接入步骤，只有提供平台托管执行时才需要另行部署 Worker。

Server 默认使用用户名和密码登录，也可选配 OIDC。单用户模式只允许实例所有者访问；开启多用户后可以邀请其他账号，并分别分享查看、发送、停止和配置权限。空间管理员与实例所有者是不同权限，不会因担任空间管理员就获得所有电脑。

`ternilo` 和 `ternilo-server` 可以在同一台电脑或 VPS 上运行。即使同机，本地 `ternilo` 也只监听回环地址，其他设备使用 Server 的认证入口；不需要把本地 `3210` 端口开放到局域网或公网。

先按[服务器部署](deployment.md)启动 Server。SQLite／PostgreSQL 与使用模式分别选择，连接自己的电脑不需要 OIDC 或托管 Worker。

## 1. 在需要工作的电脑上准备 Ternilo

已有交付包时使用其中的 `bin/ternilo`；从源码启动见[本地快速开始](getting-started.md)。仅在这台电脑工作时，`ternilo serve` 已足够；远程访问只是给同一个命令增加连接参数，不安装第二套 Node 程序。

如果原本已运行本地服务，先用 `ternilo stop`（自定义目录加 `--data-dir`）停止并等待成功返回，再使用相同数据目录和原有 Profile 启动远程连接。不要同时运行两个服务打开同一数据目录；桌面作为客户端可以与网页共用该服务。

## 2. 取得电脑连接凭据

1. 登录 Server，进入“用户设置 → 我的机器”，填写电脑名称，例如 `工作电脑`、`家庭电脑` 或 `VPS`；ID 由系统生成且不可修改。
2. 生成启动命令。页面使用一次性登记授权取得可撤销的电脑凭据，只显示一次。
3. 把命令放到要连接的电脑执行；格式如下。
4. 回 Server 确认电脑在线。已有工作区和会话会被发现并沿用；也可以选择这台电脑上的新工作文件夹。

接入更多机器时重复上述步骤，为每个独立实例填写唯一名称，由系统分配各自的 ID 和凭据。此处不需要配置数据库、再创建一套模型设置或复制项目文件。

```text
ternilo serve --gateway-url "wss://agent.example.com/api/v1/executors/connect" --node-id "ter_pc_GENERATED_ID" --token "ter_n_REPLACE_WITH_YOUR_TOKEN"
```

此单行参数形式适用于 Bash、PowerShell 和 CMD；使用页面生成的 ID 和 Token，不要自行编造 ID。长期服务仍可使用下面的环境配置文件。


电脑凭据用于长期重连，不要复用其他用户或其他电脑的值。凭据丢失或已吊销时，选择“恢复原电脑接入”，沿用原 ID、归属及资源关联。正常断线与重启不重新登记；凭据轮换单独后移。

已登记电脑可以在[电脑管理](computers.md)中修改电脑名称和备注、查看详情、暂停或恢复接入。暂停保留原凭据；移除登记会撤销接入并隐藏登记，保留已有工作区和历史。

## 3. 在本地与手机验证

目标机器有桌面时，可以先打开本机的 `http://127.0.0.1:3210`，选择工作文件夹、配置模型并发送一个读取任务。没有桌面的 VPS 可以直接从 Server 网页完成这些操作。

手机打开 Server 公开地址并登录，选择要使用的机器和工作区。打开已有会话继续工作，或新建会话；选择文件夹时，浏览的是目标机器的目录，不是手机目录。针对该电脑的模型与插件配置保存到目标机器；账号模型和平台模型则保存到 Server，使用哪台电脑执行与选择哪个模型来源分别设置。

局域网可用 Server 的实际 IP 和监听端口访问；`0.0.0.0` 表示监听所有网卡，不是给客户端填写的服务器地址。浏览器将 localhost／回环地址视为可信上下文，但普通 HTTP 局域网地址不是：基础工作台、原生登录与文件标签仍须正常使用，依赖安全上下文的 PWA、部分剪贴板和 OIDC 功能则需要 HTTPS。远程长期使用应通过 HTTPS 入口，不要把 HTTP 作为公网账号与凭据传输方案。

原生登录只保存在当前标签页。使用侧栏“退出登录”清除该标签页的登录状态。若服务器启用 OIDC，可先用原生账号进入“设置 → 通用 → 账号”，显式绑定组织登录；绑定保持同一账号和原有资源，不按邮箱自动合并。

手机可将网页安装为 PWA。离线时只缓存公开界面，不提供离线执行；恢复网络后会补齐断线期间的会话记录。

## 同一台电脑运行多个实例

可以同时运行多个实例，并全部连接同一个 Server。每个实例使用独立端口、数据目录、系统分配的 ID 和凭据。在“我的机器”中分别登记 `home-a` 与 `home-b`，然后在两个终端执行：

```text
ternilo serve --listen 127.0.0.1:3210 --data-dir "./ternilo-data-a" --gateway-url "wss://agent.example.com/api/v1/executors/connect" --node-id "ter_pc_GENERATED_ID_A" --token "ter_n_REPLACE_WITH_TOKEN_A"
ternilo serve --listen 127.0.0.1:3211 --data-dir "./ternilo-data-b" --gateway-url "wss://agent.example.com/api/v1/executors/connect" --node-id "ter_pc_GENERATED_ID_B" --token "ter_n_REPLACE_WITH_TOKEN_B"
```

相对数据目录以启动时的当前目录为基准；长期运行建议改成固定绝对路径。两个本地网页分别访问 `http://127.0.0.1:3210` 和 `http://127.0.0.1:3211`。它们的会话、模型、预设和凭据分别保存，Server 中显示为两个 Node。不要共用数据目录；同一 Node 身份重复连接会替换之前的连接。多个实例可以选择相同项目目录，但同一用户并行修改同一文件仍可能冲突，应按任务划分目录或 Git worktree。

每个 Node 凭据在首次同步时绑定所用数据目录。之后启动或重连应保持相同目录；使用另一个目录时登记独立 Node／凭据。账号停用后的受管任务清理由原目录中的 Node 确认，空目录不能代替原实例提交完成回执。

查看或停止指定实例时也必须带对应数据目录，例如 `ternilo status --data-dir "./ternilo-data-b"` 和 `ternilo stop --data-dir "./ternilo-data-b"`。桌面端的单实例窗口行为与 CLI 服务不同；双击桌面应用不会自动新建第二个独立 Node。

## 4. 长期运行

Linux 可使用用户级 systemd 服务。先将 `ternilo` 安装到 `~/.local/bin/`，创建 `~/.config/ternilo/remote.env`，权限设为 `0600`：

```dotenv
TERNILO_LOCAL_GATEWAY_URL=wss://agent.example.com/api/v1/executors/connect
TERNILO_LOCAL_TOKEN=<computer-token>
TERNILO_LOCAL_NODE_ID=ter_pc_GENERATED_ID
TERNILO_LOCAL_MAX_STEPS=0
```

使用自定义数据目录或 Profile 时，也把 `TERNILO_LOCAL_DATA_DIR`、`TERNILO_LOCAL_PROFILES` 写入这个文件，沿用手动启动时的值。服务环境不会自动继承你当前终端中的临时变量；漏掉自定义数据目录会打开另一套数据。

创建 `~/.config/systemd/user/ternilo.service`：

```ini
[Unit]
Description=Ternilo local and remote workspace
After=network-online.target
Wants=network-online.target

[Service]
EnvironmentFile=%h/.config/ternilo/remote.env
ExecStart=%h/.local/bin/ternilo serve
Restart=on-failure
RestartSec=3
TimeoutStopSec=120

[Install]
WantedBy=default.target
```

启动并查看状态：

```bash
systemctl --user daemon-reload
systemctl --user enable --now ternilo
systemctl --user status ternilo
journalctl --user -u ternilo -n 100
```

需要用户登出后继续运行时，由系统管理员按主机策略启用该用户的 lingering。电脑睡眠、关机或断网都会影响远程操作，应按实际使用安排电源设置。

通过 systemd 启动时，用 `systemctl --user stop ternilo` 停止；手动启动时可用 Ctrl-C 或 `ternilo stop`。正常关闭会停止接纳新请求、取消交互等待、保存会话并清理插件，同时等待已经接收的远程指令完成收尾并持久保存结果，之后才释放本地服务。网络断线本身不取消已接收的指令，重连时可复用已保存的结果，避免重复执行。不要强制结束进程来代替正常关闭。

## 连接参数与环境变量

连接与长期运行参数支持固定环境变量，显式 CLI 优先：

| CLI | 环境变量 | 默认值/用途 |
|---|---|---|
| `--gateway-url` | `TERNILO_LOCAL_GATEWAY_URL` | 不设置时只在本地使用 |
| `--token` | `TERNILO_LOCAL_TOKEN` | 设置远程地址后必填；长期服务建议用环境配置 |
| `--node-id` | `TERNILO_LOCAL_NODE_ID` | Server 分配的固定 ID；使用生成命令中的值 |
| `--listen` | `TERNILO_LOCAL_LISTEN` | `127.0.0.1:3210` |
| `--no-local-web[=BOOL]` | `TERNILO_LOCAL_NO_LOCAL_WEB` | `false`；远程连接但不提供本机网页 |
| `--allow-insecure-gateway[=BOOL]` | `TERNILO_LOCAL_ALLOW_INSECURE_GATEWAY` | `false`；仅用于明确的明文开发联调 |
| 可重复的 `--profile` | `TERNILO_LOCAL_PROFILES` | 环境配置用逗号分隔多个 Profile 路径 |
| `--data-dir` | `TERNILO_LOCAL_DATA_DIR` | 与本地默认数据目录相同 |
| `--max-steps` | `TERNILO_LOCAL_MAX_STEPS` | `0`，宿主不设置固定步数上限 |

布尔项可用 `=false` 覆盖环境中的 `true`。`--no-local-web` 只禁用本机网页，认证后的 API 和 `ternilo status/stop` 仍可用。需要打开桌面时应移除该参数并重启服务。电脑范围的设置、模型及预设保存在运行电脑，账号和平台范围的设置保存在 Server；会话内步数和插件配置仍从普通设置入口修改，不要求编辑服务环境文件。

Server 与电脑使用匹配的执行器协议接入；当前为 `47`，已部署的 Worker 也须同步升级。升级和回滚步骤见[部署维护](deployment.md#升级与回滚)。

## 断线、数据与权限

- 本地与远程网页共用电脑上的同一份数据与后台服务，不会产生两套会话或协作任务板。
- 关闭网页或手机断网不会自动取消已经开始的任务。电脑与 Server 的控制连接短暂中断时，已接收的本机任务仍可继续，重连后补齐记录。账号／平台模型另经 Server 的模型请求链路调用；Server 停止或该链路断开可能使当前调用失败，不会自动切换来源或重做已接收的模型请求。电脑本身关机、睡眠或进程中断也不能当作普通控制重连。
- 电脑离线时仍可查看 Server 已保存的工作区、会话与部分历史；发送任务、更改目标机器设置等操作需要它重新上线。界面不会把缓存当作执行成功。
- Server 保存电脑归属、工作区／会话关联、共享权限、已接收的事件及账号／平台模型配置；实际项目文件和电脑范围的模型、插件配置仍在电脑上。不同机器即使存在相同内部会话 ID，也会分别管理。
- 已派发命令有持久记录以避免重复执行。进程异常退出时，某项外部操作可能已经发生却来不及确认；这种情况不会自动重做，应先查看会话与文件结果，再决定是否重试。
- Server 可见转发内容，不是端到端不可见代理。服务器日志、备份和进程仍由部署者保护。
- “只读”“工作区写入”“完整访问”保留原来的本机含义；移动端操作仍需遵守审批及目标机器的文件、终端限制。授予他人“发送任务”后，对方使用该会话已有的执行权限，共享本身不会额外隔离一份操作系统。

同一用户可以并行使用同一电脑上的相同或重叠工作目录；本机操作与登记该电脑的所有者账号按同一归属协调。不同账号使用重叠目录时需要等待前一使用者释放目录。这是协作进程的目录协调，不会为每个任务复制文件，也不协调多台电脑上的独立文件系统；需要避免同用户并发改写同一文件时，应自行划分目录或工作树。

## 排查连接

| 现象 | 检查方法 |
|---|---|
| 网页能开但电脑离线 | 在目标机器执行 `ternilo status`，检查服务是否运行、连接地址与凭据是否正确，再看原终端或服务日志 |
| 手机无权登录 | 检查账号登录状态；单用户模式暂停其他账号访问，成员还需对应资源的共享权限 |
| 网页流式停住或频繁掉线 | 外部网关需关闭流式缓冲并转发 WebSocket Upgrade，不能只代理普通 HTTP |
| 数据目录已被占用 | 使用 `ternilo status` 查找已有后台服务；修改启动配置前用 `ternilo stop` 正常停止，再以同一服务提供本地与远程 |
| 其他设备无法访问电脑的 `3210` 端口 | 本地端口仅供当前机器使用，不能改成 `0.0.0.0` 或局域网地址；从其他设备访问 Server 的公开 URL，再确认电脑在线 |

本机明文联调可以让服务器监听 `127.0.0.1:4321`，电脑用 `ws://127.0.0.1:4321/api/v1/executors/connect` 并显式传 `--allow-insecure-gateway`。正式远程使用外部网关提供的 HTTPS/WSS。服务器一致备份、恢复和令牌轮换见[部署维护](deployment.md#备份恢复升级与轮换)。

备份 Server 不会备份连接电脑上的项目文件和本地配置。需要恢复原有工作时，还应分别保存每台机器的 Ternilo 数据目录与实际项目目录，见[本地数据与备份](getting-started.md#5-数据与备份)。
