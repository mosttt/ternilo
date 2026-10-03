import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { availableParallelism, cpus, platform, release, tmpdir, totalmem } from 'node:os'
import path from 'node:path'
import { parseArgs } from 'node:util'
import { performance } from 'node:perf_hooks'
import { execute, freePort, initializeServer, repository, serverRequest, startProcess, stopProcess, waitForHttp } from './platform-e2e-fixture.mjs'
import { localApi, until } from './model-device-fixture.mjs'

const { values } = parseArgs({ options: {
  concurrency: { type: 'string', default: '1,8,32' }, seconds: { type: 'string', default: '10' },
  sessions: { type: 'string', default: '4' }, deltas: { type: 'string', default: '10000' },
  attempts: { type: 'string', default: '200' }, 'build-label': { type: 'string', default: 'unspecified' }, output: { type: 'string', default: 'server-read-load.json' },
} })
const positive = (value, maximum, name) => { const result = Number(value); if (!Number.isInteger(result) || result < 1 || result > maximum) throw new Error(`${name} must be between 1 and ${maximum}`); return result }
const levels = [...new Set(values.concurrency.split(',').map(value => positive(value, 128, 'concurrency')))]
const seconds = positive(values.seconds, 300, 'seconds'), sessions = positive(values.sessions, 100, 'sessions')
const deltas = positive(values.deltas, 100000, 'deltas'), attempts = positive(values.attempts, 10000, 'attempts')
if (sessions * (deltas + 2 * attempts + 3) > 1_000_000) throw new Error('Use at most one million synthetic events per run')
if (process.env.TERNILO_LOAD_DATABASE_URL && !new URL(process.env.TERNILO_LOAD_DATABASE_URL).pathname.startsWith('/ternilo_load_')) throw new Error('Load testing requires an empty disposable database whose name begins with ternilo_load_')
const output = path.resolve(values.output)
const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-read-load-')), processes = []
const binary = process.env.TERNILO_E2E_NODE_BINARY ?? path.join(repository, 'target/debug/ternilo')
const serverBinary = process.env.TERNILO_E2E_SERVER_BINARY ?? path.join(repository, 'target/debug/ternilo-server')
const report = {
  workload: 'closed_loop_authenticated_reads', build_label: values['build-label'], generated_at: new Date().toISOString(),
  backend: process.env.TERNILO_LOAD_DATABASE_URL ? 'postgresql' : 'sqlite',
  source_commit: (await execute('git', ['rev-parse', 'HEAD'], { cwd: repository })).stdout.trim(),
  working_tree_dirty: Boolean((await execute('git', ['status', '--porcelain'], { cwd: repository })).stdout.trim()),
  binaries: {}, host: { os: platform(), release: release(), architecture: process.arch, cpu: cpus()[0]?.model, available_cpus: availableParallelism(), memory_bytes: totalmem(), node: process.version },
  settings: { concurrency: levels, seconds, sessions, deltas_per_session: deltas, attempts_per_session: attempts },
  phases: [], errors: [],
}
const save = async () => { await mkdir(path.dirname(output), { recursive: true }); await writeFile(output, JSON.stringify(report, null, 2) + '\n') }
function journal(id, now) {
  const events = [], add = (type, fields = {}) => events.push({ seq: events.length, occurred_at_ms: now + events.length, run_id: 'load-fixture', type, ...fields })
  add('turn_started'); add('user_message', { content: 'Synthetic read-load fixture' })
  for (let index = 0; index < attempts; index++) {
    const start = events.length
    add('provider_usage_started', { source_session_id: id, step: index + 1, attempt: 1, route: { provider: `load-provider-${index % 4}`, model: `load-model-${index % 8}`, protocol: 'openai-responses' } })
    add('provider_usage_finished', { started_seq: start, error_code: null, upstream_request_id: null, usage: { input_tokens: 100, output_tokens: index % 7 === 0 ? null : 20, cached_input_tokens: null, cache_write_tokens: null, reasoning_tokens: null } })
  }
  for (let index = 0; index < deltas; index++) add('assistant_reasoning_delta', { step: attempts, delta: `Synthetic reasoning ${index}\n` })
  add('turn_failed', { message: 'Synthetic completed read fixture; no upstream request' })
  return events
}
function statistics(values) {
  values.sort((a, b) => a - b)
  const percentile = p => values.length ? values[Math.min(values.length - 1, Math.ceil(values.length * p) - 1)] : null
  return { count: values.length, p50_ms: percentile(.5), p95_ms: percentile(.95), p99_ms: percentile(.99), max_ms: values.at(-1) ?? null }
}
try {
  for (const [component, file] of Object.entries({ node: binary, server: serverBinary })) report.binaries[component] = { sha256: createHash('sha256').update(await readFile(file)).digest('hex'), version: (await execute(file, ['--version'])).stdout.trim() }
  const server = await initializeServer({ directory: path.join(directory, 'server'), origin: `http://127.0.0.1:${await freePort()}`, mode: 'single_user',
    databaseUrl: process.env.TERNILO_LOAD_DATABASE_URL, migrationDatabaseUrl: process.env.TERNILO_LOAD_MIGRATION_DATABASE_URL })
  processes.push(server)
  const tenantId = server.owner.session.personal_tenant_id, token = server.owner.session.access_token
  const owner = (resource, options = {}) => serverRequest(server.origin, resource, { token, tenantId, ...options })
  const { enrollment } = await owner(`/tenants/${tenantId}/my-computer-enrollments`, { body: { name: 'read-load-node', project_id: null, ttl_seconds: 600 } })
  const { credential } = await owner('/enrollments/consume', { body: { token: enrollment.token } })
  const origin = `http://127.0.0.1:${await freePort()}`, data = path.join(directory, 'node')
  const args = ['serve', '--listen', new URL(origin).host, '--data-dir', data, '--node-id', enrollment.executor_id, '--gateway-url', `${server.origin.replace('http:', 'ws:')}/api/v1/executors/connect`, '--allow-insecure-gateway']
  const environment = { TERNILO_LOCAL_TOKEN: credential.token, XDG_STATE_HOME: path.join(directory, 'state') }
  let node = startProcess(binary, args, environment); processes.push(node); await waitForHttp(origin, node)
  const local = await localApi(origin), folder = path.join(directory, 'workspace'); await mkdir(folder)
  const workspace = await local('/workspaces', { body: { path: folder } }), ids = []
  for (let index = 0; index < sessions; index++) ids.push((await local('/sessions', { body: { workspace_id: workspace.workspace_id } })).identity.session_id)
  const mapped = await until(() => owner('/state'), value => value.sessions.length === sessions, 'load sessions mapped')
  const publicIds = mapped.sessions.map(session => session.identity.session_id)
  await stopProcess(node)
  const calendar = new Date(), now = Date.UTC(calendar.getUTCFullYear(), calendar.getUTCMonth(), 1), month = new Date(now).toISOString().slice(0, 7)
  let length
  for (const id of ids) {
    const events = journal(id, now); length = events.length
    const file = path.join(data, 'data', 'sessions', `${Buffer.from(id).toString('hex')}.jsonl`)
    await mkdir(path.dirname(file), { recursive: true }); await writeFile(file, events.map(event => JSON.stringify(event)).join('\n') + '\n')
  }
  report.settings.events_per_session = length
  node = startProcess(binary, args, environment); processes.push(node); await waitForHttp(origin, node)
  const summaryPath = `/model-computers/${enrollment.executor_id}/usage/summary?month=${month}`
  await until(() => owner(summaryPath), value => value.totals.attempts === sessions * attempts && value.totals.completed === sessions * attempts, 'complete device observations replicated')
  for (const id of publicIds) await until(() => owner(`/sessions/${id}/history?limit=100`), value => value.events.at(-1)?.seq === length - 1, 'history fully replicated')
  const operations = ['state', 'history_tail', 'history_older', 'usage_summary']
  const call = async (operation, user) => {
    const id = publicIds[user % publicIds.length]
    const resource = operation === 'state' ? '/state' : operation === 'usage_summary' ? summaryPath : `/sessions/${id}/history?limit=100${operation === 'history_older' ? `&before_seq=${length - 100}` : ''}`
    const response = await fetch(server.origin + '/api/v1' + resource, { headers: { authorization: `Bearer ${token}`, 'x-ternilo-tenant': tenantId }, signal: AbortSignal.timeout(15_000) })
    if (!response.ok) { await response.arrayBuffer(); throw new Error(`${operation}: HTTP ${response.status}`) }
    const bytes = await response.arrayBuffer(), body = JSON.parse(Buffer.from(bytes).toString())
    if (operation === 'state') assert.equal(body.sessions.length, sessions)
    else if (operation === 'usage_summary') assert.equal(body.totals.attempts, sessions * attempts)
    else { assert.equal(body.events.length, 100); assert.equal(body.events.at(-1).seq, length - 1 - (operation === 'history_older' ? 100 : 0)) }
    return bytes.byteLength
  }
  for (const placement of ['connected', 'offline']) {
    if (placement === 'offline') {
      await stopProcess(node)
      await until(() => owner(`/tenants/${tenantId}/my-computers`), value => value.executors.every(computer => !computer.connected), 'load Node offline')
    }
    for (const concurrency of levels) {
      for (const operation of operations) await call(operation, 0)
      const timings = Object.fromEntries(operations.map(operation => [operation, []])), failures = {}, byteCounts = Object.fromEntries(operations.map(operation => [operation, 0]))
      const start = performance.now(), deadline = start + seconds * 1000
      await Promise.all(Array.from({ length: concurrency }, async (_, user) => {
        let index = user
        while (performance.now() < deadline) {
          const operation = operations[index++ % operations.length], began = performance.now()
          try { const transferred = await call(operation, user); byteCounts[operation] += transferred; timings[operation].push(performance.now() - began) }
          catch (error) { const label = `${operation}: ${error.name} ${error.message}`; failures[label] = (failures[label] ?? 0) + 1 }
        }
      }))
      const elapsed = (performance.now() - start) / 1000, counts = Object.values(timings).reduce((sum, values) => sum + values.length, 0)
      const result = { placement, concurrency, elapsed_seconds: elapsed, successful_requests: counts, requests_per_second: counts / elapsed,
        operations: Object.fromEntries(operations.map(operation => [operation, { ...statistics(timings[operation]), bytes: byteCounts[operation] }])), failures }
      report.phases.push(result); await save(); console.log(JSON.stringify(result))
      if (Object.keys(failures).length) throw new Error('Load phase failed; details retained in the JSON report')
    }
  }
} catch (error) { report.errors.push(error.message); process.exitCode = 1; console.error(error.message) }
finally {
  for (const process of processes.reverse()) await stopProcess(process)
  await rm(directory, { recursive: true, force: true }); await save()
  console.log(`Read-load evidence: ${output}`)
}
