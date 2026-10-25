import assert from 'node:assert/strict'
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { chromium } from 'playwright'
import { freePort, initializeServer, repository, serverRequest, startProcess, stopProcess, waitForHttp } from './platform-e2e-fixture.mjs'
import { localApi, until } from './model-device-fixture.mjs'

const beforeQuestion = 'Here are the two approaches and their tradeoffs.'
const customAnswer = 'Compare both before implementing.'
const finalAnswer = 'The comparison is complete.'

function questionHistory() {
  const events = []
  const add = (type, fields = {}) => events.push({ seq: events.length, occurred_at_ms: 1_700_000_000_000 + events.length, run_id: 'question-history', type, ...fields })
  const response = (step, content, tool_calls = []) => add('assistant_message', { step, response: {
    provider: 'fixture', model: 'fixture', content, tool_calls, finish_reason: tool_calls.length ? 'tool_calls' : 'stop',
  } })
  add('turn_started')
  add('user_message', { content: 'Help choose an approach.' })
  add('step_started', { step: 1 })
  for (let index = 0; index < 1_400; index++) add('assistant_reasoning_delta', { step: 1, delta: 'Compare the alternatives. ' })
  const call = { id: 'read', name: 'read_file', arguments: { path: 'example.md' } }
  response(1, 'Checking the alternatives.', [call])
  add('tool_call_started', { call })
  add('tool_call_finished', { call_id: call.id, name: call.name, output: { content: 'Both are available.', is_error: false } })
  response(2, beforeQuestion, [{ id: 'ask', name: 'ask_user', arguments: {} }])
  add('user_question_asked', { question: { id: 'direction', question: 'Which approach should we take?', options: [{ label: 'Compare' }, { label: 'Implement' }] } })
  add('user_question_answered', { answer: { question_id: 'direction', selected: [], custom: customAnswer } })
  response(3, finalAnswer)
  add('turn_failed', { message: 'Synthetic history ended after output.' })
  return events
}

