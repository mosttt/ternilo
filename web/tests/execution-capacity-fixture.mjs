import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { mkdir, readFile, readdir, readlink, writeFile } from 'node:fs/promises'
import { createServer } from 'node:http'
import path from 'node:path'
import { DatabaseSync } from 'node:sqlite'
import { execute, freePort, initializeServer, repository, serverRequest, startProcess, stopProcess } from './platform-e2e-fixture.mjs'

export async function until(read, ready, label, diagnostic = () => '') {
  const deadline = Date.now() + 45_000
  let latest
  while (Date.now() < deadline) {
    latest = await read()
    if (ready(latest)) return latest
    await new Promise(resolve => setTimeout(resolve, 50))
  }
  throw new Error(`Timed out: ${label}; latest=${JSON.stringify(latest)}; ${diagnostic()}`)
}

async function listen(server) {
  await new Promise((resolve, reject) => { server.once('error', reject); server.listen(0, '127.0.0.1', resolve) })
  return `http://127.0.0.1:${server.address().port}`
}

async function readBody(request) {
  const chunks = []
  for await (const chunk of request) chunks.push(chunk)
  return Buffer.concat(chunks)
}

function textContent(message) {
  return typeof message.content === 'string' ? message.content
    : (message.content ?? []).map(part => part.text ?? '').join('\n')
}

export async function modelFixture() {
  const calls = [], failures = [], held = new Map()
  function hold(key, response, finish) {
    assert.equal(held.has(key), false, `a model call must not be replayed: ${key}`)
    held.set(key, { response, finish })
    response.on('close', () => held.delete(key))
  }
  function release(key) {
    const pending = held.get(key)
    assert.ok(pending, `model barrier exists: ${key}`)
    held.delete(key)
    pending.finish()
  }
  const server = createServer(async (request, response) => {
    try {
      assert.equal(request.url, '/v1/chat/completions')
      const body = JSON.parse(await readBody(request))
      const title = body.messages.some(message => textContent(message).includes('You name software-agent conversations'))
      const matches = body.messages.filter(message => message.role === 'user')
        .flatMap(message => [...textContent(message).matchAll(/CAPACITY::(wide|pressure|stop|blocker)::(\d+)::(\d+)/g)])
      const match = matches.at(-1)
      assert.ok(title || match, 'the real model request contains its task marker')
      const [, scenario, root, depthText] = match ?? []
      const depth = Number(depthText)
      const tag = title ? 'title' : `${scenario}-${root}-${depth}`
      const tools = body.messages.filter(message => message.role === 'tool')
      const spawnResult = tools.find(message => message.tool_call_id === `${tag}-spawn`)
      const waitResult = tools.find(message => message.tool_call_id === `${tag}-wait`)
      const call = { tag, title, completed: false, stage: waitResult ? 'finish' : spawnResult ? 'wait' : 'start' }
      calls.push(call)
      const content = title ? 'Capacity verification' : waitResult
        ? textContent(waitResult).includes('capacity_exhausted') ? 'Recovered capacity_exhausted.' : `Completed ${tag}.`
        : `Completed ${tag}.`
      let tool
      if (!title && scenario !== 'blocker' && depth < (scenario === 'stop' ? 1 : 2) && !waitResult) {
        assert.ok(body.tools.some(value => value.function?.name === (spawnResult ? 'wait_agent' : 'spawn_agent')), 'native subagent tools remain available')
        const args = spawnResult
          ? { subagent_id: JSON.parse(textContent(spawnResult)).subagent_id, timeout_ms: 120_000 }
          : { task: `CAPACITY::${scenario}::${root}::${depth + 1}`, label: `${scenario} ${root} depth ${depth + 1}`, background: true }
        if (spawnResult) assert.equal(typeof args.subagent_id, 'string', 'spawn returns a real Subagent ID')
        tool = { index: 0, id: `${tag}-${spawnResult ? 'wait' : 'spawn'}`, type: 'function', function: {
          name: spawnResult ? 'wait_agent' : 'spawn_agent', arguments: JSON.stringify(args),
        } }
      }
      const finish = () => {
        call.completed = true
        const identity = { id: `capacity-${calls.indexOf(call)}`, created: 1, model: body.model }
        const usage = { prompt_tokens: 100, completion_tokens: 20, total_tokens: 120 }
        const introduction = depth === 0 && tool?.function.name === 'spawn_agent'
          ? Array.from({ length: 36 }, (_, index) => `Execution note ${index + 1}: keep this earlier output readable while the child is working.`).join('\n\n') : ''
        if (!body.stream) {
          response.writeHead(200, { 'content-type': 'application/json' })
          response.end(JSON.stringify({ ...identity, object: 'chat.completion', choices: [{ index: 0, message: { role: 'assistant', content }, finish_reason: 'stop' }], usage }))
          return
        }
        const sse = value => `data: ${JSON.stringify(value)}\n\n`
        response.writeHead(200, { 'content-type': 'text/event-stream' })
        response.end(sse({ ...identity, object: 'chat.completion.chunk', choices: [{ index: 0,
          delta: tool ? { role: 'assistant', content: introduction, tool_calls: [tool] } : { role: 'assistant', content }, finish_reason: null }] })
          + sse({ ...identity, object: 'chat.completion.chunk', choices: [{ index: 0, delta: {}, finish_reason: tool ? 'tool_calls' : 'stop' }] })
          + sse({ ...identity, object: 'chat.completion.chunk', choices: [], usage }) + 'data: [DONE]\n\n')
      }
      if (!title && scenario === 'wide' && depth === 0 && call.stage === 'start') {
        hold(tag, response, finish)
        if (calls.filter(value => /^wide-\d-0$/.test(value.tag) && value.stage === 'start').length === 4) {
          for (let index = 0; index < 4; index++) release(`wide-${index}-0`)
        }
      } else if (!title && ((scenario === 'wide' && depth === 2) || (scenario === 'pressure' && depth === 1 && call.stage === 'start')
        || (scenario === 'stop' && depth === 1) || scenario === 'blocker')) hold(tag, response, finish)
      else finish()
    } catch (error) {
      failures.push(error.stack)
      if (!response.headersSent) response.writeHead(500, { 'content-type': 'application/json' })
      response.end(JSON.stringify({ error: { message: error.message } }))
    }
  })
  const origin = await listen(server)
  return { baseUrl: `${origin}/v1`, calls, failures, held, release,
    close: async () => { for (const entry of held.values()) entry.response.destroy(); held.clear(); server.closeAllConnections(); await new Promise(resolve => server.close(resolve)) },
  }
}

