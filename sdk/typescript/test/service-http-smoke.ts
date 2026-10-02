import assert from 'node:assert/strict'
import { ServerClient, ServerError } from '../src/client.ts'

const config = JSON.parse(process.env.TERNILO_SDK_TEST_CONFIG!) as { origin: string; token: string; tenant: string; service: string; session: string }
const client = new ServerClient({ baseUrl: config.origin, accessToken: config.token, tenantId: config.tenant })
try {
  const identity = await client.request<{ user_id: string }>('/me')
  assert.equal(identity.user_id, config.service)
  const page = await client.history(config.session, { limit: 100 })
  assert.equal(page.next_before_seq, null)
  await assert.rejects(client.request('/auth/session'), error => error instanceof ServerError && error.status === 403)
  const workspaces = await client.request<{ workspaces: Array<{ owner_user_id: string }> }>('/workspaces')
  assert.equal(workspaces.workspaces.length, 1)
  assert.ok(workspaces.workspaces.every(workspace => workspace.owner_user_id === config.service))
  process.stdout.write('TypeScript service HTTP verified\n')
} finally { client.close() }
