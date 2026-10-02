# Server 技术参考

Server 统一使用 `ternilo-server init`／`serve`／`admin`，电脑端 Node 角色由 `ternilo serve` 承担。Server 的单用户／多用户模式保存在同一数据库；两种模式均可选择 SQLite 或 PostgreSQL，均不强制要求 OIDC 或 Worker。

Server 的 Control、Cloud 和 Gateway 资源都通过同一个 Database 访问层持久化。用户通过原生账号或可选 OIDC 登录，同一工作台按资源执行位置路由：

| placement | 执行者 | 文件 | 调度路径 |
|---|---|---|---|
| `local_node` | 用户登记的 `ternilo serve` | 用户电脑 | Server 经当前 WSS 连接转发 typed application command |
| `cloud` | 独立 Worker 的隔离 child | 持久 Workspace 文件 | Server 编译不可变 RunSpec，使用同一数据库的队列／租约／用量事务 |

Server 不运行 agent loop。个人电脑和 VPS 可以运行完整 Ternilo，并主动连接 Server；托管 Worker 通过出站 HTTP 领取任务，在自己的持久目录中运行隔离 child。Worker 不需要数据库连接、实例 master key 或模型密钥。单用户和多用户模式都可以启用托管执行。部署步骤见[托管 Worker](worker.md)。

## 网络发布边界

Server 只提供 HTTP upstream，Ternilo 不内置 HTTPS/TLS、ACME、证书或续签。生产 Compose 默认把 Server 发布到宿主 loopback 的可配置端口；同机外部 gateway 直接连接 loopback。gateway 与 Ternilo 不在同一主机时，部署者必须显式绑定受控私网地址，并用网络 ACL 限制只有 gateway 可以访问，不能把明文 upstream 直接暴露到公网。

DNS、公开 HTTPS、TLS 策略、ACME、证书生命周期和入口流量策略均由外部 gateway 或负载均衡器负责。它必须保留流式响应的即时刷新，并支持 `/api/v1/live` 和 `/api/v1/executors/connect` 的 WebSocket Upgrade。Control 的 `public_url` 保持外部 HTTPS origin；Web 与 Node 使用同一公开 origin 派生或取得 WSS 地址，而不是使用内部 HTTP 监听地址。

Server 配置保存在私有 `config.json`，`TERNILO_SERVER_*` 与显式 CLI 可覆盖对应字段，CLI 优先。数据库和密钥仍使用 `TERNILO_DATABASE_URL`、`TERNILO_MIGRATION_DATABASE_URL`、`TERNILO_SECRET_MASTER_KEY`；离线轮换另外接收 `TERNILO_NEXT_SECRET_MASTER_KEY`。WorkerPolicy 只保存执行上限，平台模型及其凭据通过独立管理 API 保存；两者均独立于账号模式。完整流程见[部署文档](deployment.md)。

## 原生 Server 备份与恢复

