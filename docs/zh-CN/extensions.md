# 扩展系统

Ternilo 的产品行为通过可信 catalog 中的插件组合。Profile row 使用稳定 `id` 和 `kind`；多层 profile 中后层以相同 ID 替换整行，新 ID 追加。服务依赖图决定激活顺序，插件注册的 prompt、tool 和 service 都由 activation effect 拥有，卸载时精确撤销。

安全边界不由 profile 插件决定：身份、租户隔离、catalog trust、secret、quota ceiling 和 OS sandbox 的最终授权属于宿主。普通插件只能使用已授予 capability，不能用同名 service 放宽 policy。

## 配置插件

设置页把插件数据分成三个作用域，三者不能互相替代：

| 层级 | 当前动作 | 生效范围 |
|---|---|---|
| Extension inventory | 安装、卸载、目标级启停 package，信任或撤销发布者 | 当前配置目标；本机或“此电脑”为对应 Node，Cloud 为当前 tenant 的 Cloud 执行环境 |
| Agent 预设 | 定义插件组合、启用状态和 `config` 基线 | 使用该预设的新会话；显式应用到空闲会话后成为该会话的新基线 |
| Session override | 在插件页修改配置字段、启停插件或挂载组件 | 仅当前 Session；“恢复默认”删除该插件的覆盖并重新继承 composed base/preset Profile |

静态 `PluginFactory` 从真实 Rust 配置类型发布 JSON Schema。当前 Session 的插件配置卡据此生成 object、string、number、boolean、enum 和嵌套字段；数组或开放 object 使用局部 JSON 编辑。保存会写入 Session 的整行 `profile_plugins` 覆盖，恢复默认会移除该行。浏览器 Schema 不是安全边界，后端仍会解析候选 Profile 并验证依赖图。

Agent 预设编辑器对已有插件 row 复用同一份 Catalog JSON Schema；Extension mount 使用已安装签名 manifest 的 `config_schema` 编辑其 `settings`。高级编辑区同时显示“完整 Profile JSON（只读）”和“预设覆盖 JSON（可编辑）”：完整预览包含继承的基础插件，覆盖配置只保存新增或修改的插件。复制标准预设后，覆盖 JSON 可能只有 `code-mode`，其余基础插件仍生效。删除覆盖项会恢复继承，关闭继承插件则保存该项的禁用覆盖。普通用户不需要直接编辑 Node 数据目录中的配置文件。

Session override 中的典型 row：

```json
{
  "id": "example",
  "kind": "ternilo.example.plugin",
  "enabled": true,
  "config": {}
}
```

主 Agent 的 step 设置就是同一机制下的普通插件字段：

```json
{
  "id": "agent-loop",
  "kind": "ternilo.agent.react",
  "enabled": true,
  "config": { "max_steps": 0 }
}
```

当前 Session 的 Schema 卡可以直接编辑 `max_steps`；`0` 表示插件不主动限制，正整数表示请求的主 Agent 上限。把同一 row 写入 Agent 预设并设为默认，会影响使用该预设的新会话。Host/CLI/Worker ceiling 仍可把有效值压低。内置子 Agent 的 `ternilo.subagents.in_process.config.max_steps` 是独立字段，不继承主 Agent 值。

空闲会话可以显式切换 Agent 预设，即使已有历史；切换会保存新的预设快照并重启 session runtime，后续 turn 使用新插件图。正在执行 turn 时切换会被拒绝，避免同一 turn 混用新旧图。运行时 extension 挂载则在当前 turn 完成后重启 session runtime。

“扩展组件”页签中的 package 启停改变当前目标的 Extension inventory；同一页签中的“挂载到当前会话”只写 Session override。停用或卸载 package 会清理受影响的会话与用户预设挂载，不能把 package 的目标级可用性误解为某个会话的插件开关。

Extension 卡片的开关显示 composed effective Profile，而不是只查看 raw Session row。base 或 preset 继承的挂载也会显示为开启；关闭继承项时，Web 写入相同 row identity 的 disabled Session shadow，重新开启则原位更新该 override。编辑继承设置会创建 Session override，不会修改预设本身。若签名 `config_schema` 含 required 且没有 default 的字段，首次点击挂载只展开配置，填写并保存后才以一次 Session mutation 创建完整、启用的 mount。

## MCP 与 LSP

