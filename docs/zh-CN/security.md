# 安全模型

Ternilo 复用内核、Workspace／Session 与 Web，但本地电脑、Server 账号和独立 Worker 仍有不同信任边界。Server 统一使用 `ternilo-server init`／`serve`，Node 角色由 `ternilo serve` 承担；单用户／多用户是同一实例的访问策略，SQLite／PostgreSQL 是独立的部署选择，不是权限档位。

原生密码使用 Argon2id；登录会话、邀请和电脑凭据只保存摘要。仅实例所有者可以切换使用模式和任命平台职责。平台管理职责与团队成员角色分别校验，不会自动获得其他用户私有资源的访问权。显式 OIDC 绑定要求有效原生会话和重新验证的 issuer＋subject，保留原 user_id；相同邮箱不构成绑定依据。

注册邮箱是尚未验证的联系信息，不构成登录凭据或 OIDC 绑定依据。本人账号页和具有账号查看权限的管理目录可见邮箱，公开协作身份仅返回用户名与账号 ID。

管理员封禁或注销账号时，在状态事务中撤销本站浏览器会话、模型 Key、模型授权设备、机器凭据及未使用的接入凭证；解除封禁不恢复这些旧凭据。Server 通知网页重新认证、断开对应机器连接，并在后续请求检查账号状态；其他 Server 实例通过持续的权限检查发现撤销。注销保留历史归属，迟到的真实用量仍可结算。外部 IdP token 的签发和撤销仍由 IdP 管理，平台会拒绝封禁及注销身份，也不会借首次登录重新创建同一身份。

团队权限组由团队拥有者或管理员管理，资源授权仍由资源所有者决定。组不能包含其他团队的成员，也不会放宽团队只读角色。用户、组和工作区授权可同时生效，撤销一项来源不影响其他独立来源。通过组分叉得到的权限受原共享和组员身份约束，不能通过分叉把组授权变为永久个人授权；所有者在子会话显式保存单独授权时才改变这一关系。

## 账号恢复、会话与设备

原生账号支持[自助改密、邮箱验证与邮件找回、验证器及恢复码](account-recovery.md)，运维人员也可以通过私有配置执行密码或 MFA 恢复。密码更新和旧本站浏览器登录撤销原子提交；登录签发复核密码和 MFA 配置代次。邮箱必须经当前账号确认才能用于恢复，邮件改密不关闭 MFA。已绑定 OIDC 的账号也须完成本站第二因素，不能以外部访问令牌绕过。仅有 OIDC 身份的账号使用 IdP 的多因素策略。通行密钥尚未提供；解除封禁、改密与 MFA 恢复分别处理账号状态和凭据。

注册准入已区分管理员账号邀请与开放注册：邀请模式不允许叠加注册审核，接受有效账号邀请后直接建立可用账号；开放注册按配置决定是否审核，并禁用账号邀请的签发、消费和页面入口。团队邀请面向已有可用账号，不绕过账号注册或审核。

原生浏览器会话可通过退出登录撤销，账号封禁或注销会撤销该账号全部本站浏览器会话。用户可在账号设置列出尚未到期／撤销的原生登录会话、逐项撤销或退出其他原生会话；公开会话 ID 不能用来登录，API 不返回令牌或持久化摘要。所有操作只作用于已认证的当前账号，撤销后在线连接被要求重新认证，其他有效会话不因此失效。标准 OIDC 网页登录签发独立本站会话，退出时撤销当前访问及刷新凭据；封禁／注销也会撤销这些凭据，解封不会恢复。原生会话列表暂不列出 OIDC 会话；外部 IdP 的 token 和组织会话仍由 IdP 管理。

凭据撤销与执行端收尾分别确认。封禁或注销账号时，Server 持久登记 Node 清理请求，按输入账号和接收版本清理该账号的排队消息、运行任务、提醒、持续目标及受管后台资源；其他账号和本机输入保留。解除封禁不恢复旧任务授权。Node 启动时先同步授权，省略远程连接参数也不会把持久的账号任务变成本机任务。

