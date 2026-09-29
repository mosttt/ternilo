# 自动化、ACP 与客户端 SDK

Ternilo 的 JSON-RPC、ACP 和 SDK 复用 `LocalApplication`、事件日志与插件内核，不需要驱动网页 DOM。单次 `ternilo run` 直接为当前目录启动一个内存 `HarnessSession`，适合脚本和快速检查，不读取网页的数据目录。

## 单次 CLI

从源码运行：

```bash
cargo run --locked -p ternilo -- run "/glob *" --json
```

安装 release binary 后：

```bash
ternilo run "完成当前工作区中的任务" --json
```

默认使用离线 rule model。单次命令使用真实模型时，通过 `--profile examples/openai-compatible-profile.json` 配置模型，并设置该 profile 引用的环境变量；它不会读取网页中保存的 Provider。需要持久 Workspace、Provider 和事件日志时使用 `ternilo rpc`、网页或桌面入口。

## 本地服务管理

`ternilo serve` 提供浏览器、桌面与可选远程连接共用的本地服务：

```bash
ternilo status --data-dir /path/to/ternilo-data
ternilo stop --data-dir /path/to/ternilo-data
```

状态命令输出实例身份、版本、PID、监听地址及网页/远程状态，不输出 API token。停止命令请求优雅关闭，并等待应用收尾和数据目录释放；成功返回后可继续需要独占目录的操作。等待超过 30 秒仍未完成时会明确报错，不代表外部副作用被回滚。两个命令均支持 `TERNILO_LOCAL_DATA_DIR`，CLI 优先。

`serve --no-local-web` 不提供网页资源，仍保留认证后的 HTTP API，包括 `GET /api/v1/service` 和 `POST /api/v1/service/stop`。本地服务发现记录与 token 保存在该数据目录的私有 `service.json`，不能当作可公开的服务状态文件分发。

## Ternilo JSON-RPC v1

启动 protocol-pure NDJSON stdio server：

```bash
ternilo rpc --data-dir /path/to/automation-data
```

Client 必须先调用 `initialize`，协议版本固定为 `1`。方法：

| 方法 | 用途 |
|---|---|
| `ping` | 存活检查 |
| `session/list` | 工作区与会话 snapshot |
| `session/new` | `workspace_path` 与 `workspace_id` 必须二选一；增加 `parent_session_id` 可在父会话所属工作区 fork |
| `session/prompt` | 立即返回 `run_id` receipt，后台执行 |
| `session/cancel` | 取消指定 run |
| `session/events` | 传入 `session_id` 读取该会话全部持久事件；不接受游标或分页参数 |
| `session/status` | 查询当前连接的 active runs |
| `session/close` | 删除并关闭会话 |
| `shutdown` | 先返回接收确认，再取消活动 run、完成应用收尾并关闭 stdio 进程 |

运行中发送 `session.event` 通知，并用 `session.status` 报告 `running`、`idle`、`failed` 或 `cancelled`。事件先持久化再广播；run 结算前会按内部游标补读尾部事件。通知不是客户端断线恢复协议；需要核对完整历史时，调用 `session/events` 全量重读，再按事件 `seq` 去重。`session/status` 只反映当前 stdio 进程登记的活动运行，历史成败应查看持久事件。

Stdout 只承载 NDJSON protocol，诊断必须写 stderr。`rpc` 与 `acp` 当前仍自行创建并独占持久应用，不会转接正在运行的 `ternilo serve`。请使用独立数据目录，或先运行 `ternilo stop --data-dir` 并等待成功返回；桌面窗口关闭不代表后台服务已停止。网页与桌面彼此可以共用一个服务，这不等于 stdio adapter 已具备复用能力。

## Python SDK

需要 Python 3.11 或更新版本。SDK 位于 `sdk/python`，源码仓库与本地交付包均包含它。在解包根目录先准备当前终端的环境；源码开发时，将第一行中的 `bin` 改为已构建程序所在目录：

```bash
export PATH="$PWD/bin:$PATH"
export PYTHONPATH="$PWD/sdk/python/src"
```

将下面内容保存为根目录中的 `sdk-example.py`。示例从 `PATH` 查找程序，并使用独立的 `automation-data/python` 数据目录；可以与默认数据目录中的网页或桌面服务同时运行：

```python
from pathlib import Path

from ternilo import HarnessClient

data_dir = Path("automation-data/python").resolve()
with HarnessClient(("ternilo", "rpc", "--data-dir", str(data_dir))) as client:
    result = client.run(
        "/glob *",
        workspace_path=str(Path.cwd()),
    )
    print(result.answer)
```