test('Local and Server retain question replies across background history, reload and cached reopening', { timeout: 180_000 }, async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-question-history-'))
  const processes = [], errors = []
  const environment = { XDG_STATE_HOME: path.join(directory, 'state') }
  let browser, node, page
  const releases = []
  try {
    const server = await initializeServer({ directory: path.join(directory, 'server'), origin: `http://127.0.0.1:${await freePort()}`, mode: 'single_user', environment })
    processes.push(server)
    const tenantId = server.owner.session.personal_tenant_id
    const owner = (resource, options = {}) => serverRequest(server.origin, resource, { token: server.owner.session.access_token, tenantId, ...options })
    const { enrollment } = await owner(`/tenants/${tenantId}/my-computer-enrollments`, { body: { name: 'Question history', project_id: null, ttl_seconds: 600 } })
    const credential = await owner('/enrollments/consume', { body: { token: enrollment.token } })
    const origin = `http://127.0.0.1:${await freePort()}`, data = path.join(directory, 'data')
    const binary = process.env.TERNILO_E2E_NODE_BINARY ?? path.join(repository, 'target/debug/ternilo')
    const args = ['serve', '--listen', new URL(origin).host, '--data-dir', data, '--node-id', enrollment.executor_id, '--gateway-url', `${server.origin.replace('http:', 'ws:')}/api/v1/executors/connect`, '--allow-insecure-gateway']
    const env = { ...environment, TERNILO_LOCAL_TOKEN: credential.credential.token }
    node = startProcess(binary, args, env); processes.push(node); await waitForHttp(origin, node)
    let local = await localApi(origin)
    const folder = path.join(directory, 'workspace'); await mkdir(folder)
    const workspace = await local('/workspaces', { body: { path: folder } })
    const session = await local('/sessions', { body: { workspace_id: workspace.workspace_id } })
    const sessionId = session.identity.session_id
    const state = await until(() => owner('/state'), value => value.sessions.length === 1, 'mapped question session')
    const serverId = state.sessions[0].identity.session_id
    await stopProcess(node)
    const expected = questionHistory()
    const file = path.join(data, 'data', 'sessions', `${Buffer.from(sessionId).toString('hex')}.jsonl`)
    await mkdir(path.dirname(file), { recursive: true })
    await writeFile(file, expected.map(event => JSON.stringify(event)).join('\n') + '\n')
    node = startProcess(binary, args, env); processes.push(node); await waitForHttp(origin, node)
    local = await localApi(origin)
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
    for (const [surface, target, id] of [['local', origin, sessionId], ['server', server.origin, serverId]]) {
      const context = await browser.newContext({ locale: 'zh-CN', viewport: { width: surface === 'local' ? 1280 : 390, height: 844 }, serviceWorkers: 'block' })
      await context.addInitScript(id => localStorage.setItem('ternilo.current-session', id), id)
      page = await context.newPage()
      page.on('pageerror', error => errors.push(error.message))
      page.on('console', message => { if (message.type() === 'error') errors.push(message.text()) })
      page.on('response', response => { if (response.status() >= 400) errors.push(`HTTP ${response.status()} ${new URL(response.url()).pathname}`) })
      let release
      let gate
      let historyRequests = 0
      const resetGate = () => { gate = new Promise(resolve => { release = resolve; releases.push(resolve) }) }
      resetGate()
      await page.route(`**/sessions/${id}/history?*`, async route => {
        historyRequests += 1
        if (new URL(route.request().url()).searchParams.has('before_seq')) await gate
        await route.continue().catch(() => {})
      })
      const assertConversation = async () => {
        assert.equal(await page.getByText(beforeQuestion, { exact: true }).isVisible(), true)
        assert.equal(await page.locator('[data-question-id="direction"]').isVisible(), true)
        assert.equal(await page.locator('[data-question-answer]').textContent(), `回答${customAnswer}`)
        assert.equal(await page.getByText(finalAnswer, { exact: true }).isVisible(), true)
      }
      const selectSession = async selectedId => {
        if (await page.locator('[data-app-frame][data-mobile]:not([data-mobile-sidebar-open])').count()) {
          await page.getByRole('button', { name: '打开侧边栏', exact: true }).click()
        }
        const button = page.locator(`[data-session-id="${selectedId}"] [data-sidebar-session-button]`)
        await button.click()
      }
      await page.goto(target)
      if (surface === 'server') {
        await page.getByLabel('用户名', { exact: true }).fill(server.owner.username)
        await page.getByLabel('密码', { exact: true }).fill(server.owner.password)
        await page.getByRole('button', { name: '登录', exact: true }).click()
        await selectSession(id)
      }
      for (let load = 0; load < 2; load++) {
        console.log(`${surface}: checking ${load ? 'reloaded' : 'initial'} question history`)
        if (load) { resetGate(); await page.reload() }
        await page.locator('[data-history-state="backfilling"]').waitFor()
        await assertConversation()
        release()
        await page.locator('[data-history-state="backfilling"]').waitFor({ state: 'detached' })
        await assertConversation()
        assert.equal(await page.locator('[data-turn-process]').getAttribute('aria-expanded'), 'false')
      }
      if (process.env.TERNILO_E2E_ARTIFACT_DIR) {
        await mkdir(process.env.TERNILO_E2E_ARTIFACT_DIR, { recursive: true })
        await page.getByText(beforeQuestion, { exact: true }).scrollIntoViewIfNeeded()
        await page.screenshot({ path: path.join(process.env.TERNILO_E2E_ARTIFACT_DIR, `question-history-${surface}.png`) })
      }
      const other = await local('/sessions', { body: { workspace_id: workspace.workspace_id } })
      const otherState = await until(() => owner('/state'), value => value.sessions.length >= 2, 'second mapped session')
      const otherId = surface === 'local' ? other.identity.session_id : otherState.sessions.find(value => value.identity.session_id !== serverId).identity.session_id
      const beforeSwitch = historyRequests
      await selectSession(otherId)
      await page.getByText(finalAnswer, { exact: true }).waitFor({ state: 'detached' })
      await selectSession(id)
      await page.getByText(finalAnswer, { exact: true }).waitFor()
      await assertConversation()
      assert.equal(historyRequests, beforeSwitch, 'Cached reopening must retain question history without another REST read')
      await context.close()
    }
    assert.deepEqual(errors, [])
    assert.deepEqual((await local(`/sessions/${sessionId}/events`)).map(event => [event.seq, event.type]), expected.map(event => [event.seq, event.type]))
  } catch (error) {
    if (process.env.TERNILO_E2E_ARTIFACT_DIR && page && !page.isClosed()) {
      await page.screenshot({ path: path.join(process.env.TERNILO_E2E_ARTIFACT_DIR, 'question-history-failure.png') }).catch(() => {})
    }
    throw error
  } finally {
    releases.forEach(release => release())
    await browser?.close()
    for (const process of processes.reverse()) await stopProcess(process)
    await rm(directory, { recursive: true, force: true })
  }
})
