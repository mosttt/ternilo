import assert from 'node:assert/strict'
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { chromium } from 'playwright'
import { freePort, repository, startProcess, stopProcess, waitForHttp } from './platform-e2e-fixture.mjs'
import { localApi, until } from './model-device-fixture.mjs'

const fixture = String.raw`
import { appendFileSync } from 'node:fs'
import { createInterface } from 'node:readline'
const record = value => appendFileSync(process.env.ACP_TRACE, JSON.stringify(value) + '\n')
const send = value => process.stdout.write(JSON.stringify({jsonrpc: '2.0', ...value}) + '\n')
record({type: 'started', credential: process.env.ACP_KEY === 'fixture-acp-key'})
let prompt, authorized = false, mode = 'default'
createInterface({ input: process.stdin }).on('line', line => {
  const request = JSON.parse(line)
  const method = request.method
  if (method) record({type: method})
  const reply = result => send({id: request.id, result})
  if (method === 'initialize') reply({protocolVersion: 1, agentCapabilities: {}, authMethods: [{id: 'api-key', name: 'Fixture API key'}]})
  else if (method === 'authenticate') { authorized = request.params.methodId === 'api-key' && process.env.ACP_KEY === 'fixture-acp-key'; reply({}) }
  else if (method === 'session/new') {
    if (!authorized) { send({id: request.id, error: {code: -32000, message: 'not authenticated'}}); return }
    reply({sessionId: 'external-session', modes: {currentModeId: mode, availableModes: [{id: 'default', name: 'Default'}, {id: 'plan', name: 'Plan'}]}})
  }
  else if (method === 'session/set_mode') { mode = request.params.modeId; record({type: 'mode', mode}); reply({}) }
  else if (method === 'session/prompt') {
    prompt = request.id
    if (request.params.prompt[0].text.includes('slow')) return
    send({id: 'permission', method: 'session/request_permission', params: {sessionId: 'external-session', toolCall: {toolCallId: 'write', title: 'Fixture write', status: 'pending'}, options: [{optionId: 'allow-once', name: 'Allow', kind: 'allow_once'}, {optionId: 'reject-once', name: 'Reject', kind: 'reject_once'}]}})
  }
  else if (method === 'session/cancel') { record({type: 'cancelled'}); send({id: prompt, result: {stopReason: 'cancelled'}}) }
  else if (request.id === 'permission') {
    record({type: 'permission', outcome: request.result.outcome.outcome})
    send({method: 'session/update', params: {sessionId: 'external-session', update: {sessionUpdate: 'agent_message_chunk', content: {type: 'text', text: 'ACP authenticated in ' + mode + '; permission ' + request.result.outcome.outcome}}}})
    send({id: prompt, result: {stopReason: 'end_turn'}})
  }
})
`

