# 扩展包开发

本文面向扩展作者。日常插件、MCP、LSP 和脚本配置见[扩展系统](extensions.md)。

## 外部 Extension Package

Ternilo 只有一套外部扩展包契约。manifest、发布者信任、签名、安装、版本、启停、永久撤销、卸载、Profile 挂载、配置 Schema、能力授权和贡献声明都由同一个 Extension Registry 管理。Rhai 与 WASM Component 只是这个契约下的两个执行后端，不各自建立 inventory、市场或生命周期。

### Manifest 与单选 runtime

每个 package version 的 manifest 必须声明且只能声明一个 `runtime`：

- `{"kind":"rhai","limits":{...}}`：payload 是 UTF-8 Rhai 源码；
- `{"kind":"wasm-component","world":"ternilo:extension/runtime@1.0.0","limits":{...}}`：payload 是 base64 编码的 WebAssembly Component。

当前且唯一的 package manifest 合同使用 `schema_version: 1`，签名 domain 为 `TERNILO-EXTENSION-PACKAGE-V1\0`。六类 contribution 都直接同属该首发合同。同一个包版本不能同时携带两种 payload。签名覆盖完整 manifest，其中包含 package/version、精确 source、publisher key ID、payload SHA-256、唯一 runtime、配置 Schema、贡献、请求能力和资源上限。

签名与验签使用同一份 v1 canonical manifest：递归按 UTF-8 key 字典序排列每个 JSON object，并把 JSON Number 的负零归一化为浮点正零；整数零和所有非零数值保持原类型/值，Unicode 字符串不做规范化，数组顺序保持不变。这样 bundle 经 PostgreSQL JSONB 重排对象键或把 `-0.0` 表示成 `0.0` 后仍能验签，同时数组调换或 NFC/NFD 文本变化仍被视为 manifest 变化。

当前外部贡献包括 Tool、静态有序 Prompt section、静态 Skill、Hook、Command 和声明式 Provider template：

- Tool 声明 handler、输入/输出 JSON Schema、effect 和声明式 Web presentation；运行后端不会改变 Tool 进入共享 Plan、permission、Hook、审批与事件流水线的方式。`output_schema` 是 handler 原始 JSON 返回值的签名合同，不只是展示提示：Rhai 与 WASM Component 都先验证原始 JSON，再将其统一渲染为 canonical `ToolOutput`。
- Prompt section 声明 package 内唯一的 `id`、整数 `order` 和非空静态 `content`。它不执行 Rhai/WASM payload，也不做变量插值；包挂载后进入同一个 `Prompts` service，并参与每次模型请求的 system prompt 组装。
- Skill 声明 package 内唯一的 `name`、`description`、可选 `when_to_use`、`invocation` 和完整静态 `content`。它进入共享 `Skills` service，可由模型目录或 `/skill` 用户入口发现和加载；加载静态内容不会执行 Rhai/WASM payload。
- Hook 声明 package 内唯一的 `id`、Hook point、runtime handler 和 `all` 或 `tool_names` matcher。挂载后进入共享 `Hooks` service，Rhai/WASM guest 只能返回 decision/reason/stop/context/message；物理 handler ID、dialect、point、duration、exit code 和错误摘要始终由宿主生成。runtime、超时或返回值解码失败会形成可持久化的 neutral Hook 结果，不会暗设另一套 fail-closed 规则。
- Command 声明 package 内唯一的命令名，并映射到同包已经贡献的 Tool。它不直接执行 Rhai/WASM handler；共享 `Commands` service 只把签名 fixed arguments 与可选字符串后缀解析成目标 Tool 调用，实际执行仍进入同一 Schema、Hook、权限、审批、事件与取消链。
- Provider template 声明普通 `ProviderProfile` 的 endpoint、协议、模型、默认值和重试参数；协议使用共享 `ProviderProtocol`，当前包括 `openai-chat-completions`、`openai-responses`、`deepseek-responses`、`google-gemini` 和 `anthropic-messages`。它不执行扩展 runtime；用户只能选择最终 Provider ID 和 credential ref，宿主从当前目标的已安装、启用、可信且未撤销 inventory 复制其余签名字段。