/** Delay only a real resume request; every authorization and admission still runs on Server. */
export async function workerProxy(origin) {
  const gates = new Map(), calls = [], failures = []
  let questionHandler
  const server = createServer(async (request, response) => {
    const cancellation = new AbortController()
    response.on('close', () => { if (!response.writableEnded) cancellation.abort() })
    try {
      const bytes = await readBody(request)
      const envelope = request.url === '/internal/worker/v1/rpc' ? JSON.parse(bytes) : null
      const rpc = envelope?.request
      const record = rpc ? { worker_id: envelope.identity.worker_id, operation: rpc.operation, run_id: rpc.run?.run_id, activity_revision: rpc.activity_revision } : null
      if (record) calls.push(record)
      const gate = rpc?.operation === 'resume_run' ? gates.get(rpc.run.run_id) : null
      if (gate) { gate.entered = true; await gate.promise }
      const upstream = await fetch(new URL(request.url, origin), { method: request.method,
        headers: { ...(request.headers.authorization ? { authorization: request.headers.authorization } : {}), ...(bytes.length ? { 'content-type': 'application/json' } : {}) },
        ...(bytes.length ? { body: bytes } : {}), signal: cancellation.signal,
      })
      response.writeHead(upstream.status, { 'content-type': upstream.headers.get('content-type') ?? 'application/json' })
      if (rpc) {
        const text = await upstream.text()
        const reply = JSON.parse(text)
        Object.assign(record, { status: upstream.status, reply: reply.type, admission: reply.admission?.status, error: reply.error?.message })
        if (upstream.ok && rpc.operation === 'record_question') await questionHandler?.(rpc)
        response.end(text)
      } else {
        for await (const chunk of upstream.body) response.write(chunk)
        response.end()
      }
    } catch (error) {
      if (cancellation.signal.aborted) return
      failures.push(error.stack)
      response.destroy(error)
    }
  })
  const url = await listen(server)
  return { url, calls, failures,
    onQuestion(handler) { questionHandler = handler },
    holdResume(runId) { let release; const promise = new Promise(resolve => { release = resolve }); const gate = { entered: false, promise, release }; gates.set(runId, gate); return gate },
    releaseResume(runId) { const gate = gates.get(runId); assert.ok(gate); gates.delete(runId); gate.release() },
    close: async () => { for (const gate of gates.values()) gate.release(); gates.clear(); server.closeAllConnections(); await new Promise(resolve => server.close(resolve)) },
  }
}