“平台管理 → 账号 → 电脑任务清理”显示等待确认、确认时间和未完成原因。离线电脑、尚未退出的受管进程或重启后无法核实的旧进程保持等待确认；仅发送取消请求不算完成。Shell、job、terminal 和外部 ACP 使用进程监督确认收尾并释放目录占用。确认覆盖 Ternilo 登记的执行资源，不回滚任务已造成的外部效果，也不等同于撤销外部服务中的操作。

已撤销的 Node 凭据只保留自身清理请求的领取和回执权限，不能用于正常连接或访问数据。凭据绑定具体本机数据实例；换目录需要独立登记，不能用空目录为原目录的工作提交确认。完成回执校验凭据、数据实例和撤销版本，重复提交幂等。

模型授权设备与远程控制电脑已有各自的列表和撤销入口：“我的模型 → 授权与接入 → 模型授权设备”撤销模型设备凭据；电脑管理撤销 Node 凭据并断开连接。它们与浏览器登录会话不同。模型设备同时支持独立到期时间、月度 token 和并发限制，在初次批准时配置，也可由授权账号本人修改；平台管理职责不授予他人的设备管理权。账号模型与多个平台预算共享同一设备限制，原平台授权的限制继续生效。

设备新请求在同一设备锁内核验跨来源的已结算用量、预留和并发；重复请求不重复占用，未知消耗不视为零。降低上限不会取消已经接受的调用；到期或撤销会使后续访问和活动流复核失败，但不取消实际用量结算。失效设备不能通过编辑重新启用旧凭据，必须重新授权；设备管理 API 不回显令牌或摘要，也不能修改模型范围和资源身份。

## 不可由插件放宽的边界

“Everything is a plugin”不包含：

- 用户和租户身份；
- RBAC、RLS 与资源 ownership；
- catalog/publisher trust；
- credential 与 master key；
- quota、WorkerPolicy ceiling，以及解析 Provider 默认值、上游声明和 exact-model 手动值后的容量／推理 allowlist；
- 操作系统 sandbox 的最终授权。

这些边界由宿主拥有。Profile 和插件可以关闭能力或请求更少权限，不能扩大宿主上限。Linorun realm 是服务路由机制，不是租户隔离。

## API Key 与凭据

普通 Provider 无论运行在本机还是云端，都可在设置页直接填写 API Key。页面不会把 key 放进 Provider JSON，而是生成稳定 credential reference，并把值写入独立 managed credential store。输入框提交后立即清空；Provider、会话、事件、导出和投影只看到引用名。

云端 Provider 与凭据按空间和用户分别保存；“我的模型”的账号 Provider 固定使用账号个人空间，团队托管执行仍使用实际团队空间的文件、租约与额度。Server 根据明确模型绑定读取配置和密钥，并实际调用模型；Worker 只转发本轮模型请求与结果，不连接数据库，也不接收实例主密钥或模型 API Key。托管模型受益人当前必须是资源所有者；Node 会话可由具有配置权限的模型持有人明确授权自己的模型。账本分别保留真实操作者、资源所有者和模型受益人，不把三者视为同一身份。无网络子进程启动时清空环境，子进程不能交换模型来源或用量归属。

独立模型服务另有平台上游命名空间及模型 Key。平台上游密文使用独立关联信息并纳入主密钥轮换；模型 Key 只保存摘要和前缀，不能用作浏览器登录、电脑登记或 Worker 凭据。每次接受请求重新检查账号模式、当前授权、组员身份、模型和 Key 范围。活动流复核撤销，已接受调用的用量写入不再依赖调用者仍有权限。

Node 的定时模型请求携带创建／派发事件引用，Server 在该租户和电脑的持久事件中核验定时器 ID、派发运行与原始已接受输入。嵌套调度只能沿已登记的子任务祖先追溯，普通分叉不能借复制的输入或任意运行 ID 获得授权。调用者仍须具有原会话提交权限，模型持有人仍须具有实际执行会话的配置权限；调度来源不构成永久授权。未解析的自动化身份不能退化为电脑所有者身份。

独立模型的普通请求历史只展示自己的公共模型、预算与标准用量，不返回内部上游模型、跟踪 ID 或原始诊断。Responses 当前不允许引用共享上游账号的既有会话、响应或文件；公开协议与用量边界见[模型接入](model-service.md)。

