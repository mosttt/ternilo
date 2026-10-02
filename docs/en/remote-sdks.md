# Remote Server SDKs

[简体中文](../zh-CN/remote-sdks.md)

Python and TypeScript `ServerClient` access an existing Server over its workbench HTTP API and Live v1. They do not spawn a local process. Supply a Server login `access_token` and tenant ID; model API keys cannot authorize workbench access. Authentication follows Server's existing native/OIDC and verification settings. The SDK does not store passwords or replace login identities.

Each client is bound to one account and space. Create another client to change identity or space. Closing a client ends its connections without shutting down Server or cancelling submitted tasks. Server enforces all existing session and workspace permissions.

Python requires 3.11+. From a source or extracted package directory, install `python3 -m pip install './sdk/python[remote]'`. HTTP-only use does not require the `remote` extra; `watch` and `run` use it.

```python
import os
from ternilo import ServerClient

with ServerClient(os.environ['TERNILO_SERVER_URL'],
                  access_token=os.environ['TERNILO_SERVER_TOKEN'],
                  tenant_id=os.environ['TERNILO_TENANT_ID']) as client:
    result = client.run(os.environ['TERNILO_SESSION_ID'], '/glob *', timeout=60)
    print(result.status, result.answer)
    page = client.history(result.session_id, limit=200)
```

TypeScript requires Node.js 22.18+ and uses built-in fetch/WebSocket without runtime dependencies. Save an `.mts` file in the source or extracted package directory and run `node --experimental-strip-types your-example.mts`.

```ts
import { ServerClient } from './sdk/typescript/src/client.ts'

const client = new ServerClient({
  baseUrl: process.env.TERNILO_SERVER_URL!,
  accessToken: process.env.TERNILO_SERVER_TOKEN!,
  tenantId: process.env.TERNILO_TENANT_ID!,
})
try {
  const result = await client.run(process.env.TERNILO_SESSION_ID!, '/glob *', { timeoutMs: 60000 })
  console.log(result.status, result.answer)
} finally {
  client.close()
}
```

`state` lists visible workbench resources. `createSession(workspaceId)` / `create_session(workspace_id)` creates a session using a public workspace ID in the selected Server space. A path on the script's computer is not a remote workspace. `history` returns bounded ascending events and `next_before_seq` for older pages. Existing local `HarnessClient` behavior is unchanged.

`submit` returns the queue receipt and accepts an explicit `runId` / `run_id`. HTTP operations are never automatically retried, including task submission, creation and cancellation. A connection failure does not prove a mutation was rejected: inspect the queue/history using your saved run ID. `request` exposes other paths relative to `/api/v1`, with authentication/tenant headers and redirects disabled.

`watch(sessionId, { afterSeq })` / `watch(session_id, after_seq=...)` yields batches retaining `reset`, `complete`, `next_seq` and `events`. Start after the last raw event sequence from a history page. Network reconnection resumes after the last delivered cursor and removes replayed older events. Each watch owns one Live connection. The default reconnection deadline is 30 seconds and can be configured; watches can also have an overall timeout, and TypeScript accepts an `AbortSignal`.

On `reset=true`, discard the old journal and use the new sequence space. Loading older HTTP pages must not rewind your Live cursor. Authorization failures and revoked sharing end the subscription with `ServerError` instead of retrying old credentials indefinitely. HTTP errors carry `status` and `code`; Live errors carry `code`.

`run` records the history tail before submitting and waits for that run's terminal event. It returns the same result shape as the local SDK; always inspect `status`. Configuration or permission failures before queue acceptance raise `ServerError` directly. A timeout does not cancel the task. To retain its ID and explicitly stop it, call `submit`, then `cancel(sessionId, runId)` / `cancel(session_id, run_id)`. Pending queue entries use the existing queue API and version conditions.

Real Server/Node tests cover shared-member file operations, pagination, Server restart and offline event recovery, deduplication and sharing revocation. Applications must persist processed cursors and coordinate their own side effects across process restarts. The SDK does not convert model keys into workbench credentials, implement multi-Server routing or promise exactly-once business execution.

## Service account access

Use a [service account](service-accounts.md) `ter_t_` credential as the `ServerClient` access token together with its space ID. `request`, paginated `history`, and allowed HTTP create/submit/cancel methods enforce the credential scopes without inheriting the creator’s identity. Query the identity with `request("/me")` rather than browser `/auth/session`. `watch` requires `resource.read` and session view permission. `run` additionally requires `run.execute` and resource submit permission. Live Hello rechecks the credential’s space. Removing workspace access ends subscriptions that lose view permission; disabling the account or revoking the credential ends its connections. Model execution still requires explicit model authorization.