async function startCapacityWorker(directory, proxy, credential, active, resident) {
  const binary = process.env.TERNILO_CLOUD_E2E_WORKER_BINARY ?? path.join(repository, 'target/debug/ternilo-worker')
  const config = path.join(directory, `${credential.worker_id}.json`)
  await execute(binary, ['init', '--config-dir', path.dirname(config), '--server-url', proxy.url, '--workspace-root', path.join(directory, 'workspaces'),
    '--sandbox', 'bubblewrap', '--health-listen', '127.0.0.1:0', '--poll-interval-ms', '50', '--max-active-runs', String(active), '--max-resident-runs', String(resident)],
  { cwd: repository, env: { ...process.env, TERNILO_WORKER_TOKEN: credential.token } })
  const worker = startProcess(binary, ['serve', '--config-dir', path.dirname(config)])
  try {
    await until(() => worker.diagnostics(), text => /Ternilo cloud worker .* ready/.test(text), 'real Bubblewrap Worker ready')
    assert.equal(JSON.parse(await readFile(config)).sandbox, 'bubblewrap')
    return worker
  } catch (error) {
    await stopProcess(worker)
    throw error
  }
}

export async function startScenario(directory, active, resident, upstream) {
  await mkdir(directory, { recursive: true, mode: 0o700 })
  const policy = JSON.parse(await readFile(path.join(repository, 'examples/worker-policy.json')))
  policy.policy_revision = 'execution-capacity-browser'
  policy.minimum_workspace_free_bytes = 0
  policy.allowed_plugin_kinds = [...new Set([...policy.allowed_plugin_kinds,
    'ternilo.model.host_gateway', 'ternilo.sandbox.cloud_outer', 'ternilo.files.local', 'ternilo.shell.local',
    'ternilo.prompt.workspace_instructions', 'ternilo.tools.files', 'ternilo.tool.shell', 'ternilo.tool.ask_user',
    'ternilo.tool.plan', 'ternilo.skills.registry', 'ternilo.skills.filesystem', 'ternilo.tools.skills',
    'ternilo.subagents.in_process', 'ternilo.tools.agent_team'])]
  const policyPath = path.join(directory, 'policy.json')
  await writeFile(policyPath, JSON.stringify(policy))
  const databasePath = path.join(directory, 'server.sqlite3')
  const origin = `http://127.0.0.1:${await freePort()}`
  let application, proxy, worker, database, monitor
  const peers = new Set()
  const samples = []
  try {
    application = await initializeServer({ directory: path.join(directory, 'server'), origin,
      databaseUrl: `sqlite:${databasePath}`, workerPolicy: policyPath, managedExecutionEnabled: true })
    const token = application.owner.session.access_token
    const owner = (resource, options = {}) => serverRequest(origin, resource, { token, ...options })
    const registration = await owner('/admin/registration')
    await owner('/admin/registration', { method: 'PATCH', body: { mode: 'open', require_approval: false, revision: registration.revision } })
    const memberCredentials = { username: 'capacity-member', email: 'capacity-member@example.test', password: 'capacity-member-password' }
    const registered = await serverRequest(origin, '/auth/register', { body: memberCredentials })
    assert.ok(registered.session?.access_token)
    const member = registered.session
    const tenant = (await owner('/tenants', { body: { slug: 'capacity', display_name: 'Capacity verification', quota: {
      max_nodes: 10, max_concurrent_runs: active, monthly_model_tokens: 20_000_000, max_secrets: 100,
    } } })).tenant
    const tenantId = tenant.tenant_id
    await owner(`/tenants/${tenantId}/members/${member.user.user_id}`, { method: 'PUT', body: { role: 'member' } })
    const api = (resource, options = {}) => owner(resource, { tenantId, ...options })
    const memberApi = (resource, options = {}) => serverRequest(origin, resource, { token: member.access_token, tenantId, ...options })
    const project = (await api('/projects', { body: { name: 'Capacity tasks' } })).project
    const profile = { id: 'capacity-upstream', display_name: 'Capacity upstream', base_url: upstream.baseUrl,
      protocol: 'openai-chat-completions', api_key_ref: null,
      defaults: { context_window: 128_000, max_output_tokens: 2048 }, models: [{ id: 'capacity-model', settings: { mode: 'inherit' } }],
      timeout_ms: 60_000, max_attempts: 1, retry_base_delay_ms: 50 }
    await owner('/admin/models/providers', { body: { profile, enabled: true, api_key: 'capacity-fixture-key' } })
    await owner('/admin/models/publications', { body: { model_id: 'capacity-model', display_name: 'Capacity model', provider_id: profile.id, upstream_model: 'capacity-model', enabled: true } })
    const grant = await owner('/admin/models/grants', { body: { name: 'Owner model budget', subject: { kind: 'user', id: application.owner.session.user.user_id },
      model_ids: ['capacity-model'], monthly_tokens: 20_000_000, max_concurrent_requests: active, allow_resource_sharing: true } })
    const assetHashes = {}
    for (const asset of ['app.js', 'app.css']) {
      const response = await fetch(`${origin}/assets/${asset}`)
      assert.equal(response.status, 200)
      const digest = bytes => createHash('sha256').update(bytes).digest('hex')
      assetHashes[asset] = digest(Buffer.from(await response.arrayBuffer()))
      assert.equal(assetHashes[asset], digest(await readFile(path.join(repository, 'web/dist/assets', asset))))
    }
    proxy = await workerProxy(origin)
    const credential = await owner('/admin/workers', { body: { worker_id: 'capacity-worker' } })
    worker = await startCapacityWorker(directory, proxy, credential, active, resident)
    assert.ok(path.resolve(databasePath).startsWith(`${path.resolve(directory)}${path.sep}`))
    database = new DatabaseSync(databasePath, { readOnly: true })
    const query = (sql, ...args) => database.prepare(sql).all(...args).map(row => ({ ...row }))
    const approvals = []
    proxy.onQuestion(async rpc => {
      const question = rpc.question
      assert.equal(question.tool_approval?.tool_name, 'spawn_agent', 'the fixture approves only its explicit native child launches')
      assert.match(question.tool_approval.arguments.task, /^CAPACITY::(?:wide|pressure|stop)::\d+::[12]$/)
      const run = query('SELECT session_id FROM cloud_runs WHERE tenant_id=? AND run_id=?', tenantId, rpc.run.run_id)[0]
      assert.ok(run)
      await api(`/questions/${encodeURIComponent(question.id)}/answer?session_id=${encodeURIComponent(run.session_id)}`, { body: { selected: ['Allow once'] } })
      approvals.push({ run_id: rpc.run.run_id, session_id: run.session_id, question_id: question.id, tool: 'spawn_agent' })
    })
    const snapshot = () => {
      const rows = query('SELECT run_id, session_id, phase, owner_user_id FROM cloud_run_execution WHERE tenant_id=?', tenantId)
      const value = { at: Date.now(), active: rows.filter(row => ['claimed', 'active'].includes(row.phase)).length,
        resident: rows.filter(row => row.phase !== 'released').length, rows }
      if (JSON.stringify(samples.at(-1)?.rows) !== JSON.stringify(rows)) samples.push(value)
      assert.ok(value.active <= active && value.resident <= resident, `observed capacity exceeded: ${JSON.stringify(value)}`)
      return value
    }
    let monitorError
    monitor = setInterval(() => { try { snapshot() } catch (error) { monitorError = error } }, 20)
    const fixture = { application, worker, proxy, database, query, snapshot, samples, assetHashes, approvals, tenantId, api, memberApi, owner, member, memberCredentials, grant,
      check() {
        if (monitorError) throw monitorError
        assert.deepEqual(upstream.failures, []); assert.deepEqual(proxy.failures, [])
        for (const process of [worker, ...peers]) assert.equal(process.child.exitCode, null, process.diagnostics())
      },
      async startPeerWorker(workerId) {
        const peerCredential = await owner('/admin/workers', { body: { worker_id: workerId, storage_id: credential.storage_id } })
        const peer = await startCapacityWorker(directory, proxy, peerCredential, active, resident)
        peers.add(peer)
        await until(() => proxy.calls.some(call => call.worker_id === workerId && call.operation === 'claim_run' && call.status === 200), Boolean, 'the spare Worker actually polls Server')
        return { ...peer, close: async () => { await stopProcess(peer); peers.delete(peer) } }
      },
      async session(id, workspaceId) {
        const workspace = workspaceId ? { workspace_id: workspaceId } : (await api('/workspaces', { body: { project_id: project.project_id, name: id, placement: 'cloud' } })).workspace
        await api('/sessions', { body: { workspace_id: workspace.workspace_id, session_id: id, permissions: 'workspace_write' } })
        const session = await api(`/sessions/${id}`, { method: 'PATCH', body: {
          model: { provider: 'platform_model', grant_id: grant.grant_id, model_id: 'capacity-model' } } })
        await api(`/sessions/${id}/sharing/user/${member.user.user_id}`, { method: 'PUT', body: { view: true, submit: true, stop: true, configure: false } })
        return session
      },
      async submit(id, input) { return memberApi(`/sessions/${id}/queue`, { body: { content: { kind: 'prompt', input } } }) },
      runs() { return query('SELECT run_id,session_id,state,error,actor_user_id,user_id,quota_reservation_id FROM cloud_runs WHERE tenant_id=?', tenantId) },
      diagnostic() { return `${JSON.stringify(fixture.runs())}\n${worker.diagnostics().slice(-6000)}` },
      async close() { clearInterval(monitor); database.close(); await Promise.all([worker, ...peers].map(stopProcess)); await proxy.close(); await stopProcess(application) },
    }
    return fixture
  } catch (error) {
    clearInterval(monitor); database?.close(); await Promise.all([worker, ...peers].map(stopProcess)); await proxy?.close(); await stopProcess(application)
    throw error
  }
}