直接运行二进制的安装与首次初始化见[非容器部署](deployment.md#非容器部署)。已有实例恢复时沿用备份中的配置和主密钥，不要重新运行 `init` 创建另一套身份或密钥。

完整备份前正常停止 Server，保存私有配置、实际数据库和持久文件。默认 SQLite 数据库位于实例目录的 `data/db/` 下，但 `database_url`、`workspace_root` 或外部 WorkerPolicy 可以指向其他位置；仅保存配置目录不能保证包含这些外部数据。数据库地址或主密钥由环境变量覆盖时，也要安全保存实际使用的环境配置。PostgreSQL 需要另外保存一致的数据库转储。连接电脑与 Worker 的文件也需分别备份，见[本地数据与备份](getting-started.md#5-数据与备份)及[Worker 备份与恢复](worker.md#备份与恢复)。

SQLite 也可在线生成一致的数据库快照：

```bash
ternilo-server admin backup-sqlite --config-dir /path/to \
  --output /private/backups/server-snapshot.sqlite3
```

该命令拒绝覆盖已有输出，快照权限为 `0600`。快照只包含已提交的数据库内容，不包含配置、主密钥和 Workspace 文件；另行保存与它匹配的私有配置及所需文件，不能把数据库快照当作完整备份。

恢复到新环境时：

1. 使用与备份匹配的程序，在新的空目录恢复。完整停机目录备份应整体还原；在线快照则单独复制到新的数据库路径，不要与另一份数据库留下的 `-wal`、`-shm` 文件混用。
2. 保留原 `secret_master_key`，核对复制后配置的 `database_url`、`migration_database_url`、`workspace_root` 和外部策略路径。配置保存的绝对路径不会随目录复制自动迁移；更换端口或公开地址时，也要调整 `listen` 和 `public_url`。
3. 保存恢复后的配置，再用 `ternilo-server serve --config-dir /path/to/restored` 启动。也可用 CLI 或环境变量覆盖路径，但覆盖只对本次启动生效，后续服务管理器启动时必须继续提供。
4. 用原账号重新登录，确认模型配置和已保存凭据可用；恢复 Node 的启动配置，确认电脑在线、工作区及历史一致，并实际验证远程文件读取或任务。

若数据库来自在线快照，可能仍保存着旧 Server 的连接租约，Node 在租约失效前会暂时无法接入。此时历史仍可能从缓存显示，但目标电脑的模型、插件或目录请求会返回 `503`。等待 Node 重新连接并确认在线后重试；不要把首页或历史可见当作恢复完成，也不要据此清空机器记录或重新生成主密钥。

## 统一 Workspace 与 Session

Workspace 是项目、文件根、执行位置和 Session 的绑定。公共浏览器只使用稳定 `workspace_id`：

- Cloud Workspace 绑定 tenant/project 和平台持久卷。
- 本机 Workspace 固定绑定原存储账号、Node executor 和 Node 返回的 opaque Workspace ID；当前管理者独立保存，本机绝对路径不会写入 Control。
- Session 创建后固定 placement；从侧边栏移除 Workspace 只解除列表登记，不迁移文件或已有 Session。
- Server 先按已认证账号、membership 与显式资源权限解析 placement，再路由请求；浏览器不能自报 executor、Node Session ID 或宿主路径。

共享工作台 API 覆盖 Workspace/Session 列表、模型、Chat、queue/steer、停止、问题、Skills、Agent Team、附件、导出、统计和调用轨迹。Node 与 Cloud adapter 尽量返回相同的公共 DTO；执行位置特有的能力会明确报告 unavailable，不用空结果冒充成功。会话搜索是跨位置读取聚合：Server 保留实际 actor 可 View 的缓存标题与 Cloud canonical 命中，并只在短预算内补充在线 Node 的脱敏内容命中；单个 Edge executor 离线或失败不会把其他来源一起变成错误。

`local_node` Session 的 Agent Team snapshot/task/mailbox 请求经 typed executor protocol 到达 Node，并使用 Node 的持久 Session tree 与 `agent-team.sqlite3`。Cloud Session 使用统一数据库中的 canonical child Session tree、共享任务与成员邮箱；模型内 7 个 Team 工具经 child→parent host pipe 调用同一个 store，Web 也直接读取该 store，不维护浏览器或 Control 内存副本。

## Node 执行链

1. 已授权成员在“我的机器”生成归属自己的 enrollment，并立即换取只显示一次的 Node credential。
2. `ternilo serve --gateway-url ...` 使用 credential 主动连接 `wss://<platform>/api/v1/executors/connect`。
3. Control 验证 credential 哈希、tenant、executor state 和握手 ID，然后把在线连接登记到 Edge Gateway。
4. 连接时发现 Node 原有 Workspace／Session 并建立公开 ID 映射；也可经 Node 浏览／创建目录。数据库保留归属和不可变绑定，不把不同电脑的同名 ID 当作同一资源。
5. Session operation 由 placement resolver 发往当前连接。Node 再次校验 command scope/expiry，并在自己的 `LocalApplication` 中执行。
6. Node 返回 reply 和连续 event batch；Control 只缓存属于已映射 Session 的脱敏事件与 cursor。

Node credential 可显式吊销；吊销会使当前连接断开。Node 离线时，必须抵达 Node 的新操作返回 `503 unavailable` 和重连/重试提示；已经开始的任务继续由本机 runtime 决定，重连后按 durable cursor 补读。Provider Key、OAuth secret 和授权回答只在线转发，不进入 Control event cache。用户步骤见 [远程访问与 Node](remote-access.md)。

## Cloud Run 执行与结算

1. 用户通过统一 Session API 或 `POST /api/v1/tenants/{tenant}/runs/chat` 提交任务。
2. Server 根据实际 actor 校验资源权限，使用 owner 的配置与配额编译 RunSpec；配额预留和入队在同一事务中完成，Run、提交、持久命令和审计分别记录实际 actor；后台子代理继承原任务的操作者与授权来源。原始 CloudRunDraft 允许完整 profile，保持 owner-only。
3. `cloud_runs` 保存 canonical JSON digest、reservation、实际操作者、授权来源会话和队列状态。
4. Worker 只提交作用域明确的租约标识。Server 从数据库读取 RunSpec、owner 和 Workspace，并在每次事务重新检查凭据、generation、租约和 writer fence。PostgreSQL 领取使用行锁和 SKIP LOCKED；SQLite 使用串行写事务，业务判断相同。
5. 父进程校验 WorkerPolicy、插件和 Control 已验签分发的 Extension 精确 package version，然后为该 run 启动无网络 child。
6. Child 不含 Worker token、数据库 URL、Provider Key 或宿主环境；模型请求经有界 pipe 到父进程，再由父进程转发给 Server。
7. Server 在同一事务核对 Worker、租约、资源权限和明确模型绑定，再接受逻辑模型请求。平台绑定包含 grant、公开模型与受益账号，BYOK 绑定包含空间、所有者和 Provider／model。每次真实重试另记 attempt，密钥只在 Server 内解密和使用。
8. Child 调用 `ask_user` 时，父进程请求 Server 保存问题。用户在网页提交结构化答案后，父进程把答案交还同一个 child 和 run，不重放已经发生的步骤。
9. Server 保存上游实际报告的用量，未报告字段保持未知。已知用量幂等结算；未知消耗保留预留，不伪装为零。用量已到而最终完成事件尚未到时，取消也会保留已观察到的记录。
10. 父进程提交事件和终态；Server 验证连续 event sequence，并从权威 usage ledger 结算成功、失败、取消或 reaper 终态。

状态流转：

```text
queued → leased → running | cancel_requested
                       └→ succeeded | failed | cancelled | indeterminate
```

尚未启动的 leased run 可在 operator retry 预算内重新领取；running run 丢失 lease 会成为 `indeterminate`，不会重放可能已经产生副作用的任务。取消或 child crash 后，已接受模型调用仍可独立保存迟到的可信用量，不要求原 Worker 凭据或 Writer lease 继续有效。

如果 child 在工具执行中途退出，Server 会在结束旧 run 的同一事务内补齐“结果未知”的工具／命令结束事实和 turn 结束事件。事件继续归属于旧 run，并更新会话游标和遥测，然后才推进队列。后续 Worker 拿到已经闭合的历史，不能借新任务的租约写入旧 run。

自动子任务在 Server 提交入队事务后，才向执行中的父任务确认已接纳，并返回真实会话与运行标识。入队与等待结果分开；父运行、租约和写入代次在同一事务记录，历史 Run 删除后仍保留关系。普通提交、人工续发和普通分叉建立新的调度根。子任务的读取与取消逐次校验所属父运行；已请求停止的父任务只能清理已接纳子任务，不能继续创建或入队。

父轮正常成功结束时，已经接纳并明确交付给调用方、且没有被取消的后台子任务继续运行，使用自己的 Run、预算和租约。父轮停止、失败、关闭失败，以及尚未交付的启动被丢弃时，Worker 会清理相应子任务并等待取消确认后再结束。收到接受回复不等于已交付，关闭父轮也不等于停止它已交付的后台任务。父轮结束后，查看或管理仍在运行的子任务应使用子会话本身的权限；旧父运行的租约不能继续使用。整个 Worker 失联后的处置仍遵循租约与不确定终态规则。

父任务等待已接纳的子任务时，会让出前台执行名额，但仍占用驻留名额，因为进程和后台服务还在。等待结束后必须重新取得前台名额再继续。Worker 默认允许 4 个前台任务、16 个驻留任务，可在初始化或启动配置中调整；模型预算和模型请求并发分别管理。

共同使用一个目录的父子任务，在占用期间固定到同一 Worker。该 Worker 的驻留名额已满、所有驻留任务都在等待、且没有可以继续推进的依赖时，系统会明确结束最深的未启动子任务，让父任务收到容量不足的结果；这个子任务不运行模型、不产生模型消耗。其他 Worker 的空闲容量无法承接仍被占用的目录，也不会让这组任务无限等待。

尚未获准启动的领取失效时，执行名额和目录登记一起释放，其他 Worker 可以在原重试预算内领取。已经获准启动的任务不同：即使租约过期或历史已结束，目录仍需执行端确认进程退出后才能交接。现有恢复核验依赖同一 Linux 启动环境和 PID 视图，完整的跨主机故障接管尚未提供，不能仅凭超时强行开放目录。

## Control plane 边界

### 身份、租户与成员

- 原生密码使用 Argon2id，原生会话和一次性邀请只存摘要；每个账号与其个人空间、默认项目在同一事务创建，平台邀请与团队邀请分别处理。OIDC 校验签名 ID Token 的 issuer、client audience、expiry、iat、nbf、nonce、azp 和存在时的 at_hash，UserInfo 的 sub 必须一致，拒绝共享密钥 JWT 算法。上游 access token 无需是 JWT；校验后签发本站 OIDC 会话，访问及刷新凭据只存摘要，上游刷新凭据加密保存。
- 原生账号显式绑定 OIDC 时验证两种凭据并保持同一 user_id；相同邮箱不自动合并，其他账号已占用的 issuer＋subject 返回冲突。单用户模式仅暂停非实例所有者的访问，不删除或迁移任何资源。
- `viewer < member < admin < owner` 单调 RBAC；只有 owner 能管理 owner，最后一个 owner 不能被移除或降级。
- 两库共用 membership 与 View／Submit／Stop／Configure 资源权限。PostgreSQL 额外用 RLS 限定数据库作用域；SQLite 由同一 Server 授权事务执行，不以数据库种类削减功能。租户管理员不会自动取得其他用户的私有会话。
- URL、header 或请求体中的 tenant/project 只用于定位，不能替代已验证身份和 membership。
- 页面退出登录会同时清除 OIDC access/refresh 状态和当前 tenant、Workspace、Session 选择；再次登录从服务端重新读取 tenant roster，不复用退出前的客户端快照。

### Enrollment、配额与审计

- Enrollment token 最长一小时且只能消费一次；Node credential 只保存哈希，签发后没有时间到期，直到显式吊销电脑才失效。已登记电脑不能刷新或重新签发 credential；已吊销或已移除的电脑通过显式恢复签发新凭据并沿用原 ID；正常重启及暂停恢复不重新登记。
- 电脑管理使用独立 `computer_management` 组件。`/tenants/{tenant}/my-computers/{id}` 为本人入口，`/tenants/{tenant}/executors/{id}` 为具有对应职责的空间入口：`GET` 读取详情，`PATCH` 保存 `{name, notes, expected_revision}`，`PUT /suspension` 保存 `{suspended, expected_revision}`，`DELETE /registration` 接收 `{expected_revision}` 并移除登记；原 `DELETE` 仍表示吊销。状态操作按版本拒绝冲突，暂停不能经原凭据重新连接，恢复沿用原凭据，移除保留关联工作区和历史。详情和列表采用较新的服务端心跳或认证时间。
- 登记请求为 `{name, project_id?, ttl_seconds?}`，ID 由系统生成；同一账号和空间内名称唯一。列表默认隐藏已移除项，`?include_removed=true` 才读取全部可见登记。`POST /recovery` 接收 `{name, expected_revision}`，仅恢复已吊销或已移除的原身份，保留归属、项目及持久目录绑定。
- Control 限制 Node 数、并发 run、月度 model token、secret 和扩展数量；Run 预留与模型 attempt 分配在数据库事务中处理，不重复预占同一空间额度。结算只接受 Server 观察到的用量，不提供客户端自报实际消耗的 commit 接口。
- 空间管理 → 用量供当前空间 owner/admin 按 UTC 月查看 token ledger 与 reservation，并单列不受月份筛选影响的全时段异常；member/viewer 无入口且 API 拒绝。它不是金额账单：空间预算和相关用量按 Run 首次预留的 UTC 月份归属，平台模型授权按逻辑模型请求接受月份归属；各次重试保持该月份。页面分别显示已知消耗、未知预留和活跃 Run 剩余预留，可相加查看占用；当前配额上限不是历史快照。
- 审计日志 append-only，并以 SHA-256 hash chain 连接；支付、定价、发票和税务属于外部运营系统，不是 usage ledger 的职责。

### Secret 与 BYOK

Tenant secret 和用户 Provider Key 使用 XChaCha20-Poly1305 加密，AAD 绑定作用域、名称和版本。浏览器只能写入/轮换或读取 metadata，没有回显明文的 API。

平台 Provider 的 endpoint/key 由独立模型管理保存，Key 使用单独的加密作用域。用户 Provider 的 endpoint、模型目录和 write-only Key 按 tenant + user 存储；“我的模型”管理的是该账号规范个人空间的来源，团队托管会话通过明确绑定使用它，不能把当前执行 tenant 当成模型配置 tenant。已有任务保留原完整绑定，空间任务预留仍属于实际执行空间。两种模型调用都由 Server 发起；Worker 只拿到不含凭据的执行策略和流式结果。Server 的 master key、数据库，以及每个 Worker 的完整持久目录应保留为同一恢复点，步骤见[Worker 备份与恢复](worker.md#备份与恢复)。

## WorkerPolicy 与模型 broker

生产示例为 [`deploy/docker/worker-policy.json`](../../deploy/docker/worker-policy.json)。关键字段：

- `catalog_revision` 必须与 Control/Worker 链接的 catalog 完全相同。
- `policy_revision` 写入每个 RunSpec；执行 allowlist 或 ceiling 改变时递增。
- `maximum_limits`、`max_run_attempts`、Workspace quota、`denied_tools` 和 `allowed_plugin_kinds` 是 operator 上限。
- `max_extension_packages_per_run` 与 `extension_host_policy` 分别限制每轮已分发 Extension 数量、可授予的 capability 和两个 runtime 的资源上限。正式 Docker policy 当前允许 `ternilo.extension.package`、每轮最多 4 个包，并只允许 `log` 与 `workspace_read`；Worker child 仍不能安装、信任、撤销或卸载 tenant inventory。
- 官方 policy 保留 `ternilo.tool.ask_user`，使 Cloud Session 可以暂停并等待网页回答；从 allowlist 移除它会使模型目录不再暴露该工具。
- Cloud 真实模型使用 `ternilo.model.host_gateway`；带 secret 的直接 Provider 不进入 child。
- 平台模型目录来自账号或模型组获得的授权。上游连接、Key、模型能力和重试次数由独立模型服务管理。
- Provider 默认值包含 `context_window`、`max_output_tokens` 和可选推理档位映射；exact model 的 `settings.mode: "automatic"` 分开保存 `upstream` 和 `overrides`，逐字段按手动值、上游值、Provider 默认值解析。推理的 `mode: "disabled"` 明确关闭，缺失则继续继承；原 `inherit` 与完整 `override` 的含义保留。
- Cloud Session 保存可选的明确模型绑定与能力快照。没有模型时可创建空白会话，提交任务前必须选择可用模型。Server 按所选协议把统一推理档位映射为实际请求：Chat Completions 的 `reasoning_effort`、Responses 的 `reasoning.effort`、Gemini 的 `thinkingConfig` 或 Claude 的 `thinking`／`output_config`。模型需明确配置支持的等级或 token 预算，不按模型名猜测；详细映射见[模型说明](models.md)。

Server 编译不可变 RunSpec。Worker 父进程为 child 准备 `ExecutionEnvelope`：`workspace` 指向该次隔离环境中的目录，`extensions` 包含已授权的精确扩展版本，`attachment_objects` 用于恢复本机缺失的合法历史附件。Child 以自己的 UID 校验并保存对象；Worker 不接受任意宿主路径或任意 digest 下载。

Extension 的签名 `config_schema` 也在控制面生效：Cloud Session create/PATCH/submit 和用户 Agent preset PUT 会在持久化前解析每个 enabled mount 的已验签 distribution 并校验 settings；同一 reserve 边界还会拒绝同一 package/version 重复 mount 与多个包贡献同名 Tool，不把组合失败推迟到 Worker child boot。Worker `compile_draft` 在运行前再次验证。缺失 required、类型或 pattern 错误会在用户动作边界直接拒绝，不会先保存一个注定无法启动的 Cloud Profile。没有 enabled Extension 的普通 Profile 不开启额外 distribution transaction。

Control 的 append-only install audit 永久固定每个 `package_id@version` 的 publisher/payload/signature 和 capability grants identity。卸载后可重装完全相同的 signed bundle/grants，但换代码、签名或 grants 必须发布新版本。安装会在 existing-row 查询前取得 tenant advisory lock，使并发首次同 identity 请求幂等，不同 identity 仍关闭式拒绝。这保证已排队的 immutable RunSpec 稍后 resolve 同一版本时不会静默变成另一个包。

`max_steps` 中的 step 是一次“模型决策 → 可选工具调用”的主 Agent 循环，不是 token、消息或单次工具调用。`maximum_limits.max_steps = 0` 表示 operator 不设置 Host ceiling，也是官方 policy 默认值；正整数 `M` 是硬 ceiling，并会拒绝 `RunSpec.limits.max_steps = 0` 这种无限 Host 请求。Session/Agent 预设中的 `ternilo.agent.react.config.max_steps = N` 是用户请求值；有效值在两者均为正数时取 `min(N, M)`，一方为 `0` 时使用另一方，均为 `0` 时不按 step 截断。`max_tool_calls` 独立计算。

Agent 的“每轮工具调用上限”可在网页“设置 → 插件 → 插件配置”中的 `agent-loop` 配置，默认 512，`0` 表示 Agent 不按次数截断。平台另外使用 `maximum_limits.max_tool_calls` 设置宿主上限，Server 默认、官方示例和 Docker 策略均为 512；显式策略值不会自动改写。实际限制取 Agent 请求值与宿主上限中非零的较小值，两者都是 `0` 才不限制。有限的 WorkerPolicy 会拒绝 `RunSpec.limits.max_tool_calls = 0`，不能用无限请求绕过部署者的限制；WorkerPolicy 自身设为 `0` 时才允许无限 Host 请求。

当前执行位置的宿主限额随已有 `/catalog` 返回，插件页据此说明实际值；不需要另外部署配置服务。用户保存会话插件配置后，新提交任务使用新值。宿主上限仍由部署者修改 Server 策略，并重启 Server 与 Worker；Worker 在登记时重新取得策略。已经接受的排队任务保持原 RunSpec 和插件快照；若降低宿主上限，旧任务仍需由允许其原上限的 Worker 策略执行。

零值只取消相应的次数或步数截断，不移除控制面。用户从统一工作台或 API 发起停止后，取消仍会传到当前 Worker child、模型读取、支持取消的工具 future 和等待中的问题，并写入既有终态与 usage 结算链；配额、超时和其他 WorkerPolicy 继续有效。WASM 工具已支持 Store 独立的运行中取消，但 guest 中断不能强行打断宿主同步 I/O，见[扩展安全边界](security.md#extension-package)。

Server 在接受任务时解析所选模型的最终能力，Worker 只消费不含密钥的能力快照。模型入口再次验证绑定、当前授权、上下文窗口、最大输出和推理档位。自动上下文压缩默认在估算上下文达到解析后窗口的 80% 时触发，未知窗口不会改用固定 token 阈值；模型请求使用解析后的最大输出和实际推理值，平台模型与 BYOK 共用同一能力语义。

每次真实上游尝试前，Server 在独立账本保存逻辑请求 ID、attempt 和保守预算；相同请求在重开数据库后也不会自动重放，并发请求不能各自占用同一余额。仅在权威 usage 已记账后释放保守预算；中断或缺少 usage 的失败请求保留预算，避免重复失败绕过额度。最终账本使用 Provider 返回的 usage；缓存命中属于输入、推理属于输出，保存明细但不重复加总。托管请求保留 1～8 次可配置重试，公开模型 API 每个请求只调用上游一次。

## Cloud 会话遥测

Cloud Session 的遥测状态不是 Worker 内存投影。Control 在统一数据库中为每个 Session 持久保存 `disabled`、`feedback_only` 或 `full`、释放游标、导出确认游标和最近错误，统一工作台的 `GET /api/v1/sessions/{session_id}/telemetry` 直接读取这份权威状态；tenant 由已验证身份与 `x-ternilo-tenant` 选择，不放进这条资源路径。

- sharing status 按 Profile row id 的后置覆盖语义从 effective plugin rows 得出，不按原始 JSON 中偶然出现的第一行判断。
- `disabled` 不建立待发送 occurrence；切换到 disabled 时丢弃尚未领取的 pending occurrence，claim 也再次核对当前状态。已经进入 inflight 的 occurrence 已交给可信 parent，可能完成一次发送；若发送失败则删除且不再重试。
- `feedback_only` 只在 canonical feedback event 落库时释放自上次 handoff 后的新后缀；`full` 随 canonical event 建立 outbox。
- Worker parent 按 Session cursor 领取 durable occurrence，生成与 Local/Node 相同形状的 record并执行结构化遮蔽。OTLP 成功后才 ACK 并推进 `export_seq`。
- OTLP 已成功但 ACK 丢失时，租约到期或 Worker 重启会重试同一个 occurrence；每条 OTLP record 都带稳定的 `ternilo.telemetry.occurrence_id`。接收端可据此识别 at-least-once 重试，但当前不宣称第三方 collector exactly-once。
- Session 的 profile 只决定 sharing mode。Cloud 会在编译 child RunSpec 时移除 `ternilo.telemetry.otlp`；实际 endpoint、认证 header 和 service name 只由可信 Worker parent 的 operator 配置提供，不进入 child、Session event、command reply 或导出文件。

Telemetry claim／ack／fail／release 已迁入 Server 共用 Rust 事务；Worker 通过受认证接口调用，不能建立第二套数据库状态机。Outbox 长期归档与第三方 collector 的 retention 属于部署运维策略。

## 数据库权限与事务

| 数据库 | 运行边界 |
|---|---|
| SQLite | 私有持久文件；BEGIN IMMEDIATE 串行写入；Control、Cloud、Gateway 同一数据库 |
| PostgreSQL | schema-owner 负责最终 schema 初始化；非 owner runtime 无 DDL，tenant/user 事务配合 RLS |

核心变更、配额、去重、租约和审计由同一 Rust 实现完成；PostgreSQL 的跨作用域只读候选函数仅用于发现待处理记录，实际状态判断和写入仍在明确 tenant/user 事务中。不能用全局 worker 标记绕过 RLS。

Worker 只通过 Server API 访问执行与模型服务。租约代次、事件和用量幂等、恢复与凭据边界均由服务协议保证，SQLite 与 PostgreSQL 复用相同业务判断。

## HTTP API 概览

工作台资源接口要求原生或 OIDC 登录凭据，并由服务端校验空间和资源权限。健康检查、初始化、注册／登录、OIDC 回调和一次性接入流程各有专用认证规则；模型 Key、设备凭据及 Worker 凭据只能访问对应的接口，不能替代用户 Bearer。下面是接口分组概览，完整路由以 `apps/ternilo-server/src/` 为准。

### 统一工作台

`GET /api/v1/sessions/{session_id}/workspace` 返回绑定目录的浏览能力；Server 返回的 `applications` 始终为空。`POST` 的 `kind` 支持 `list` 和 `read`，使用工作区相对路径，要求工作区查看权限（不仅是会话共享）。Server 拒绝所有 `open` 请求，包括资源所有者的请求，不转发到 Node 启动桌面程序；该操作只保留在本机 Ternilo 接口。列表与读取仍发往实际 Node／Worker。当前执行器协议版本为 46，包含输入账号及凭据版本证明、逐项设置、定时来源、事件确认、原生模型思考签名、设备 Provider 用量事件和有界历史分页。队列编辑必须携带版本条件。Server、Node 与 Worker 使用同一发行版本。

`ModelRequest` 必须带当前 `run_id`；Models／ModelGateway 服务合同升至 `@3`，SessionTitles 升至 `@2`，内置与本机目录版本升至 v19。Node 模型网关同时提交实际子会话、原始输入会话和 Server 接受的输入证明，不能再按父会话最新输入推测作者。旧历史仍可读取，但没有运行绑定证明的旧远程输入不能用于新的账号模型调用，应重新提交任务；不会静默冒充其他账号或更换预算。

| 路径组 | 用途 |
|---|---|
| `/api/v1/state`、`/projects`、`/workspaces`、`/execution-targets` | 工作台基线、项目、Workspace 与 placement |
| `/api/v1/projects/{id}` | PATCH 修改项目名称，DELETE 删除无关联资源的空项目；需空间项目管理权限 |
| `/api/v1/executors/{id}/directories` | 经在线 Node 浏览/创建目录 |
| `/api/v1/sessions`、`/api/v1/sessions/{id}/events`、`/api/v1/live` | Session 生命周期、canonical 完整事件读取与实时事件/metadata 订阅 |
| `/api/v1/sessions/archived`、`/api/v1/sessions/{id}/restore` | GET 当前有权查看的归档记录，POST 恢复原会话；恢复需原归档／删除权限，不启动任务 |
| `/api/v1/sessions/{id}/archive-events` | GET 归档历史快照，要求当前 View 权限；返回事件数组与 `Cache-Control: no-store`，不启动任务或恢复会话 |
| `/api/v1/sessions/{id}/turns`、`/queue` | 提交、排队、steer 与取消 |
| `/api/v1/sessions/{id}/team`、`/team/tasks`、`/team/messages` | Agent Team roster、revision task board 与当前成员 mailbox；Local/Node 与 Cloud placement 使用各自的权威持久 store |
| `/api/v1/providers`、`/credentials`、`/authorizations` | 用户／设备 Provider、命名凭据和插件登录；平台上游另由 `/api/v1/admin/models/providers` 管理 |
| `/api/v1/catalog`、`/extensions`、`/agent-presets` | 插件能力、统一 Extension inventory 与 Agent 预设 |

`PATCH /api/v1/sessions/{id}/queue/{item_id}` 必须携带 `input` 和开始编辑时读取的 `expected_updated_at_ms`。缺少版本条件返回 400，版本过期返回 409，不覆盖现有正文、作者、附件或执行快照；已消费／移除的项不能重新创建。每次修改严格推进版本，即使系统时钟回拨也不能复用旧版本。客户端应保留失败草稿，要求显式加载最新内容再编辑，不能将刚收到的实时版本自动套在旧草稿上。Local、Node 和 Cloud 使用相同合同。

项目改名请求为 `{ "name": "项目名称" }`，名称去除首尾空白后须为 1–256 字节且不含控制字符；成功返回 `200 { "project": ProjectRecord }`，不改变项目 ID 和资源关联。删除成功返回 204，个人默认项目按持久 ID 保护，可改名但不可删除，空间最后一个项目也不可删除。存在工作区、电脑登记、注册令牌、项目密钥或模型请求历史引用时返回 409，已移除、过期或撤销的记录仍计入检查。

项目写入需要当前空间的 `ProjectManage` 权限，平台管理职责不代替空间授权。检查、变更与审计在同一事务中完成；PostgreSQL 串行化同空间删除，并在授权和锁定后启用模型服务作用域检查历史引用，不能只按普通资源可见性判断空项目。删除不递归清理文件或业务记录；显式 tenant 路由与使用 `x-ternilo-tenant` 的短路由遵守同一规则。

归档列表仅返回当前空间中仍有查看权限的记录。恢复只清除归档标记，保留原 ID、历史、作者和配置；不创建会话副本，不重新登记已移除的工作区，不恢复目标或运行任务。共享查看、提交、停止或配置权限不等于恢复权限。Node 离线时可以读取 Server 已缓存的归档元信息，但恢复需要在线。

归档历史接口在读取时确认会话仍处于归档状态；已恢复返回 409，删除或撤权后拒绝读取。Server 返回前再次确认状态和权限，Node 历史沿现有脱敏及引用转换路径读取；离线返回已同步缓存，不能保证包含未上传记录。Local 在会话生命周期锁内读取，保持与恢复／删除互斥。旧 `/archive-events` 保留完整快照语义。网页改用 `/archive-history` 的有界事件游标分页，页面每次最多显示 50 项；Cloud 归档列表最多返回 500 条记录。

`GET /api/v1/sessions/{id}/history?limit=200&before_seq=400` 读取 `seq < 400` 的最近一页，省略 `before_seq` 返回尾页；`limit` 默认 200，允许 1–1000。响应为 `{ "events": [...], "next_before_seq": 200 }`，事件按 seq 升序，`next_before_seq: null` 表示无更早记录。`/archive-history` 使用相同结构并额外核验归档状态。网页先读取尾页，再以最后原始事件的 seq 订阅 Live；加载旧页不改变实时游标。现有 `/events`、导出和 Live 协议保持兼容。

### 浏览器登录会话

`GET /api/v1/auth/sessions` 统一列出当前账号尚有效的密码登录与本站 OIDC 会话；`DELETE /api/v1/auth/sessions/{session_id}` 撤销一项；`POST /api/v1/auth/sessions/revoke-others` 保留当前本站会话并撤销同账号其他两类会话。仅持有外部 IdP 访问凭据时，当前凭据不属于本站会话，批量操作撤销本账号全部本站会话；不撤销该外部凭据或 IdP 会话。

这些接口只接受已验证的当前账号身份，不接受其他用户 ID 作为管理目标。返回的公开 ID 不是 Bearer 或数据库中的令牌摘要，也不返回可用于认证的秘密；响应禁止缓存。撤销当前会话后客户端需退出，被撤销的已有实时连接同样需要重新认证，未撤销的有效会话继续可用。浏览器登录会话与模型 Key、模型授权设备及 Node 凭据分别管理。

列表包含 `login_kind`、OIDC 的 `issuer`、首次登录时间、会话到期时间、访问凭据到期时间、当前标识、首次浏览器标识、首次／最近来源 IP 及最后活动时间。未记录的时间或浏览器信息保持空值。OIDC 会话在访问凭据到期但仍可刷新时继续列出；刷新保留同一公开 ID、首次登录及活动信息，不延长原会话期限。撤销同时使该本站会话的访问与刷新凭据失效，迟到的刷新不能重建已撤销会话。`current_session_managed` 区分当前本站会话与直接外部凭据；后者不显示为可撤销的当前本站会话。

### 工作台配置与模型

设置类接口接受可选的 `session_id`、`workspace_id` 或 `executor_id`。Session 与 Workspace 同时提供时以 Session 为准，只有 Workspace 时可在创建首个会话前配置该执行宿主；单独指定 `executor_id` 可在电脑尚无工作区时配置，但必须通过机器归属、空间权限和撤销状态检查，不能与 Session／Workspace 混用。三者均省略时使用当前空间的 Cloud 用户设置。模型中心账号页明确选择账号个人空间，不跟随聊天执行目标；托管会话的账号模型目录与凭据可用状态从 `/model-options` 按明确绑定读取。非执行所有者的安全读取只返回获准的模型目录、凭据配置状态及不可编辑的预设；全局写入要求原执行账号的配置权限，资源管理者身份不代替它。其他凭据及 Agent preset 的设置目标不随账号模型迁移。

`GET /api/v1/model-options` 返回当前选择及分页的“模型＋授权”选项。会话或工作区最多指定一个，读取该资源所有者的配置并检查协作者的使用权限；均不指定时读取本人在当前空间的默认模型和本人授权目录，可在创建工作区前配置默认模型。当前选择独立于分页返回，不因搜索结果或同名模型自动更换预算。

平台 Provider Key 轮换位于 `POST /api/v1/admin/models/providers/{provider_id}/key-rotation`；返回轮换 ID 后，通过 `/{rotation_id}/commit` 或 `/{rotation_id}/rollback` 完成操作。对应 GET 只返回待处理轮换元数据，不返回新旧 Key。普通 Provider 更新不能覆盖尚未完成的 Key 轮换。

目标是“此电脑”时，Server 先验证实际操作权限，再发给绑定 Node。共享 View 可以读取必要模型／凭据 readiness／预设元数据，但不取得 endpoint、凭据值或全局配置权限；全局插件启停／撤销要求原执行账号的配置权限。审批按 Node 返回的真实问题区分权限，不能相信客户端自报问题类型。对应配置与密钥只保存在该 Node，Control 不复制，也不会在 Node 离线时回退到同名 Cloud 配置。工作台 `/state`、Control 已保存的 Workspace/Session 索引和缓存历史与目标配置分开读取，因此 Node 离线时这些内容仍可见；只有必须抵达该 Node 的设置读取和写入单独返回 `503 unavailable`。

### Tenant 与运维

| 路径组 | 用途 |
|---|---|
| `/api/v1/me`、`/tenants` | 当前用户、membership 和 tenant 创建 |
| `/api/v1/tenants/{tenant}/members` | admin/owner 成员管理 |
| `/api/v1/tenants/{tenant}/executors`、`/enrollments` | Node 列表、吊销和一次性登记 |
| `/api/v1/tenants/{tenant}/quota`、`/model-usage`、`/audit`、`/secrets` | 配额、模型 token 用量、审计与 tenant/project secret |
| `/api/v1/tenants/{tenant}/runs`、`/sessions` | 低层 Cloud Run 与 durable Session 查询 |
| `/api/v1/tenants/{tenant}/extensions` | Tenant publisher 与统一 Extension inventory |

只有资源 owner 可用高级 `CloudRunDraft` 指定 project、Workspace、agent、Session、limits、profile 和 input，但 tenant/user/catalog/policy/priority/max-attempts 不接受客户端自报。日常网页应使用统一工作台 API。

### 模型服务与设备授权

`/api/v1/admin/models` 管理平台上游、发布、模型组、授权和用量；`/api/v1/model-access` 提供当前账号的目录、接入密钥、模型授权设备和用量。独立客户端从 `/api/v1/model-device/authorize` 取得一次性授权码，网页在 `/api/v1/model-access/device-authorization` 确认范围，再由客户端调用 `/api/v1/model-device/token` 换取模型专用凭据；`GET/DELETE /v1/model-device` 查询或断开该设备授权。

批准设备时可附带 `limits`，包含 `monthly_tokens`、`max_concurrent_requests`、`requests_per_minute`、`expires_at_ms`，均可为空；令牌额度为 1–9007199254740991，并发和每分钟请求上限均为 1–10000，到期时间为未来且不晚于 9999 年末 UTC 的绝对毫秒值（上限 253402300799999）。这些上限保证网页可无损编辑与显示。缺少 `limits` 时不加设备额外限制。`PATCH /api/v1/model-access/devices/{id}` 替换完整限制配置，字段缺失或 `null` 清除对应限制，不改变模型范围或凭据；未设频率限制时响应省略 `requests_per_minute`。频率按滚动 60 秒已接受的逻辑请求计数，跨模型来源共享，拒绝返回 429；不因取消、上游失败或编辑限制清除近期记录，幂等重复与内部重试不重复计数。

`GET /api/v1/model-access/devices/{id}/usage` 返回 `month`、`used_tokens`、`reserved_tokens`、`active_requests`，不把请求次数混入 token。管理与用量两者只允许授权账号本人访问，包括用量历史；已到期／撤销设备拒绝更新，不能恢复旧 token。

设备月度限制按 UTC 月份跨平台授权与账号自有模型聚合，同一设备并发也跨这些入口计算。上游调用前在设备锁内核验，原平台授权仍独立约束，未知消耗继续保留预留。降低上限只作用于新请求；到期与撤销则影响目录、凭据读取和活动流复核。用量结算不因设备失效而丢失。额度／并发不足为 429，到期或失效凭据为 401；公开设备状态／断开入口的认证失败同时返回 Bearer challenge。

公开 `/v1` 包含 `models`、`chat/completions`、`responses`、`messages` 和 Gemini 的 `models/{model}:generateContent`／`:streamGenerateContent`。平台设备前缀 `/v1/device/{grant_id}` 与私有 Provider 前缀 `/v1/device-account/{provider_id}` 提供相同协议路径，继续核验各自凭据和模型范围。原生协议复用用量预留、幂等、活动流撤销与结算。参数与鉴权见[模型服务](model-service.md#兼容接口与当前范围)。

托管 Worker 使用 `/internal/worker/v1/model`，受运行租约与不可变模型绑定约束；已登记 Node 使用 `/internal/node/v1/model`，受机器凭据、实际会话模型快照和 Server 已接受的输入来源约束。两者复用模型执行、每次尝试的授权检查、重试和账本，不互换凭据。普通接入密钥和独立模型设备调用每个请求只尝试上游一次，与托管调用的重试策略分别处理。

## 浏览器认证

原生会话只保存在当前标签页 sessionStorage，退出时撤销；OIDC 使用 state + S256 PKCE。绑定 OIDC 的回调先用原生 Bearer 完成显式关联，不先走普通 OIDC 登录或替换当前原生会话。本站 OIDC access/refresh token、verifier、nonce 和回调恢复路径只保存在当前标签页 `sessionStorage`；Server 验证上游 ID Token 和 UserInfo 后签发本站会话，上游 refresh token 加密保存且不返回浏览器。Token 交换只连接 issuer discovery 固定的 endpoint，响应 `no-store`。页面默认使用 self-only CSP；仅启用 Turnstile 时额外允许 `https://challenges.cloudflare.com` 的脚本和 iframe，不加载任意第三方脚本。固定前端依赖注入的滚动锁、Radix Select 视口样式及空样式初始化通过精确哈希白名单允许，不开放任意内联脚本或 `style` 标签内容。

实例所有者通过 `GET/PUT /api/v1/admin/instance/authentication` 管理登录配置。数据库保存版本化加密配置，公开响应只含密钥是否存在，审计不含密钥；更新在新请求上生效。OAuth token proxy 支持公开客户端、HTTP Basic 或请求正文中的 Client Secret。Turnstile 针对密码登录、注册和邀请注册调用固定 Siteverify 地址，核验 action、hostname 与验证结果。部署配置优先级和操作员恢复命令见[登录与人机验证](server-authentication.md)。

身份路由提供 setup、注册、登录、当前会话读取、退出、邀请接受、OIDC 显式绑定，以及[本站浏览器会话自助管理](#浏览器登录会话)。[原生密码重置](account-recovery.md)仅由本机维护 CLI 提供，没有公开重置路由；邮件找回、自助恢复和 MFA 尚未提供。退出当前会话、逐项／批量撤销其他本站会话，以及管理员封禁／注销触发的全账号撤销已实现。模型授权设备及 Node 电脑的撤销是另外两类接口，均不撤销外部 IdP 会话。

实际部署见 [Docker 与生产部署](deployment.md)，Node 使用见 [远程访问与 Node](remote-access.md)，整体威胁模型见 [安全模型](security.md)。

模型用量核对：`GET /api/v1/admin/models/requests/{request_id}/reconciliations` 读取管理核对记录；`POST /api/v1/admin/models/requests/{request_id}/attempts/{attempt}/reconcile` 由 Owner／Admin 补齐已结束尝试的缺失计数。权限、条件与请求格式见[核对指南](model-usage-reconciliation.md)。

电脑设备报告接口：`GET /api/v1/model-computers/{executor_id}/usage`，使用当前账号和明确的空间上下文，仅电脑所有者可读；共享会话查看权不包含整机用量。可选 `month=YYYY-MM`（UTC）、`query`、`cursor`、`limit`（默认 25，最大 100），返回 `source=device_reported`、`period`、`observations`、`next_cursor`。每条记录包含公开会话 ID、原运行 ID、尝试开始序号、时间、Provider／模型／协议、已验证提交者及可空计数／结束状态；不返回提示词、凭据和原始上游数据。此接口只读已同步的原始记录，不写模型请求账本。覆盖与删除语义见[设备用量](device-provider-usage.md)。

项目共享沿用 `/api/v1/projects/{project_id}/sharing` 的 `GET`、`GET /candidates`、`PUT/DELETE /{user|group}/{subject_id}`，支持明确空间头及既有目录分页。团队成员可读规则，空间管理员可修改；项目创建者身份不代替当前团队管理角色。`ResourceAccess.can_manage_sharing` 明确共享管理能力，项目继承来源的 `resource_kind` 为 `project`，`resource_name` 为项目名称。

`PUT /api/v1/workspaces/{workspace_id}/project-sharing` 接收 `{ "enabled": true }` 或 `false`，只允许有可写权限的资源所有者。返回 `project_id`、`project_name`、`enabled`、`can_change`；工作区共享查询也返回 `project_inheritance`。关闭后移除项目来源，直接共享不变。新组件为 `project_sharing` schema `1`，Control 保持 `14`，无新 Node RPC 操作。参见[项目共享](project-sharing.md)。

工作区／会话的 `/sharing/ownership` 提供 `GET` 当前管理者与版本、`GET /candidates` 有界接收者搜索、`PUT` 管理权交接。交接仅限同团队有效可写成员，请求包含 `owner_user_id`、`expected_owner_user_id`、`expected_revision` 和 `retain_previous_owner`。过期版本返回 `409 conflict`；保留原管理者时显式授予编辑协作权限，不保留共享管理、删除或再次交接权。项目通过空间角色管理，不提供此操作。

`ResourceAccess.owner_user_id` 为当前管理者，`storage_user_id` 为原存储／执行身份，`ownership_revision` 为管理版本。`is_owner` 与 `is_execution_owner` 分别判断这两种身份；全局配置仍需原执行身份及配置权限。组件 `resource_ownership` 和 `resource_ownership_live` 均为 schema `1`；执行器协议仍为 `46`。工作区详情显示当前管理者和原存储账号，内部 Node 绑定不变。参见[资源管理权交接](resource-management.md)。

### Node 清理确认

`POST /api/v1/executors/cleanup/sync` 使用 Node 凭据和本机 `storage_instance_id` 同步授权与清理请求。`GET /api/v1/executors/cleanup` 读取自身快照；`POST` 提交匹配请求、撤销版本和数据实例的回执。此通道不恢复撤销凭据的普通访问权。

`GET /api/v1/admin/accounts/{user_id}/node-cleanup` 供具有账号读取权限的实例所有者、平台管理员或审计员查看状态。`pending` 表示尚未确认，`confirmed` 表示 Node 已持久确认受管工作收尾。`detail` 仅接受 `process_state_unknown`、`process_exit_pending`、`session_busy`、`cleanup_failed` 或空值；诊断原文留在 Node 日志，不上传机器目录路径。

## 服务账号

空间管理员可创建独立服务身份，签发当前空间的读取或执行 HTTP 凭据，并进行启停与逐项撤销。接口、有效期和资源边界见[服务账号](service-accounts.md)。本站 OIDC／密码会话与服务凭据分别管理，服务身份不能取得创建者的私有资源。