进程环境中同名且非空的 credential 优先，并且是只读的；网页不能覆盖或删除。凭据 inventory 只返回名称、来源和是否已配置，永不回显值。

### 高级授权流程

设置中的“高级：其他凭据与插件登录”用于插件提供的 OAuth、设备码或共享命名凭据，不是普通模型 API Key 的必要步骤。

`ternilo-authorization` 定义中立 flow：

- Flow 独占一个 credential reference 或 record key；同一 key 只能注册一个 flow。
- `AuthorizationWriter` 被限制到声明的 key；secret flow 不能写 record，record flow 不能写 secret。
- 只有 credential store 真正提交新值后，flow 才能返回 Authorized。
- 同一 key 全局只有一个在途 attempt，支持取消；不同 key 可并行。

每个浏览器标签页在 `sessionStorage` 中产生独立 `surface_id`。一个标签页启动的 prompt、notice 和取消按钮不会泄露给另一标签页；answer 必须带原 surface ID。Secret 只存在于当前 password input 和瞬时请求，提交后表单清空，snapshot、DOM 结果和会话事件不含 answer。

本地与 Node adapter API：

```text
GET  /api/v1/authorizations?surface_id=...
POST /api/v1/authorizations/begin
POST /api/v1/authorization-prompts/answer
POST /api/v1/authorizations/cancel
POST /api/v1/credentials
```

Credential set/record、authorization answer、begin 和 cancel 都只允许在线瞬时传输，不写 Server event cache、持久 command store 或 Node reply ledger。远程入口仍可在转发瞬间看到请求，因此必须部署在受控服务器并保护 TLS、日志和进程转储。

Node 的工作区绝对路径绑定保留在电脑。目录浏览、创建及相关本机元数据使用在线临时传输；发往 Server 的会话、执行结果、事件和错误副本按对应工作区根脱敏。本地规范数据及本地网页 API 保留原值。任务、队列和会话修改继续使用持久命令与去重回复，重连恢复不会因此退化为只在内存中传递。

机器所有者在工作区位置浮层中查看路径时，Server 临时向在线 Node 查询真实目录、主目录和创建时间，响应禁止缓存。关闭浮层后浏览器清除这些临时数据，Server 数据库和两端命令日志不保存路径。该查询同时要求工作区查看权限与机器所有权；共享工作区的使用权限不包含查看所有者的绝对路径。电脑离线时不会返回上次读取的目录。

删除本地 OAuth credential record 只删除 Ternilo 记录，不自动向签发方 revoke token；具体插件应把远程 revoke 作为显式流程。

## 工具 policy 与一次性审批

每个工具注册时声明 `ReadOnly`、`Mutating` 或 `Dangerous`。执行顺序：

1. Code-only presentation 与 agent PreTool Hook；
2. Host denied-tool、Plan 和 permission preset 单调检查；
3. 工具 guard；已知必然失败的调用不会询问用户；
4. Dangerous 调用写入 `user_question_asked` 和参数快照；
5. 只有精确“允许一次”才进入 handler；
6. 决定先写 `user_question_answered`，随后执行 handler 和 PostTool Hook。

拒绝、取消、传输失败或无 answerer 都会关闭式拒绝。没有“永久允许”。Code Mode 嵌套调用、MCP/Extension 和远程操作遵守同一流水线。Cloud 也保存真实 pending question，等待具备相应权限的用户回答；无交互执行不能自动批准。共享回答由 Server 按后台问题分类：普通回复 Submit、计划／挂载 Configure、停止类 Stop、机器级插件管理 owner-only。

## 本地进程 Sandbox

Shell、后台 job 和持久 terminal 共用可信 `ternilo/sandbox@1` service。普通用户插件与 tenant Extension 不能提供或放宽它；缺少 backend 时不会退回裸进程。

| 权限/请求 | 执行方式 |
|---|---|
| `ReadOnly` | 平台 sandbox；工作区和宿主文件只读 |
| `WorkspaceWrite` | 平台 sandbox；工作区可写 |
| `FullAccess` | 用户明确选择的宿主全访问 |
| 单次 shell `full_access: true` | 先经危险审批；仅该调用绕过 sandbox |

平台实现：

