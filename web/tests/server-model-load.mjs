import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { createServer } from 'node:http'
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { availableParallelism, cpus, platform, release, tmpdir, totalmem } from 'node:os'
import path from 'node:path'
import { parseArgs } from 'node:util'
import { performance } from 'node:perf_hooks'
import { execute, freePort, initializeServer, repository, serverRequest, startProcess, stopProcess, waitForHttp } from './platform-e2e-fixture.mjs'
import { until } from './model-device-fixture.mjs'

const { values } = parseArgs({ options: {
  concurrency: { type: 'string', default: '1,8,32' }, seconds: { type: 'string', default: '10' },
  'soak-seconds': { type: 'string', default: '60' }, instances: { type: 'string', default: '1' },
  'latency-ms': { type: 'string', default: '25' }, 'build-label': { type: 'string', default: 'unspecified' },
  output: { type: 'string', default: 'server-model-load.json' },
} })
function integer(value, minimum, maximum, name) { const n = Number(value); if (!Number.isInteger(n) || n < minimum || n > maximum) throw new Error(`${name} must be between ${minimum} and ${maximum}`); return n }
const levels = [...new Set(values.concurrency.split(',').map(value => integer(value, 1, 128, 'concurrency')))]
const seconds = integer(values.seconds, 1, 300, 'seconds'), soakSeconds = integer(values['soak-seconds'], 0, 3600, 'soak-seconds')
const latency = integer(values['latency-ms'], 0, 5000, 'latency-ms'), instances = integer(values.instances, 1, 2, 'instances')
const database = process.env.TERNILO_LOAD_DATABASE_URL
if (database && !new URL(database).pathname.startsWith('/ternilo_load_')) throw new Error('Use a new disposable PostgreSQL database whose name begins with ternilo_load_')
if (instances > 1 && !database) throw new Error('Multiple Server instances require disposable PostgreSQL')
const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-model-load-')), output = path.resolve(values.output)
const binary = process.env.TERNILO_E2E_SERVER_BINARY ?? path.join(repository, 'target/debug/ternilo-server')
const processes = [], acceptedIds = new Set(), upstreamIds = new Set(), failures = [], origins = [], credentials = []
let upstream, sequence = 0, owner
const report = { workload: 'closed_loop_model_admission_streams_and_cancellation', build_label: values['build-label'], generated_at: new Date().toISOString(),
  backend: database ? 'postgresql' : 'sqlite', source_commit: (await execute('git', ['rev-parse', 'HEAD'], { cwd: repository })).stdout.trim(),
  working_tree_dirty: Boolean((await execute('git', ['status', '--porcelain'], { cwd: repository })).stdout.trim()),
  binary: { sha256: createHash('sha256').update(await readFile(binary)).digest('hex'), version: (await execute(binary, ['--version'])).stdout.trim() },
  host: { os: platform(), release: release(), architecture: process.arch, cpu: cpus()[0]?.model, available_cpus: availableParallelism(), memory_bytes: totalmem(), node: process.version },
  settings: { concurrency: levels, seconds, soak_seconds: soakSeconds, instances, upstream_latency_ms: latency }, phases: [], errors: [], final: null }
