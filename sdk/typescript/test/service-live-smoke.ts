import assert from 'node:assert/strict'
import { ServerClient } from '../src/client.ts'
const config = JSON.parse(process.env.TERNILO_SDK_TEST_CONFIG!) as { origin: string; token: string; tenant: string; session: string }
const client = new ServerClient({ baseUrl: config.origin, accessToken: config.token, tenantId: config.tenant })
try {
  assert.ok(JSON.stringify(await client.state()).includes(config.session))
  const written = await client.run(config.session, '/write service-typescript.txt service TypeScript proof', { timeoutMs: 30000 })
  assert.equal(written.status, 'idle')
  const read = await client.run(config.session, '/read service-typescript.txt', { timeoutMs: 30000 })
  assert.equal(JSON.parse(read.answer).content, 'service TypeScript proof')
  process.stdout.write('TypeScript service Live run verified\n')
} finally { client.close() }