| 平台 | Backend | 边界 |
|---|---|---|
| Linux | bubblewrap | 只读 `/`、私有 PID/`/proc`/`/dev`；写模式额外提供私有 `/tmp` 和工作区 bind |
| macOS | Seatbelt `sandbox-exec` | 默认拒绝全局写；写模式只放行工作区与系统临时目录 |
| Windows | `ternilo-sandbox-windows.exe` | Restricted token、capability-SID workspace ACL、Job Object 进程树回收；当前不宣称完整读取隔离 |

本地文件 sandbox 不隐式关闭网络：Linux 不创建 network namespace，macOS profile 不限制网络，Windows runner 标记 network allowed。需要限制出网时应在主机/容器层增加策略。云端 worker 使用更强的无网络边界。

Shell/job/terminal 启动前会清理凭据、宿主配置、沙箱实现和模型供应商相关的敏感环境变量。MCP/LSP/ACP 子进程同样先清空环境，只加入基础变量与配置显式允许的值。

Windows wrapper 的稳定 argv：

```text
ternilo-sandbox-windows.exe --workspace <absolute-path> --mode <read-only|workspace-write> -- <program> <args...>
```

它复用 MIT `zagens-windows-sandbox` 的 Windows API，但 Ternilo 自己拥有协议、环境清理和 capability seam。归属见 [`THIRD_PARTY_NOTICES.md`](../../THIRD_PARTY_NOTICES.md)。

## 云端隔离

Worker 使用独立凭据主动连接 Server。Server 每次处理请求都检查凭据、当前进程身份和任务租约，任务内容与资源所有者从权威数据库读取，不接受 Worker 自报的身份或完整执行配置。撤销凭据或更换进程后，旧连接不能继续写入任务结果。

可信父进程与每次运行的子进程分离；子进程：

- 在独立 bubblewrap mount/PID/network/IPC/UTS/cgroup namespace 中运行；
- 默认没有网络；
- runtime/policy/envelope 只读，`/tmp` 私有；
- 没有数据库 URL、provider key 或宿主环境；
- 只通过有界 NDJSON capability protocol 请求模型；
- 模型请求由父进程转发，Server 按已保存的运行配置检查模型、权限与预算。

Server 的资源授权、事务、任务租约和会话写入版本检查与操作系统隔离共同生效；PostgreSQL 另外启用行级安全策略。仅携带一个空间 ID 不构成授权。模型请求在调用前保存请求编号和预算，连接中断后不会盲目再次调用；用量以 Server 保存的模型返回结果为准。完整执行链见[平台与云端执行](server-reference.md)。

同一空间的工作区绑定一个持久存储位置。多个 Worker 只有使用同一真实存储卷时才能服务该位置；空目录不能冒用原卷，离线时也不会改派到空盘。数据库拒绝旧进程提交结果，并不能撤销该进程已经写入磁盘的文件；迁移或联合备份前必须停止相关 Worker。

保留的 Docker worker 隔离模式使用 `--sandbox container`；Linux 宿主部署使用 `--sandbox bubblewrap`。`--sandbox process` 没有多租户 OS 隔离，只能用于开发。

## Extension Package

第三方能力使用签名 Extension Package，而不是 tenant Rust dylib。每个 package version 必须且只能选择受限 Rhai 源码或 WebAssembly Component。当前 manifest 可贡献 Tool、静态 Prompt section、静态 Skill、Hook、Command 和声明式 Provider template。Prompt/Skill 的静态内容不执行 runtime；Hook 与 Tool 使用统一受限运行时，Command 映射同包 Tool 并经过原有审批链，Provider template 创建受现有 Provider 验证规则约束的普通模型配置。两种 runtime 都只暴露 manifest 申请且安装者明确授予的 `log` 与 `workspace_read` capability；Rhai 受 operations、墙钟、结构和 I/O 上限约束，WASM Host 不链接 WASI，并限制 fuel、内存和 I/O。