test('external ACP authentication, session mode, credential references and cancellation use the real host', { timeout: 120000 }, async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-acp-adapters-'))
  const artifacts = process.env.TERNILO_E2E_ARTIFACT_DIR ? path.join(process.env.TERNILO_E2E_ARTIFACT_DIR, 'acp-adapters') : undefined
  if (artifacts) await mkdir(artifacts, { recursive: true })
  const script = path.join(directory, 'agent.mjs'), trace = path.join(directory, 'trace.jsonl')
  await writeFile(script, fixture)
  const recorded = async () => (await readFile(trace, 'utf8').catch(() => '')).trim().split('\n').filter(Boolean).map(line => JSON.parse(line))
  const errors = [], proofs = []
  let app, browser, page
  try {
    const origin = `http://127.0.0.1:${await freePort()}`
    app = startProcess(process.env.TERNILO_E2E_NODE_BINARY ?? path.join(repository, 'target/debug/ternilo'), ['serve', '--data-dir', directory, '--listen', new URL(origin).host])
    await waitForHttp(origin, app)
    const api = await localApi(origin)
    const workspacePath = path.join(directory, 'workspace'); await mkdir(workspacePath)
    const workspace = await api('/workspaces', { body: { path: workspacePath } })
    await api('/credentials', { body: { name: 'ACP_CREDENTIAL', value: 'fixture-acp-key' } })
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
    page = await browser.newPage({ viewport: { width: 1440, height: 960 }, serviceWorkers: 'block' })
    page.on('pageerror', error => errors.push(error.message))
    page.on('console', message => { if (message.type() === 'error') errors.push(message.text()) })
    page.on('response', response => { if (response.status() >= 400) errors.push(`HTTP ${response.status()} ${new URL(response.url()).pathname}`) })
    await page.goto(origin)
    await page.evaluate(() => localStorage.setItem('ternilo.transcript-view', 'normal'))
    for (const scenario of ['success', 'missing-key', 'unknown-auth', 'unknown-mode', 'cancel']) {
      if (scenario === 'missing-key') await api('/credentials/ACP_CREDENTIAL', { method: 'DELETE' })
      if (scenario === 'unknown-auth') await api('/credentials', { body: { name: 'ACP_CREDENTIAL', value: 'fixture-acp-key' } })
      const session = await api('/sessions', { body: { workspace_id: workspace.workspace_id, agent_preset: 'standard' } })
      const id = session.identity.session_id
      const config = { provider_name: 'external', command: process.execPath, args: [script], env: { ACP_TRACE: trace }, env_refs: { ACP_KEY: 'ACP_CREDENTIAL' },
        auth_method: scenario === 'unknown-auth' ? 'unknown' : 'api-key', session_mode: scenario === 'unknown-mode' ? 'unknown' : 'plan', shutdown_grace_ms: 100 }
      await api(`/sessions/${id}`, { method: 'PATCH', body: { title: `ACP ${scenario}`, permissions: 'full_access', profile_plugins: [
        { id: 'model', kind: 'ternilo.model.rule', enabled: true, config: {} },
        { id: 'external', kind: 'ternilo.subagents.acp', enabled: true, config },
      ] } })
      await page.evaluate(id => localStorage.setItem('ternilo.current-session', id), id)
      await page.reload()
      const previous = (await recorded()).length
      await page.getByRole('textbox', { name: '输入任务', exact: true }).fill(`/agent-on external ${scenario === 'cancel' ? 'slow task' : 'fixture task'}`)
      await page.getByRole('button', { name: '发送', exact: true }).click()
      await page.getByRole('button', { name: '允许一次', exact: true }).click()
      if (scenario === 'cancel') {
        await until(recorded, rows => rows.slice(previous).some(row => row.type === 'session/prompt'), 'ACP prompt started')
        await page.getByRole('button', { name: '停止运行', exact: true }).click()
        await until(recorded, rows => rows.slice(previous).some(row => row.type === 'cancelled'), 'ACP cancellation notification')
      }
      const events = await until(() => api(`/sessions/${id}/events`), events => events.some(event => ['turn_finished', 'turn_failed', 'turn_cancelled'].includes(event.type)), `${scenario} settled`)
      const rows = (await recorded()).slice(previous)
      assert.ok(!JSON.stringify(events).includes('fixture-acp-key'))
      if (scenario === 'success') {
        assert.deepEqual(rows.filter(row => ['initialize', 'authenticate', 'session/new', 'session/set_mode', 'session/prompt'].includes(row.type)).map(row => row.type), ['initialize', 'authenticate', 'session/new', 'session/set_mode', 'session/prompt'])
        assert.ok(rows.some(row => row.type === 'started' && row.credential))
        assert.ok(rows.some(row => row.type === 'permission' && row.outcome === 'cancelled'), 'permission rejects by default')
        assert.ok(JSON.stringify(events).includes('ACP authenticated in plan; permission cancelled'))
      } else if (scenario === 'missing-key') {
        assert.equal(rows.length, 0, 'missing credentials prevent process launch')
        assert.ok(JSON.stringify(events).includes('not configured'))
      } else if (scenario === 'unknown-auth' || scenario === 'unknown-mode') {
        assert.ok(!rows.some(row => row.type === 'session/prompt'))
        assert.ok(JSON.stringify(events).includes('not advertised'))
      } else assert.ok(events.some(event => event.type === 'turn_cancelled'))
      proofs.push({ scenario, steps: rows.map(row => row.type) })
      await page.setViewportSize({ width: 390, height: 844 })
      assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth))
      if (artifacts) await page.screenshot({ path: path.join(artifacts, `${scenario}.png`) })
      await page.setViewportSize({ width: 1440, height: 960 })
    }
    assert.deepEqual(errors, [])
    if (artifacts) await writeFile(path.join(artifacts, 'result.json'), JSON.stringify({ status: 'passed', proofs, errors }))
  } catch (error) {
    if (artifacts && page) await page.screenshot({ path: path.join(artifacts, 'failure.png') }).catch(() => {})
    throw error
  } finally {
    await browser?.close(); if (app) await stopProcess(app)
    await rm(directory, { recursive: true, force: true })
  }
})