Manifest 的 `contributions` 必须显式包含 `tools`、`prompt_sections`、`skills`、`hooks`、`commands` 和 `providers` 六个数组；每个数组可以为空，但六者合计至少要有一项贡献。缺失任意数组都不是合法 package v1 wire，宿主不会把旧形状隐式补成空数组。

一个包可以只贡献 Prompt section、Skill 或 Hook，但仍必须选择 `rhai` 或 `wasm-component` runtime、携带对应唯一 payload，并通过相同的摘要、签名与编译验证。任意 Web 组件不因存在统一 contributions 容器而自动获得支持。

统一 host capability 目前包括：

- `log`：写入受限日志，单条最多 4 KiB；
- `workspace_read`：只读当前会话工作区内的相对 UTF-8 普通文件，并受累计字节上限约束。

Manifest 的 `requested_capabilities` 只是申请。实际 `granted_capabilities` 必须同时属于申请集合和宿主 policy allowlist。两种 runtime 都不能直接取得网络、环境变量、credential 或子进程；底层宿主能力继续由 Rust 实现和授权。

### 静态 Prompt section

Manifest 中的 Prompt contribution 形如：

```json
{
  "prompt_sections": [
    {
      "id": "usage-guidance",
      "order": 450,
      "content": "Use this extension for deterministic workspace checks."
    }
  ]
}
```

`id` 只需在当前 package 内唯一。挂载时宿主将其转换为 `extension:{package_id}@{version}:{id}`，因此不同 package 可以安全使用相同本地 ID，也不会与内置 Prompt section 混淆。同一 `order` 下继续按物理 ID 稳定排序。完整 `content` 属于已签名 manifest，安装审查与已安装卡片都会显示它，不能由未签名 payload 在运行时替换。

Prompt、Skill provider、Hook、Tool 与 Command 属于同一次插件激活，注册顺序固定为 Prompt → Skill provider → Hook → Tool → Command，清理和回滚按 Command → Tool → Hook → Skill provider → Prompt 逆序执行。任何阶段失败都会撤销本次已经完成的全部注册；停用、撤销、卸载、Profile 解除挂载或 Session runtime 重启时同样清理五类贡献。历史请求已经持久化的 system prompt、Hook result 或命令事件不会被回写修改。

### 静态 Skill

Manifest 中的 Skill contribution 形如：

```json
{
  "skills": [
    {
      "name": "workspace-review",
      "description": "Review a workspace with the installed extension tools.",
      "when_to_use": "When the user requests a structured workspace review.",
      "invocation": {
        "model_invocable": true,
        "user_invocable": true
      },
      "content": "# Workspace review\n\nInspect the requested files, then report findings."
    }
  ]
}
```

Skill `name` 使用 1–128 字节的小写 kebab-case，并在当前 package 内唯一；单段 `content` 最大 2 MiB。每个 package version 注册一个由 package/version 派生、包含确定性哈希的物理 namespaced provider，`source` 明确显示 `extension:{package_id}@{version}`，便于用户审查来源。公开 Skill name 不增加 namespace：同名候选继续按共享 Skills service 的 `(rank, Profile mount 顺序, 同 caller 注册顺序, local_order)` 选择。Extension provider 固定 rank 250，因此 Workspace 中 rank 100/200 的 Skill 优先；不同 Extension provider 之间由 Profile 顺序稳定决定，不受并发 activation 的完成先后影响。

静态 Skill 的 `resource_base` 固定为 `None`，不会隐式开放相对资源目录。完整 description、routing hint、invocation policy 和 content 都属于签名 manifest，并在安装审查与已安装卡片中显示。Skill-only package 仍必须选择一个 runtime 并携带对应 payload，但读取 Skill 绝不执行该 runtime。

### Command

Command contribution 只描述一个同包 Tool 的人类快捷入口：

