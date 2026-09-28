import assert from 'node:assert/strict'
import { ServerClient, ServerError } from '../src/client.ts'

const config = JSON.parse(process.env.TERNILO_SDK_TEST_CONFIG!) as { origin: string; token: string; tenant: string; session: string; language: string }
const client = new ServerClient({ baseUrl: config.origin, accessToken: config.token, tenantId: config.tenant })
const phase = (phase: string) => process.stdout.write(JSON.stringify({ phase }) + '\n')
try {
  const state = await client.state()
  assert.ok(JSON.stringify(state).includes(config.session))
  const written = await client.run(config.session, `/write sdk-${config.language}.txt ${config.language} remote proof`)
  assert.equal(written.status, 'idle')
  const read = await client.run(config.session, `/read sdk-${config.language}.txt`)
  assert.equal(read.status, 'idle')
  assert.equal(JSON.parse(read.answer).content, `${config.language} remote proof`)
  await assert.rejects(client.run(config.session, 'This task needs an unconfigured model'),
    error => error instanceof ServerError && error.code === 'invalid_input' && error.message.includes('No model is configured'))
  const page = await client.history(config.session, { limit: 2 })
  assert.equal(page.events.length, 2)
  assert.ok(page.next_before_seq)
  const older = await client.history(config.session, { beforeSeq: page.next_before_seq!, limit: 2 })
  assert.ok(older.events.every(event => event.seq < page.events[0].seq))
  let cursor = page.events.at(-1)!.seq, ready = false, resumed = false
  const sequences: number[] = []
  for await (const batch of client.watch(config.session, { afterSeq: cursor, timeoutMs: 60000 })) {
    cursor = batch.next_seq - 1
    sequences.push(...batch.events.map(event => event.seq))
    if (batch.complete && !ready) { ready = true; phase('ready') }
    if (batch.events.some(event => event.run_id === `offline-sdk-${config.language}` && event.type === 'turn_finished')) { resumed = true; break }
  }
  assert.ok(resumed)
  assert.equal(new Set(sequences).size, sequences.length)
  phase('resumed')
  let revoke = false
  await assert.rejects(async () => {
    for await (const batch of client.watch(config.session, { afterSeq: cursor, timeoutMs: 20000 })) {
      if (batch.complete && !revoke) { revoke = true; phase('revoke') }
    }
  }, error => error instanceof ServerError && ['policy_denied', 'invalid_input'].includes(error.code))
  await assert.rejects(client.history(config.session), error => error instanceof ServerError && [400, 403].includes(error.status ?? 0))
  phase('done')
} finally {
  client.close()
}