运行：

```bash
python3 sdk-example.py
```

## TypeScript SDK

需要 Node.js 22.18 或更新版本。SDK 位于 `sdk/typescript`，源码仓库与本地交付包均包含它。先按上一节把程序目录加入 `PATH`，再将下面内容保存为根目录中的 `sdk-example.mts`：

```ts
import { resolve } from 'node:path'
import { HarnessClient } from './sdk/typescript/src/client.ts'

const client = new HarnessClient({
  command: 'ternilo',
  args: ['rpc', '--data-dir', resolve('automation-data/typescript')],
})

const result = await client.run('/glob *', {
  workspacePath: process.cwd(),
})
console.log(result.answer)
await client.close()
```

运行：

```bash
node --experimental-strip-types sdk-example.mts
```

这两个示例先使用不需要模型或 API Key 的直接工具命令验证连接。`automation-data/python` 和 `automation-data/typescript` 会保留各自的工作区、会话和配置，可替换为其他专用目录；它们不会读取默认网页数据目录中的 Provider。普通自然语言任务需要配置真实模型，未配置时返回 `failed` 和配置提示，不产生回显回答。

可以在启动参数中增加 `--profile examples/openai-compatible-profile.json`，先填写示例中的模型 ID 并设置它引用的 `OPENAI_API_KEY`；也可以先用网页配置专用数据目录，再停止该目录的服务后交给 SDK 使用。需要操作已有数据时，等待服务完成收尾，再将 `--data-dir` 改为同一路径。

两套 SDK 都管理 child process 生命周期、并发 request 路由和通知订阅，提供 timeout、cancel 和有界 shutdown；高层 `run` 之外仍暴露底层 request/session API，通知可通过 `on_notification`／`onNotification` 回调接收。`HarnessClient` 启动本机 stdio 子进程；新增 `ServerClient` 直接访问 Server HTTP／Live，支持有界历史、任务队列、游标续接和网络重连，见[Server 远程 SDK](remote-sdks.md)。

`run` 返回后应检查 `status`；业务失败或取消也可以返回结果对象，不能仅因调用未抛异常就认定任务成功。等待超时只结束客户端等待，不自动取消后台 run；需要明确停止时，使用底层 `prompt` 保存 receipt 中的 `run_id`，再调用 `cancel`。`close` 请求 shutdown，并在等待超时后依次终止、强制结束子进程，不保证外部副作用回滚。

## Agent Client Protocol

启动 ACP v1 stdio adapter：

```bash
ternilo acp --data-dir /path/to/acp-data
```

Adapter 使用官方 Rust `agent-client-protocol` SDK，实现 `initialize`、`authenticate`、`session/new`、`session/prompt` 和 `session/cancel`。每个 ACP connection 只能访问自己创建的会话，每个 session 最多一个在途 prompt。

`authenticate` 当前只返回空成功响应，不执行 Server 账号登录或模型授权。没有实现历史会话列表、加载／恢复、完整审批往返或持久事件补读；输出通知仅转换已提交的助手文本消息，不包含完整工具轨迹或逐 token 增量。持久会话仍由本地数据目录保存，但新 ACP 连接不能据旧 session ID 直接接续。

支持的 content：

- 文本；
- PNG、JPEG、WebP 和 GIF；图片先进入 Ternilo 内容寻址 attachment store；
- resource link，作为名称与 URI 文本交给任务，不自动下载资源内容。

明确不接受：

- 非空 `additionalDirectories`；
- per-session MCP server；
- audio；
- embedded resource。

这些未声明能力会被拒绝，不会静默丢弃。Stdout 只承载 ACP JSON-RPC，日志必须写 stderr。ACP 是通用互操作入口；需要 durable event、status 与完整 Ternilo lifecycle 时优先使用原生 JSON-RPC。

## 将外部 Agent 作为子代理

Ternilo 也可以反过来作为 ACP client，启动 Codex、Claude Code 或其他 ACP v1 process。该配置属于 session/profile plugin，而不是 automation server；示例和权限边界见 [扩展系统：外部 ACP 子代理](extensions.md#外部-acp-子代理)。

会话日志损坏时可在停止本地服务后使用 `ternilo repair-session-log --session-id ID --data-dir PATH` 检查；显式 `--apply` 会先保存原文件备份再修复不完整尾行，详见[离线日志维护](session-log-repair.md)。