```json
{
  "name": "review",
  "description": "Review the current workspace",
  "tool": "workspace_review",
  "input": {
    "hint": "<request>",
    "field": "request",
    "images": false
  },
  "fixed_arguments": {
    "mode": "full"
  }
}
```

`fixed_arguments` 必须是 object。存在 `input` 时，解析后的命令后缀写入所声明的顶层字符串 field；没有 `input` 时只接受空后缀。首版不支持图片或任意 JSON 命令 DSL。合成参数最终由目标 Tool 的签名 `input_schema` 校验，所以错误不会进入 handler。

命令目标必须是同一 package 已贡献的 Tool。package 内重复名、`feedback/plan/skill` typed route 保留名在 manifest 校验时拒绝；多个有效 Extension 的同名命令在 Local Registry 或 Control distribution preflight 拒绝；与 builtin 命令冲突会在 composed Session boot 注册同一个 `Commands` service 时明确失败。命令解析后仍走 canonical direct Tool 流水线，不会绕过 Pre/PostTool Hook、Plan/permission/guard、危险操作审批、durable event 或 cancellation。

### Provider template

Provider template 是已签名 manifest 中的声明式普通 Provider 蓝图。物化请求的 JSON 只包含：

```json
{
  "package_id": "dev.example.models",
  "version": "1.0.0",
  "template": "primary",
  "provider_id": "example-models",
  "api_key_ref": "EXAMPLE_API_KEY"
}
```

Local 与 Server 工作台的创建入口都是 `POST /api/v1/providers/from-extension`。目标为 Node 时，Server 通过 Node Gateway 转发 typed operation，由 Node 使用自己的 Extension inventory 物化；目标为 Cloud 时，由 Server 的 Control 使用当前 tenant inventory 物化。两条路径都不会接受客户端提交 `base_url/protocol/models/defaults/timeout/retry`，并最终执行 `ProviderProfile::validate`。

物化写入是 create-only：若最终 `provider_id` 已存在则返回冲突，不会复用普通 Provider 编辑接口的 upsert 语义。模板声明 `credential.required=true` 时，调用方必须显式给出非空且合法的 `api_key_ref`；`suggested_ref` 只供界面预填，后端不会自动采用。成功物化的结果是没有 Extension 外键的普通用户 Provider，之后停用、撤销或卸载来源 Extension 都不会删除或破坏它，模型请求继续走现有可信宿主的 credential、HTTP、retry 和 usage 路径。

本地离线 CLI 使用同一权威实现：

```bash
ternilo provider add-from-extension \
  --package-id dev.example.models \
  --version 1.0.0 \
  --template primary \
  --provider-id example-models \
  --api-key-ref EXAMPLE_API_KEY
```

各参数也有对应环境变量；CLI 参数优先于环境变量。由于本地数据目录有单写锁，直接操作同一 data directory 前应先停止正在使用它的 Local/Node 进程；运行中的实例应改用 HTTP 入口。

### Rhai 与 WASM Component

Rhai 适合源码透明、便于审查和快速修改的扩展。安装时会编译并缓存 AST，每次调用使用新的 Engine 和 Scope；公开 handler 的固定签名是 `fn handler(context, input, settings)`。`context` 只暴露安全会话标识；Tool 的 `input` 是 arguments，Hook 的 `input` 是 `HookRequest`，`settings` 来自当前 Profile mount。脚本不能使用 `import` 或 `eval`，并受 operations、墙钟、字符串、集合、递归、变量、函数、输入、输出和工作区读取上限约束。

WASM Component 适合跨语言、复杂解析或高性能计算。当前使用通用 `ternilo:extension/runtime@1.0.0` world，固定导出 `invoke(handler, context-json, input-json)`；同一包的 Tool 与 Hook 都通过 manifest handler 在 guest 内分派，挂载 `settings` 放在 `context-json` 中。每次调用创建新的 Wasmtime store，并限制 fuel、线性内存、输入、输出和工作区读取。Host 不链接 WASI，也不提供网络、环境变量、时钟、随机数或子进程。