export async function isolatedExecutions(workerPid) {
  const processes = [], visited = new Set()
  const hostMount = await readlink(`/proc/${workerPid}/ns/mnt`)
  async function visit(pid) {
    if (visited.has(pid)) return
    visited.add(pid)
    let threads
    try { threads = await readdir(`/proc/${pid}/task`) } catch (error) { if (error.code === 'ENOENT') return; throw error }
    const children = new Set()
    for (const thread of threads) {
      try {
        const content = await readFile(`/proc/${pid}/task/${thread}/children`, 'utf8')
        for (const child of content.trim().split(/\s+/).filter(Boolean)) children.add(child)
      } catch (error) { if (error.code !== 'ENOENT') throw error }
    }
    for (const child of children) {
      try {
        const argv = (await readFile(`/proc/${child}/cmdline`, 'utf8')).split('\0').filter(Boolean)
        if (argv[0] === '/worker' && argv[1] === 'execute') {
          const mount = await readlink(`/proc/${child}/ns/mnt`)
          assert.notEqual(mount, hostMount, 'each actual execution uses the Bubblewrap mount namespace')
          processes.push({ pid: Number(child), mount, argv })
        }
        await visit(child)
      } catch (error) { if (error.code !== 'ENOENT') throw error }
    }
  }
  await visit(String(workerPid))
  return processes
}
