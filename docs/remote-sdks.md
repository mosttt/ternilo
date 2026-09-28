# Server 远程 SDK

[English](remote-sdks.en.md)

Python 与 TypeScript 的 `ServerClient` 直接访问已运行的 Server，不启动本机子进程。它复用工作台 HTTP API 与 Live v1，使用本站登录返回的 `access_token` 和空间 ID；模型接入 Key 不能用于工作台接口。登录遵循 Server 已配置的原生／OIDC 与验证策略，SDK 不保存密码或自动替换登录身份。

一个客户端固定绑定一个账号与空间。需要切换身份或空间时创建另一个客户端，原客户端的 `close` 只关闭客户端请求与 Live 连接，不停止 Server，也不取消已提交任务。权限仍由 Server 判定：会话共享可以允许查看、提交或停止，不因此开放整台机器或其他会话。

## Python

需要 Python 3.11+。在源码或解包目录安装 SDK 与 Live 可选依赖：

```bash
python3 -m pip install './sdk/python[remote]'
```

```python
import os
from ternilo import ServerClient

with ServerClient(
    os.environ['TERNILO_SERVER_URL'],
    access_token=os.environ['TERNILO_SERVER_TOKEN'],
    tenant_id=os.environ['TERNILO_TENANT_ID'],
) as client:
    state = client.state()
    session_id = state['sessions'][0]['identity']['session_id']
    result = client.run(session_id, '/glob *', timeout=60)
    print(result.status, result.answer)
    page = client.history(session_id, limit=200)
    if page['next_before_seq'] is not None:
        older = client.history(session_id, before_seq=page['next_before_seq'])
```

只使用 HTTP 方法时无需安装 `remote` 依赖；`watch` 和 `run` 需要它。已有本地 `HarnessClient` 仍可按[自动化说明](automation.md)使用。

## TypeScript

需要 Node.js 22.18+，运行时使用内置 `fetch` 和 `WebSocket`，无需额外运行依赖。在源码或解包目录保存 `.mts` 文件：

```ts
import { ServerClient } from './sdk/typescript/src/client.ts'

const client = new ServerClient({
  baseUrl: process.env.TERNILO_SERVER_URL!,
  accessToken: process.env.TERNILO_SERVER_TOKEN!,
  tenantId: process.env.TERNILO_TENANT_ID!,
})
try {
  const sessionId = process.env.TERNILO_SESSION_ID!
  const result = await client.run(sessionId, '/glob *', { timeoutMs: 60000 })
  console.log(result.status, result.answer)
  const page = await client.history(sessionId, { limit: 200 })
  if (page.next_before_seq !== null) {
    const older = await client.history(sessionId, { beforeSeq: page.next_before_seq })
    console.log(older.events)
  }
} finally {
  client.close()
}
```

使用 `node --experimental-strip-types your-example.mts` 运行。需要创建会话时调用 `createSession(workspaceId)`／`create_session(workspace_id)`，传入当前 Server 空间可访问的公开工作区 ID；不会把脚本机器上的文件路径当作远程工作区。

## 提交、续读与停止

`submit` 返回 Server 的队列回执，可显式指定 `runId`／`run_id`。HTTP 请求不自动重试，包括提交、创建和取消；连接失败不证明 Server 没有接受请求，应通过队列或历史核对保存的运行 ID，再决定后续操作。`request` 暴露其余 HTTP 接口，路径相对于 `/api/v1`，认证和空间头由客户端设置，不自动跟随重定向。

`watch(sessionId, { afterSeq })`／`watch(session_id, after_seq=...)` 产生事件批次，保留 `reset`、`complete`、`next_seq` 与 `events`。首次可从分页历史最后的原始 `seq` 续读；断线后从已经交给调用方的最后游标重连，重放的旧序号会被去重。每条订阅独占一个 Live 连接。网络重连默认最多等待 30 秒，支持自定义期限；完整 watch 也可以设置超时，TypeScript 另支持 `AbortSignal`。

收到 `reset=true` 时，调用方需要丢弃旧历史并使用新的序号空间。回溯较早 HTTP 页不改变 Live 游标。授权失败或共享撤销会结束订阅并抛出 `ServerError`，不会无限尝试旧凭据；HTTP 错误带 `status` 和 `code`，Live 错误带 `code`。

`run` 在提交前记录历史尾游标，等待本次运行的完成／失败／取消事件，返回与本地 SDK 相同的结果结构；必须检查 `status`。入队前的配置或权限校验失败会直接抛出 `ServerError`。等待超时不会自动取消任务。需要保留运行 ID 并主动停止时，先 `submit`，然后调用 `cancel(sessionId, runId)`／`cancel(session_id, run_id)`。对尚在队列中的提交使用现有队列 HTTP 接口，并遵守其版本条件。

SDK 通过真实 Server／Node 验证共享成员文件读写、分页、Server 重启后补读离线事件、去重和共享撤权。服务账号、模型 Key 到工作台凭据转换、多 Server 路由与进程重启后的客户端游标保存仍由各自系统设计决定；应用需要自行持久化已处理游标及业务副作用，SDK 不提供跨进程恰好一次执行承诺。
