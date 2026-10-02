import assert from 'node:assert/strict'
import { ServerClient, ServerError } from '../src/client.ts'
const config = JSON.parse(process.env.TERNILO_SDK_TEST_CONFIG!) as { origin: string; token: string; tenant: string; session: string; denied: boolean }
const client = new ServerClient({ baseUrl: config.origin, accessToken: config.token, tenantId: config.tenant })
try {
  try {
    const result = await client.run(config.session, 'Complete the service-model task and write its proof.', { timeoutMs: 30000 })
    if (config.denied) assert.equal(result.status, 'failed')
    else { assert.equal(result.status, 'idle'); assert.match(result.answer, /service-model/) }
  } catch (error) {
    if (!config.denied || !(error instanceof ServerError) || !['policy_denied', 'invalid_input'].includes(error.code)) throw error
  }
  process.stdout.write(config.denied ? 'Service model denied\n' : 'Service model completed\n')
} finally { client.close() }