const save = async () => { await mkdir(path.dirname(output), { recursive: true }); await writeFile(output, JSON.stringify(report, null, 2) + '\n') }
function stats(values) { values.sort((a, b) => a - b); const at = p => values[Math.min(values.length - 1, Math.ceil(values.length * p) - 1)] ?? null; return { count: values.length, p50_ms: at(.5), p95_ms: at(.95), p99_ms: at(.99), max_ms: values.at(-1) ?? null } }
const pause = ms => new Promise(resolve => setTimeout(resolve, ms))
try {
  upstream = createServer(async (request, response) => {
    try {
      assert.equal(request.url, '/v1/chat/completions')
      const chunks = []; for await (const chunk of request) chunks.push(chunk)
      const body = JSON.parse(Buffer.concat(chunks).toString()), marker = body.messages[0].content
      assert.ok(!upstreamIds.has(marker), 'one logical public request reaches the upstream once'); upstreamIds.add(marker)
      const meta = { id: marker, model: body.model, created: 1 }, usage = { prompt_tokens: 30, completion_tokens: 12, total_tokens: 42 }
      if (!body.stream) {
        await pause(latency)
        response.writeHead(200, { 'content-type': 'application/json' }).end(JSON.stringify({ ...meta, object: 'chat.completion', choices: [{ index: 0, message: { role: 'assistant', content: marker }, finish_reason: 'stop' }], usage }))
      } else {
        const frame = value => `data: ${JSON.stringify({ ...meta, object: 'chat.completion.chunk', ...value })}\n\n`
        response.writeHead(200, { 'content-type': 'text/event-stream' })
        response.write(frame({ choices: [{ index: 0, delta: { role: 'assistant', content: marker }, finish_reason: null }] }))
        let cancel
        const closed = new Promise(resolve => { cancel = resolve; response.once('close', resolve) })
        await Promise.race([pause(latency), closed]); response.removeListener('close', cancel)
        if (response.destroyed) return
        response.end(frame({ choices: [{ index: 0, delta: {}, finish_reason: 'stop' }] }) + frame({ choices: [], usage }) + 'data: [DONE]\n\n')
      }
    } catch (error) { failures.push(error.message); if (!response.headersSent) response.writeHead(500); response.end() }
  })
  await new Promise(resolve => upstream.listen(0, '127.0.0.1', resolve))
  const baseUrl = `http://127.0.0.1:${upstream.address().port}/v1`
  const application = await initializeServer({ directory: path.join(directory, 'server'), origin: `http://127.0.0.1:${await freePort()}`, mode: 'single_user', databaseUrl: database, migrationDatabaseUrl: process.env.TERNILO_LOAD_MIGRATION_DATABASE_URL })
  processes.push(application); origins.push(application.origin)
  const token = application.owner.session.access_token, userId = application.owner.session.user.user_id
  owner = (resource, options = {}) => serverRequest(application.origin, resource, { token, ...options })
  if (instances > 1) {
    const origin = `http://127.0.0.1:${await freePort()}`, folder = path.join(directory, 'peer'); await mkdir(folder)
    const config = JSON.parse(await readFile(application.configPath, 'utf8'))
    await writeFile(path.join(folder, 'config.json'), JSON.stringify({ ...config, listen: new URL(origin).host }), { mode: 0o600 })
    const peer = startProcess(binary, ['serve', '--config-dir', folder]); processes.push(peer); await waitForHttp(`${origin}/readyz`, peer); origins.push(origin)
  }
  for (let index = 0; index < 4; index++) {
    const model = `load-${index}`
    await owner('/admin/models/providers', { body: { profile: { id: model, display_name: model, base_url: baseUrl, protocol: 'openai-chat-completions', api_key_ref: null, defaults: { context_window: 32000, max_output_tokens: 1024 }, models: [{ id: model, settings: { mode: 'inherit' } }], timeout_ms: 30000, max_attempts: 1, retry_base_delay_ms: 1 }, enabled: true } })
    await owner('/admin/models/publications', { body: { model_id: model, display_name: model, provider_id: model, upstream_model: model, enabled: true } })
    const grant = await owner('/admin/models/grants', { body: { name: model, subject: { kind: 'user', id: userId }, model_ids: [model], monthly_tokens: 100000000000, max_concurrent_requests: 256, allow_resource_sharing: false } })
    const key = await owner('/model-access/keys', { body: { name: model, grant_id: grant.grant_id, model_ids: [model] } })
    credentials.push({ model, token: key.token })
  }
  const policy = async concurrency => {
    const current = await owner('/admin/models/traffic')
    await owner('/admin/models/traffic', { method: 'PUT', body: { revision: current.revision, policy: { platform: { max_concurrent_requests: concurrency }, account_default: {} } } })
  }
  const call = async (user, mode) => {
    const id = ++sequence, marker = `model-load-${id}`, credential = credentials[id % credentials.length]
    const stream = mode === 'cancellation' || id % 2 === 0
    const cancel = mode === 'cancellation' && id % 4 === 0
    const response = await fetch(`${origins[user % origins.length]}/v1/chat/completions`, { method: 'POST', headers: { authorization: `Bearer ${credential.token}`, 'content-type': 'application/json', 'idempotency-key': marker }, body: JSON.stringify({ model: credential.model, messages: [{ role: 'user', content: marker }], stream }), signal: AbortSignal.timeout(30000) })
    if (response.status === 429 && mode === 'limited') {
      assert.ok(Number(response.headers.get('retry-after')) >= 1)
      assert.equal((await response.json()).error.code, 'rate_limited')
      return 'limited'
    }
    assert.equal(response.status, 200, `unexpected HTTP ${response.status}`)
    const requestId = response.headers.get('x-ternilo-request-id'); assert.ok(requestId); assert.ok(!acceptedIds.has(requestId)); acceptedIds.add(requestId)
    if (cancel) { const reader = response.body.getReader(); const first = await reader.read(); assert.ok(first.value?.length); await reader.cancel(); return 'cancelled' }
    if (stream) { const text = await response.text(); assert.ok(text.includes(marker)); assert.match(text, /\[DONE\]/); assert.match(text, /"total_tokens":42/) }
    else { const body = await response.json(); assert.equal(body.choices[0].message.content, marker); assert.equal(body.usage.total_tokens, 42) }
    return stream ? 'streamed' : 'json'
  }
  const run = async (mode, concurrency, duration) => {
    const timings = { streamed: [], json: [], cancelled: [], limited: [] }, errors = {}
    let stopped = false
    const start = performance.now(), end = start + duration * 1000
    await Promise.all(Array.from({ length: concurrency }, async (_, user) => {
      while (performance.now() < end && !stopped) {
        const began = performance.now()
        try { const kind = await call(user, mode); timings[kind].push(performance.now() - began) }
        catch (error) { const key = `${error.name}: ${error.message}`; errors[key] = (errors[key] ?? 0) + 1; stopped = true }
      }
    }))
    const elapsed = (performance.now() - start) / 1000
    const counts = Object.values(timings).reduce((count, values) => count + values.length, 0)
    const row = { mode, concurrency, elapsed_seconds: elapsed, validated_requests: counts, validated_responses_per_second: counts / elapsed, admitted_requests_per_second: (counts - timings.limited.length) / elapsed, limited_responses_per_second: timings.limited.length / elapsed, results: Object.fromEntries(Object.entries(timings).map(([key, values]) => [key, stats(values)])), errors }
    report.phases.push(row); await save(); console.log(JSON.stringify(row))
    assert.deepEqual(errors, {}); assert.deepEqual(failures, [])
    await until(() => owner('/model-access/traffic'), value => value.active_requests === 0, 'all model leases settle after the phase')
  }
  for (const concurrency of levels) await run('baseline', concurrency, seconds)
  const concurrency = Math.max(...levels)
  await policy(Math.max(1, Math.floor(concurrency / 4))); await run('limited', concurrency, seconds); await policy(null)
  await run('cancellation', concurrency, seconds)
  if (soakSeconds) await run('soak', concurrency, soakSeconds)
  const accepted = acceptedIds.size, months = new Map(); let cursor
  do {
    const page = await owner(`/model-access/requests?limit=100${cursor ? `&cursor=${encodeURIComponent(cursor)}` : ''}`)
    for (const row of page.requests) {
      assert.ok(acceptedIds.delete(row.request_id), 'ledger contains each accepted request exactly once')
      assert.notEqual(row.state, 'pending')
      const month = months.get(row.month) ?? { requests: 0, known: 0, unknown: 0 }
      month.requests++
      if (row.accounted_tokens != null) { assert.equal(row.accounted_tokens, 42); month.known++ }
      else { assert.ok(row.reserved_tokens > 0, 'cancelled unknown usage retains reservations'); month.unknown++ }
      months.set(row.month, month)
    }
    cursor = page.next_cursor
  } while (cursor)
  assert.equal(acceptedIds.size, 0); assert.equal(upstreamIds.size, accepted)
  const periods = []
  for (const [month, counts] of months) {
    const usage = await owner(`/model-access/usage?month=${encodeURIComponent(month)}`)
    assert.equal(usage.request_count, counts.requests); assert.equal(usage.active_requests, 0)
    assert.equal(usage.used_tokens, counts.known * 42)
    periods.push({ month, accepted_requests: counts.requests, known_requests: counts.known, unknown_requests: counts.unknown, known_tokens: usage.used_tokens, reserved_tokens: usage.reserved_tokens, active_requests: 0 })
  }
  report.final = { accepted_requests: accepted, actual_upstream_calls: upstreamIds.size, periods }
  assert.deepEqual(failures, [])
} catch (error) { report.errors.push(error.message); process.exitCode = 1; console.error(error.message) }
finally {
  report.server_diagnostics = processes.map(process => ({ pid: process.child.pid, exit_code: process.child.exitCode, signal_code: process.child.signalCode, output: process.diagnostics() }))
  for (const credential of credentials) for (const diagnostic of report.server_diagnostics) diagnostic.output = diagnostic.output.replaceAll(credential.token, '[redacted]')
  for (const process of processes.reverse()) await stopProcess(process)
  if (upstream) await new Promise(resolve => { upstream.closeAllConnections(); upstream.close(resolve) })
  await rm(directory, { recursive: true, force: true }); await save()
  console.log(`Model-load evidence: ${output}`)
}