启用插件只登记服务配置。MCP 在任务开始执行后建立连接并发现工具，LSP 在首次使用时启动；打开服务状态页不会启动它们。成功调用后保留进程与服务状态，Local 下会持续占用当前声明的工作目录。会话顶部的“更多 → 后台服务”可以查看、启动或停止，具体行为见[使用指南](agents.md#查看和控制后台服务)。手动停止仅抑制当前运行实例的自动启动；需要长期禁用时关闭插件。

### MCP stdio

首次添加时，进入“用户设置 → Agent 预设”，复制当前预设并编辑，在“插件 Profile（高级）→ 预设覆盖 JSON（可编辑）”的 `plugins` 数组中追加下面的配置，保留原有项。保存后点击预设卡片的“使用”，应用到当前空闲会话。随后可在“用户设置 → 插件 → 插件配置”通过字段表单调整 MCP。程序需要安装在执行任务的电脑或环境中；当前接入为 stdio，不接受 HTTP／SSE 地址。

```json
{
  "id": "project-mcp",
  "kind": "ternilo.mcp.stdio",
  "enabled": true,
  "config": {
    "server_name": "project",
    "command": "mcp-server-command",
    "args": [],
    "env": {},
    "env_refs": {
      "SERVICE_API_KEY": "PROJECT_SERVICE_API_KEY"
    },
    "cwd": "/projects/example",
    "startup_timeout_ms": 15000,
    "tool_call_timeout_ms": 60000,
    "reconnect_attempts": 0
  }
}
```

省略 `cwd` 时使用会话工作区。子进程先清空环境，只保留基础系统变量、显式 `env` 和由宿主凭据 store 解析的 `env_refs`。MCP 工具进入共享 permission、Plan、Hook 和审批链。

收到 `notifications/tools/list_changed` 后，下一次工具准备会在当前调用结束后刷新工具目录，沿用原进程并使旧 schema 的 handler 失效。目录刷新失败会停止服务，具体错误显示在后台服务状态中，其他原生工具仍可使用。

`reconnect_attempts` 默认为 `0`，可设置为 `0` 到 `10` 的整数。启用后，失败服务在后续任务准备时进行有限次数重连，不会自动重发已失败或取消的工具调用。实际完成工具 RPC 或手动重新启动会恢复重连预算；单纯连接成功不会恢复预算。手动停止后保持停止，需要显式启动。

### LSP stdio

```json
{
  "id": "rust-lsp",
  "kind": "ternilo.lsp.stdio",
  "enabled": true,
  "config": {
    "server_name": "rust",
    "command": "rust-analyzer",
    "args": [],
    "env": {},
    "env_refs": {},
    "initialization_options": null,
    "timeout_ms": 30000,
    "max_message_bytes": 8388608
  }
}
```

LSP 对模型表现为 `lsp__<server>` 危险工具，每次 JSON-RPC 调用都需要符合宿主审批策略。进程退出、协议消息过大或 timeout 会关闭式失败。

## Code Runtime 与 Code Mode

`ternilo.code_runtime.rhai` 提供 `ternilo/code-runtime@1` 服务，`ternilo.tools.code_mode` 把当前工具表投影成动态 Rhai SDK。运行时本身不知道 session、tool 或 Hooks；consumer 才把 binding 送回共享工具流水线，因此替换语言 runtime 不需要复制 agent loop。

工具呈现模式：

- `native`：模型只看到原生工具；
- `code`：模型只看到 `run_code` 和动态 SDK；
- `both`：两者都可见，本地 standard profile 默认使用此模式。

配置示例：

```json
{
  "id": "code-mode",
  "kind": "ternilo.tools.code_mode",
  "enabled": true,
  "config": {
    "mode": "code",
    "max_subcalls": 128
  }
}
```

合法 Rhai 标识符得到具名 binding，例如：

```rhai
let value = tools::read_file(#{ path: "README.md" });
#{ content: value }
```

其他工具用 `call_tool("server-tool", #{ ... })`。SDK 从工具 JSON Schema 生成参数形状；`run_code` 本身不会递归出现在 binding 集合。

每次调用创建新的 Engine 和 Scope，不共享变量或模块。Rhai 没有文件、网络、进程、环境变量、credential、WASI、`import` 或 `eval`；唯一宿主入口是当次 binding。默认限制包括 1,000,000 operations、60 秒墙钟、4 MiB 输出、1 MiB 单字符串、100,000 collection items、64 层调用/表达式、4,096 变量和 256 函数。

每个 SDK 子调用重新执行：Code-only 检查、PreTool Hook、permission/Plan/guard、一次性危险审批、handler 和 PostTool Hook。父子 dispatch 写入 durable event，但 model history 只看到外层 `run_code` 结果。Rhai capability sandbox 不替代多租户 OS 隔离；cloud run 仍位于无网络 child 中。

离线验证：

```bash
cargo run --locked -p ternilo -- run \
  '/code let files = tools::glob_files(#{ pattern: "*" }); #{ files: files }' \
  --json
```

## Hooks

Hooks 是内核 service，在以下边界运行：

- `SessionStart`
- `UserPromptSubmit`
- `PreToolUse`
- `PostToolUse`
- `Stop`

原生 Rust 插件可实现 `HookHandler`。`ternilo.hooks.claude_code` 和 `ternilo.hooks.codex` 兼容对应产品的同步 command-hook 子集，并共享 matcher、command runner、codec 和 durable result event。

Claude Code bridge 示例：

```json
{
  "id": "project-hooks",
  "kind": "ternilo.hooks.claude_code",
  "enabled": true,
  "config": {
    "configPath": "/projects/example/.claude/settings.json",
    "pluginRoot": "/projects/example/.claude/plugins/example",
    "projectDir": "/projects/example",
    "defaultTimeoutMs": 600000,
    "stderrSummaryMaxChars": 500
  }
}
```

桥支持 wrapped `{ "hooks": ... }` 和 bare event map、match-all/Claude literal alternation/Codex regex、timeout、JSON stdin、decision、additional context、system message 和 `continue:false`。多个 handler 按配置顺序运行，决策优先级为 `deny > ask > allow`。

PreTool deny 不执行工具；ask 进入现有用户问题队列。Stop deny 会注入反馈并强制下一步。Hook context 和 result 先写入 session log，再进入模型与 Web/automation stream。命令子进程会移除继承环境中的 credential-like 变量，只加入配置显式提供的环境。

兼容桥只执行同步 command handler，不声称支持供应商所有 HTTP、prompt、agent、MCP handler、输入重写或未映射生命周期事件。外部 Codex/Claude agent 应配置为 ACP subagent provider，而不是 Hook。

## 外部 ACP 子代理

`ternilo.subagents.in_process` 提供默认 provider；`ternilo.subagents.acp` 可以挂载任意 ACP v1 进程：

```json
{
  "id": "codex-acp",
  "kind": "ternilo.subagents.acp",
  "enabled": true,
  "config": {
    "provider_name": "codex",
    "command": "codex-acp",
    "args": [],
    "permission": "reject",
    "timeout_ms": 600000,
    "shutdown_grace_ms": 3000
  }
}
```

`provider_name` 在 profile 中必须唯一。省略 `cwd` 时继承规范化工作区；显式 `cwd` 在插件构建时解析为规范化绝对路径，相对路径以宿主进程的当前目录为基准，启动子代理时再检查其是否为可访问目录。进程使用清空后的环境，只保留基础系统变量、`env` 明确给出的值，以及 `env_refs` 指定的凭据。`env_refs` 把子进程环境变量名映射到执行电脑的凭据名称，每次启动重新解析，不把 Key 原文写入预设。两张映射不能使用同一个变量名，引用缺失会在启动进程前报错。

`permission` 默认 `reject`，即自动拒绝外部 agent 的 ACP 权限请求。只有该进程已经由自身 sandbox 或可信策略约束时才应设为 `allow`。ACP provider 每个任务创建独立 session，不支持对同一外部 session `/agent-send`；取消会先发送 ACP `session/cancel`，宽限期后终止整个进程组。

`auth_method` 可选，必须是代理在 `initialize` 中公布的认证方式 ID；Ternilo 在创建会话前发送 `authenticate`。不填则使用代理已有的登录或环境凭据，不自动弹出登录流程。`session_mode` 可选，必须是 `session/new` 公布的模式 ID；在发送任务前设置，不自动切换到更宽松权限。协议版本、认证方式或模式不支持时明确失败。

### Gemini CLI 与 Claude Agent ACP

在执行电脑安装并配置外部程序。Gemini CLI 使用 `gemini --acp`，Claude Agent SDK 的 ACP 适配器使用 `claude-agent-acp`。相关插件配置见 [`gemini-acp-profile.json`](../../examples/gemini-acp-profile.json) 和 [`claude-acp-profile.json`](../../examples/claude-acp-profile.json)。二者仍使用通用的 `ternilo.subagents.acp` kind，不自动下载安装程序。Windows 也可以把 `command` 设置为 `node`，在 `args` 中用绝对路径指定已安装包的 `bundle/gemini.js` 或 `dist/index.js` 入口；Gemini 入口后增加 `--acp`，路径单独作为一个参数。

Gemini 示例把 `GEMINI_AGENT_KEY` 凭据传给子进程的 `GEMINI_API_KEY`；Claude 示例把 `CLAUDE_AGENT_KEY` 传给 `ANTHROPIC_API_KEY`。如已在该电脑用供应商程序完成登录，可删除对应 `env_refs`。这类代理自行选择和调用模型，不使用当前会话的 Server 模型委托，也不继承跨电脑模型转发授权。

接入依据是 [Gemini CLI ACP 模式](https://github.com/google-gemini/gemini-cli/blob/main/docs/cli/acp-mode.md)、[Claude Agent ACP](https://github.com/agentclientprotocol/claude-agent-acp) 和 [ACP 认证协议](https://agentclientprotocol.com/protocol/authentication)。支持范围为 ACP v1 文本任务、文本结果、认证／模式协商、权限允许／拒绝和取消；不宣称完整终端、图片、会话恢复或供应商私有扩展支持。

用户侧命令见 [用户指南](agents.md#子代理后台任务与终端)。

## 开发扩展包

发布自定义工具、提示词、技能、Hook、命令或 Provider 模板，见[扩展包开发](extension-packages.md)。支持 Rhai 和 WASM Component；包签名与宿主权限检查对两者相同。