两个 runtime 的 Tool 返回值遵循同一顺序：先把 handler 结果解码为 JSON，再以签名 `output_schema` 校验；校验通过后，JSON string 变成普通成功文本，精确 `{ "content": string, "is_error": boolean }` 对象保留显式错误位，其他 JSON 序列化为文本。资源上限与 Schema 任一失败都不会产生伪造的成功 `ToolOutput`。Hook 不使用 Tool 的 `output_schema`，而是解码为宿主固定的 Hook 返回结构。

Tool 调用前两种 runtime 都检查取消状态。Rhai 还在执行进度回调中检查取消和墙钟上限；WASM 工具通过每次调用的取消监视触发 epoch 检查，仅中断被取消的 Store，不影响同一编译组件的其他调用。调用退出时停止并回收监视线程，fuel 和内存上限继续独立生效。epoch 只中断 guest 执行，不能强行打断宿主正在阻塞的同步文件系统调用；Hook 没有 Tool 的运行取消上下文，仍按其资源与生命周期合同执行。

Code Mode 仍是一次性的模型编排能力：它把当前会话已经挂载的工具投影成动态 Rhai SDK，但不参加 Extension Package 的安装、版本或分发生命周期。

### 包、发布者与生命周期

`SignedExtensionBundle` 包含统一 manifest、runtime 对应的唯一 payload 和 Ed25519 signature。Publisher trust 把不可复用的 `key_id`、公钥与一个或多个精确 source 绑定；package ID + semver version 也不可变。安装时 Registry 会验证 source、签名、摘要、能力和宿主资源上限，并对 Rhai 或 WASM payload 执行真实编译验证。package schema v1 决定 manifest 与签名消息格式；另一个独立的 inventory schema v1 为首次安装永久保留完整 manifest、signature 和 granted capabilities，作为这个 `package_id@version` 的 identity。CLI 私钥文件的 `PublisherKeyFile.schema_version: 1` 也是独立格式；三者同为 v1 不表示它们共用一个 wire。

生命周期动作具有不同语义：

- 启用/停用只改变该版本当前是否可解析和调用，可以再次切换；
- `POST .../revoke` 永久撤销精确 package version 或 publisher，撤销后不能重新启用或以同一身份重装；
- `DELETE` package version 表示卸载，删除安装记录，并在没有其他版本引用同一 digest 时删除 artifact；它不删除永久 version identity，已撤销版本保留为永久记录并拒绝卸载；
- 卸载后可重装完全相同的 signed bundle 和 capability grants；换 manifest、payload/signature 或 grants 必须发布新版本，不能复用原 `package_id@version`；
- Session mount 只引用 package/version/settings，不复制 payload，也不决定目标 inventory 的信任与授权。

package 与 inventory 都直接以上述 v1 作为唯一现行格式。

本地 inventory 位于 data directory 的 `extensions/`，artifact 按 digest 存储并在重启时重新验证。云端 inventory 是 tenant-scoped RLS 数据；Control 在安装时使用同一个 Registry 验证，Worker 只接收 RunSpec 精确引用的已签名 distribution，并在隔离 child 启动前重新验证和注册。

### 构建、签名和离线验证

可直接审查的多 Tool + 静态 Prompt + 静态 Skill Rhai 示例位于 [`examples/rhai-echo-extension`](../../examples/rhai-echo-extension)，交付包也包含此目录。`local` 和 `all` 交付包包含 `ternilo-plugin`，使用时将包内 `bin/` 加入 `PATH`，并从交付包根目录执行；`server` 和 `worker` 包需另行安装该 CLI。从源码开发时，在仓库根目录将下列 `ternilo-plugin` 替换为 `cargo run --locked -p ternilo-plugin-cli --`：