Rhai 工具通过执行进度回调检查取消及墙钟时限；WASM 工具为每次调用建立取消监视，通过 epoch 检查中断被取消的 Store，同一组件的其他并发调用不受影响。监视线程随调用退出回收；fuel 是独立的计算量上限，不代替用户停止。宿主同步文件系统调用不能由 guest epoch 强制中断，Hook 也没有 Tool 的运行取消上下文，所以仍不能承诺所有扩展操作立即结束或回滚。实现见 `crates/ternilo-extension/src/runtime.rs` 与 `crates/ternilo-extension/src/runtime/wasm_cancellation.rs`。

Publisher trust 绑定 key ID、公钥和精确 source；package/version 不可变，撤销不可恢复。任一 Extension Tool handler 执行前仍经过 Plan、permission 和工具副作用检查。Prompt、Skill provider、Hook、Tool 与 Command 属于同一 activation，失败回滚、停用、撤销、卸载或解除挂载都按逆序清理。模板物化后的普通 Provider 属于用户数据，不依赖扩展运行时，也不随扩展卸载删除。模型只能经用户批准管理已安装版本，不能新增 publisher、安装 bundle、卸载 package 或扩大 grant。细节见 [扩展系统](extension-packages.md#外部-extension-package)。

## Web、PWA 与远程入口

- 本地 Salvo 服务只监听 loopback，并通过 boot 脚本注入随机 API token；同一数据目录由一个服务持有 writer lock。
- `service.json` 包含实例信息和本地 API token，Unix 创建权限为 `0600`。客户端不能只信任文件里的端口：先检查 loopback，再携带 token 请求服务身份并比对；桌面还校验版本与网页是否启用。
- `ternilo status` 只输出不含 token 的服务信息；`stop` 通过认证接口请求关闭，并等待应用收尾与锁释放。桌面是客户端，关闭窗口不停止服务。
- `--no-local-web` 不开放网页资源或桌面 WebView，但仍提供认证后的应用和管理 API，不等于不监听任何端口。
- Server Web 支持原生账号和标准 OIDC Code + PKCE；本站 token／verifier／nonce 只在 sessionStorage，身份响应 no-store，上游刷新凭据加密保存在 Server。ID Token 和 UserInfo 严格核验同一身份；显式 OIDC 绑定使用当前原生凭据完成关联，不建立第二个 owner。
- Control enrollment token 短期有效且只能消费一次。换取的 Node credential 只在 Server 数据库保存哈希，没有时间到期或刷新接口；它在电脑被显式吊销后立即失效，重新签发必须先吊销并重新登记。
- 单用户模式暂停其他账号的远程访问；模式切换或撤销共享后，HTTP 和实时连接重新检查权限。资源、归属和已开始任务保留。
- Service worker 只缓存公开 shell；不拦截导航、`/api/`、boot token 或会话数据。
- 富文本经过协议 allowlist 与 DOMPurify；原始 HTML 不执行，链接/图片 scheme受限，CSP 禁止任意第三方脚本。仅启用 Turnstile 时，额外允许 Cloudflare challenges 的脚本及 iframe；验证必须经过服务端 Siteverify、域名和 action 校验。配置及恢复见[登录与人机验证](server-authentication.md)。
- Ternilo 本身只提供 HTTP upstream，不内置 HTTPS/TLS、ACME、证书或续签。公网 Server 必须经过部署方的外部 gateway 或负载均衡器，由该边界负责 DNS、TLS、HSTS、MIME sniffing、referrer、body limit、日志 retention 和必要的 rate limit。
- 同机部署时 Compose 默认只把 Server upstream 发布到 `127.0.0.1` 的可配置端口。跨机 gateway 必须显式使用受控私网绑定，并以安全组、防火墙或等价网络 ACL 限制来源；不能直接暴露明文 upstream。
- 外部 gateway 必须支持流式响应且不缓冲增量输出，并保留 `/api/v1/live`和 `/api/v1/executors/connect` 的 WebSocket Upgrade。生产 `public_url` 仍是外部 HTTPS origin，浏览器据此使用 WSS。

PWA 提供安装入口，不提供离线 agent。无法到达 Server／Node 时不能执行或伪造任务。

## 存储完整性与大输出

本地 append-only session log 是唯一事实来源，本地 SQLite projection 可删除重建；Server 的 SQLite／PostgreSQL 则是账号、资源、租约和 Cloud 事件的权威持久数据库，不能当作缓存删除。附件使用 SHA-256 内容寻址、原子 rename、目录同步和读取时验 digest。大工具输出先成功写对象，再提交带 locator 的事件；对象失败时不会假装事件成功。文件交付物保存当次内容快照，而不是可漂移路径。

本地凭据文件使用私有权限。备份前使用 `ternilo stop --data-dir` 正常停止后台服务并确认成功，再停止占用该目录的独立 RPC/ACP 进程，备份整个数据目录和需要保留的项目文件；仅关闭浏览器或桌面窗口不够。

Server 的完整备份保存持久数据卷以及配置／主密钥，PostgreSQL 还需对应数据库备份。需要单独在线备份 SQLite 时，使用 `ternilo-server admin backup-sqlite` 生成一致快照，不能复制运行中数据库的主文件冒充备份。包含 Worker 文件的联合备份则须先暂停领取、等活动运行与命令结束、停止各 Worker，再备份所有存储卷和 Server；单份数据库快照不保证与远程工作文件处于同一时刻。流程见[部署文档](deployment.md#备份恢复升级与轮换)。

共享附件另验证引用属于该会话的类型化事件或已有队列；普通文本中出现 digest 不构成授权。接收引用的提交在写入新队列和预留配额前检查，避免新请求为自己制造引用。owner 保留原对象复用能力，新的 inline 上传正常接受。

## 会话遥测

OTLP session telemetry 默认 `disabled`。`feedback_only` 在用户主动提交反馈前不发送会话内容；反馈落入权威日志后，它复制自上次释放游标之后、截至该反馈事件的 canonical session-log 后缀，之后的新事件继续留在本地，直到下一次反馈。`full` 实时复制 session ledger。两种发送模式都会经过结构化 redactor，并对同一 Session/run/step 的流式 delta 只取首条以避免逐 token 洪泛。

Cloud 的 handoff/export cursor 与 outbox occurrence 持久化在统一 Server 数据库。Worker parent 在 OTLP flush 成功后 ACK；Worker 或 Control 重启不会把整段历史重新当作新批次。第三方接收成功而数据库 ACK 丢失时允许以相同 `ternilo.telemetry.occurrence_id` 重试，因此当前保证 Ternilo durable outbox 不漏，接收端语义为可识别的 at-least-once。Cloud child 的 Profile 会移除网络 exporter，且 child 进程清空父进程环境并使用独立的无网络 namespace；OTLP endpoint/header secret只存在于可信 parent。

用户切换为 `disabled` 后，平台删除尚未被 Worker 领取的 pending occurrence，并在 claim 时核对最新 sharing status。已经进入 inflight 的 occurrence 已越过 parent handoff 边界，可能完成一次发送；若接收端失败则直接删除、不再重试。Profile 中重复 row id 按与 Harness composition 相同的后置覆盖规则解析 sharing，不会让被覆盖的旧 telemetry row 继续生效。

Redactor 会遮蔽 authorization、API key、token、password、credential、client secret 和 cookie 等结构化字段，但不是内容级 DLP。`feedback_only` 释放的后缀与 `full` 都可能包含 prompt、工具参数、结果和模型输出，部署者必须明确 opt in，并配置 collector 的数据驻留、访问控制和 retention。Exporter 排队或失败不会使 turn 失败。

## 依赖与许可证门禁

- `cargo audit` 使用更新后的 RustSec 数据库；`cargo deny` 检查 advisory 处置、许可证、来源和重复版本。
- OIDC JWT 选择 `jsonwebtoken` 的 AWS-LC provider，不启用会引入 RustCrypto `rsa` 的 provider；共享密钥 HS256/384/512 被拒绝。
- `deny.toml` 中每个 advisory 例外都必须有具体理由，不能按 crate 名隐藏整棵依赖树。
- Tauri Linux backend 与 Rhai 当前锁定依赖中的上游维护/unsound warning 保持可见，升级上游后应删除不再命中的例外。
- 允许的 SPDX 许可证和第三方归属分别维护在 `deny.toml` 与 `THIRD_PARTY_NOTICES.md`。

发布时必须联网刷新审计数据；离线 `cargo audit --no-fetch` 只能用于临时构建。完整门禁见 [开发与发布](contributing.md)。