```bash
ternilo-plugin keygen \
  --key-id example-publisher \
  --output /tmp/ternilo-extension-key.json

ternilo-plugin publisher \
  --key /tmp/ternilo-extension-key.json \
  --source https://plugins.ternilo.dev/examples \
  --output /tmp/ternilo-extension-publisher.json

ternilo-plugin sign \
  --manifest examples/rhai-echo-extension/manifest.json \
  --payload examples/rhai-echo-extension/extension.rhai \
  --key /tmp/ternilo-extension-key.json \
  --output /tmp/ternilo-rhai-example.bundle.json

ternilo-plugin verify \
  --bundle /tmp/ternilo-rhai-example.bundle.json \
  --publisher /tmp/ternilo-extension-publisher.json
```

`sign --payload` 根据 manifest 的 `runtime.kind` 把文件编码成 UTF-8 Rhai payload 或 base64 WASM payload，并计算真实摘要。WASM 开发者可先用 CLI 的 `componentize --module ... --output ...` 生成 Component，再把该输出传给相同的 `sign --payload`。`verify` 会通过临时 Extension Registry 执行与安装时相同的签名、policy 和 runtime 编译验证。

CLI 读取独立 manifest、publisher 和私钥 JSON 文件时使用 4 MiB 上限，以容纳一段合法的 2 MiB Skill content 及其余签名字段；runtime payload 使用独立的 16 MiB 上限。读取完整 bundle JSON 时，上限为 payload 上限的两倍加 4 MiB，即当前默认的 36 MiB，以容纳编码后的 payload 和 manifest。

最小 WASM guest 位于 [`examples/wasm-echo-plugin`](../../examples/wasm-echo-plugin)。目录名说明示例用途；其 crate/package 名是 `ternilo-extension-wasm-echo`，Rust guest 构建产物是 `ternilo_extension_wasm_echo.wasm`。示例实现两个 handler，证明同一 Component 可按宿主传入的 handler identity 分派多个 Tool；manifest 还声明静态 Prompt 与 Skill，证明两类静态 contribution 都不依赖所选 runtime。

`keygen` 拒绝覆盖并在 Unix 以 `0600` 创建私钥。私钥只能用于离线签名，不能上传到 Ternilo；Web/API 只接收 public publisher JSON 和 signed bundle。

### API 与 Profile 挂载

本地 Web 与 Server 工作台使用以下路由；Server 根据配置目标在 Node Gateway 转发与 Cloud inventory 之间选择：

| 操作 | 路径 |
|---|---|
| Inventory | `GET /api/v1/extensions` |
| 信任 publisher | `POST /api/v1/extensions/publishers` |
| 安装 bundle | `POST /api/v1/extensions` |
| 启停 | `PUT /api/v1/extensions/{package}/{version}` |
| 永久撤销版本 | `POST /api/v1/extensions/{package}/{version}/revoke` |
| 卸载版本 | `DELETE /api/v1/extensions/{package}/{version}` |
| 永久撤销 publisher | `POST /api/v1/extensions/publishers/{key}/revoke` |

Control 多租户 API 在路径中增加 `/tenants/{tenant}`，并由 admin/owner、RLS 与 audit 约束。安装请求包含统一 signed bundle 和显式 `granted_capabilities`。会话自身的插件组合读取接口继续保留 `GET /api/v1/sessions/{session_id}/plugins`；Profile 更新通过 Session mutation 完成。它管理 Session Profile，不是目标级 Extension inventory。

经 Server Node Gateway 转发到 Node 时，inventory、publisher trust/revoke、安装、启停、版本撤销、卸载和 Provider template 物化都要求 Node hello 声明 `extension_management` capability。Server 在创建 pending command 或发送 wire command 前检查；旧 Node 会收到明确的 unavailable/升级提示，不会遗留无法执行的命令。

Profile 使用通用 mount，与 runtime 后端无关：

```json
{
  "id": "extension:dev.ternilo.rhai-echo@1.0.0",
  "kind": "ternilo.extension.package",
  "enabled": true,
  "config": {
    "package_id": "dev.ternilo.rhai-echo",
    "version": "1.0.0",
    "settings": {
      "prefix": "Rhai: ",
      "label": "inspection"
    }
  }
}
```

Manifest 的 `config_schema` 会在安装时由 `jsonschema` 编译校验；mount 的 `settings` 是必填 object，写入和每次组合 Session mount 时都会再次用这份已签名 Schema 验证。缺失 `settings`、缺失 Schema 必填字段、类型或 pattern 错误都会拒绝变更/构建插件实例，不能依赖或绕过 Web 表单。Local 与 Control/Cloud 共用 `validate_extension_settings`，不会各自实现一套 Schema 语义。

挂载只接受当前 enabled、未 revoked 且 publisher 仍可信的版本。Registry 在每次调用前重新 resolve 状态，因此停用和撤销立即生效；挂载变化会在当前 turn 完成后重启 Session runtime。卸载或撤销会清理受影响的 Session 与用户预设挂载。

## 运行时扩展管理

Agent 只能管理宿主已经信任、安装并授权的 Extension version：

| 工具 | 行为 |
|---|---|
| `extension_inspect` | 查看 catalog revision、当前工具 Schema、publisher、runtime 和 inventory |
| `extension_set_enabled` | 经审批启停已安装版本 |
| `extension_set_mounted` | 经审批更新当前会话的通用 Extension row；必须显式传 object `settings`，挂载前按签名 `config_schema` 校验，下轮重启 runtime |
| `extension_revoke` | 经明确审批永久撤销精确版本 |

Publisher trust、bundle 安装、卸载和 capability grant 不能由模型创建。目标为 Node 时，Server Node Gateway 将审批与修改转发到用户电脑，不保存私钥或另建该 Node 的 inventory。Cloud Worker 拒绝 child 侧 mutable extension service；多租户 inventory 和 run profile 只能由 Control RBAC、RLS、audit 与 scheduler 编译修改。正式 Docker WorkerPolicy 当前允许 `ternilo.extension.package`，每轮最多 4 个由 Control 已验签并精确分发的包，并只授予 policy 允许的 `log` / `workspace_read`；“可以执行分发包”不等于“可以从 Worker 修改 inventory”。Cloud Session create/PATCH/submit 与用户 Agent preset PUT 会在持久化前解析已验签 distribution，校验每个 enabled mount settings，并拒绝同 package/version 重复 mount 与跨包同名 Tool/Command；Worker `compile_draft` 再做运行边界复核。Cloud 的 append-only install audit 同样固定 version identity，使已排队的 immutable RunSpec 不会在 Worker resolve 时静默换包。Cloud 示例使用 v5 `ExecutionEnvelope` 的顶层 `workspace` 与 `extensions` wire，portable RunSpec 示例也已更新为 v5 当前字段。

Cloud install 在查询 existing package row 前取得 tenant advisory lock：并发首次安装的相同 identity 请求幂等返回同一安装，不同 identity 则在锁内被拒绝，不会依赖数据库唯一键竞态决定结果。

`extension_set_mounted` 解析 base → preset → session 的 effective Profile，而不是只查 raw Session rows，也不假设 Extension 使用自动生成的 row ID。卸载继承的 custom row 会在 Session 层写入同 ID disabled shadow；重新挂载复用同一 identity 并替换已校验 settings。这样模型工具与 Web 设置拥有相同的继承/覆盖语义，且不修改原始 base 或 preset。

Local 在宿主打开、用户 preset 保存和 Session Profile 修改前执行同一 Registry Profile preflight：拒绝重复 package/version、无效 settings、不可 resolve/compile 的包和跨包同名 Tool/Command。停用、撤销或卸载时，Session-only mount 会删除，base/preset 继承项会收到 disabled Session shadow，用户 preset 会清理，受影响 Session 会在下轮使用新 runtime。新 Session 会组合 base + selected preset；若 enabled 继承 Extension 已停用、撤销、卸载或无法 resolve，宿主以原 ID/config 写 disabled Session shadow，既不改 base/preset，也不让 stale 继承项阻断新会话。
