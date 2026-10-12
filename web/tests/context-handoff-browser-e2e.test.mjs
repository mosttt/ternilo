import assert from 'node:assert/strict'
import { createServer } from 'node:http'
import { mkdir, mkdtemp, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { chromium } from 'playwright'
import { localApi, until } from './model-device-fixture.mjs'
import { freePort, repository, startProcess, stopProcess, waitForHttp } from './platform-e2e-fixture.mjs'

const handoff = {
  objective: 'Complete the multilingual handler / 完成处理逻辑 / 처리 로직 완료',
  constraints: ['Keep existing user files. Do not publish.'],
  decisions: ['Use the current provider and permission policy.'],
  completed: ['Inspected the handler.'],
  pending: ['Run the browser checks before delivery.'],
  files: ['src/handler.rs: the pending change is scoped to this file.'],
  verification: ['No tests have been run yet.'],
  risks: ['The browser result is still unknown.'],
}

async function modelFixture() {
  const requests = []
  const server = createServer(async (request, response) => {
    const chunks = []; for await (const chunk of request) chunks.push(chunk)
    const body = JSON.parse(Buffer.concat(chunks).toString()); requests.push(body)
    const system = String(body.messages.find(message => message.role === 'system')?.content)
    const content = system.startsWith('You compact an agent conversation') ? JSON.stringify(handoff)
      : system.startsWith('You name software-agent conversations') ? 'Structured continuation' : 'Turn complete.'
    const usage = { prompt_tokens: 100, completion_tokens: 20, total_tokens: 120 }
    if (!body.stream) {
      response.writeHead(200, { 'content-type': 'application/json' }).end(JSON.stringify({ id: 'handoff', model: body.model, choices: [{ index: 0, message: { role: 'assistant', content }, finish_reason: 'stop' }], usage })); return
    }
    const event = value => `data: ${JSON.stringify(value)}\n\n`
    response.writeHead(200, { 'content-type': 'text/event-stream' }).end(event({ choices: [{ index: 0, delta: { content }, finish_reason: null }] }) + event({ choices: [{ index: 0, delta: {}, finish_reason: 'stop' }] }) + event({ choices: [], usage }) + 'data: [DONE]\n\n')
  })
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve))
  return { requests, baseUrl: `http://127.0.0.1:${server.address().port}/v1`, close: () => new Promise(resolve => { server.closeAllConnections(); server.close(resolve) }) }
}

test('structured context survives restart, retains the original history and renders in three languages', { timeout: 180_000 }, async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-context-handoff-'))
  let app, browser, upstream
  const errors = []
  try {
    upstream = await modelFixture()
    const origin = `http://127.0.0.1:${await freePort()}`
    const args = ['serve', '--listen', new URL(origin).host, '--data-dir', path.join(directory, 'data')]
    const environment = { ...Object.fromEntries(Object.keys(process.env).filter(key => key.startsWith('TERNILO_')).map(key => [key, undefined])), XDG_STATE_HOME: path.join(directory, 'state') }
    const binary = process.env.TERNILO_E2E_NODE_BINARY ?? path.join(repository, 'target/debug/ternilo')
    app = startProcess(binary, args, environment); await waitForHttp(origin, app)
    let request = await localApi(origin)
    const provider = (id, contextWindow) => ({ id, display_name: id, base_url: upstream.baseUrl, protocol: 'openai-chat-completions', api_key_ref: null, defaults: { context_window: contextWindow, max_output_tokens: 64 }, models: [{ id: 'handoff-model', settings: { mode: 'inherit' } }], timeout_ms: 30_000, max_attempts: 1, retry_base_delay_ms: 10 })
    await request('/providers', { body: provider('large-context', 128_000) })
    await request('/providers', { body: provider('small-context', 256) })
    const folder = path.join(directory, 'workspace'); await mkdir(folder)
    const workspace = await request('/workspaces', { body: { path: folder } })
    const session = await request('/sessions', { body: { workspace_id: workspace.workspace_id } })
    const sessionId = session.identity.session_id
    const resource = `/sessions/${sessionId}`
    await request(resource, { method: 'PATCH', body: { model: { provider: 'named_provider', provider_id: 'large-context', model: 'handoff-model' }, profile_plugins: [{ id: 'context', kind: 'ternilo.context.compaction', enabled: true, config: { keep_recent_turns: 1, structured_summary: true } }] } })
    const run = async input => {
      const before = (await request(`${resource}/events`)).filter(event => event.type === 'turn_finished').length
      await request(`${resource}/queue`, { body: { content: { kind: 'prompt', input } } })
      await until(() => request(`${resource}/events`), events => events.filter(event => event.type === 'turn_finished').length > before, 'completed context turn')
      await until(() => request(`${resource}/queue`), value => !value.active_run_id, 'idle context queue')
    }
    await run(`Keep existing user files. Do not publish. Work on src/handler.rs. No tests have run. OLD_DETAIL_MARKER ${'Low-value detail. '.repeat(150)}`)
    await run('The remaining step is the browser check. Keep that step pending.')
    await request(resource, { method: 'PATCH', body: { model: { provider: 'named_provider', provider_id: 'small-context', model: 'handoff-model' } } })
    await run('Continue from the recorded requirements.')
    let events = await request(`${resource}/events`)
    const checkpoint = events.findLast(event => event.type === 'context_compacted')
    assert.ok(checkpoint); assert.deepEqual(JSON.parse(checkpoint.compaction.summary), handoff)
    assert.ok(events.some(event => event.type === 'user_message' && event.content.includes('OLD_DETAIL_MARKER')), 'compaction retains the original durable history')
    const last = upstream.requests.at(-1)
    const resumed = last.messages.find(message => String(message.content).includes('<earlier_conversation_summary'))
    assert.ok(resumed); assert.ok(resumed.content.includes(handoff.constraints[0])); assert.ok(resumed.content.includes(handoff.pending[0]))
    assert.ok(resumed.content.includes('## Verification')); assert.ok(!resumed.content.includes('OLD_DETAIL_MARKER'))
    await stopProcess(app)
    app = startProcess(binary, args, environment); await waitForHttp(origin, app); request = await localApi(origin)
    events = await request(`${resource}/events`)
    assert.equal(events.findLast(event => event.type === 'context_compacted').compaction.summary, checkpoint.compaction.summary)
    await run('Continue after restarting, preserving the pending verification.')
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
    for (const [locale, pending] of [['zh', '待办'], ['en', 'Pending'], ['ko', '남은 작업']]) {
      const context = await browser.newContext({ viewport: locale === 'ko' ? { width: 320, height: 560 } : { width: 1280, height: 900 }, hasTouch: true, serviceWorkers: 'block' })
      await context.addInitScript(({ locale, sessionId, workspaceId }) => {
        localStorage.setItem('ternilo.locale', locale); localStorage.setItem('ternilo.current-session', sessionId); localStorage.setItem('ternilo.current-workspace', workspaceId)
      }, { locale, sessionId, workspaceId: workspace.workspace_id })
      const page = await context.newPage()
      page.on('pageerror', error => errors.push(error.message))
      page.on('console', message => { if (message.type() === 'error') errors.push(message.text()) })
      page.on('response', response => { if (response.status() >= 400) errors.push(`${response.status()} ${response.url()}`) })
      await page.goto(origin)
      const compact = page.locator('[data-chat-event="compaction"]').last()
      await compact.waitFor({ state: 'attached' })
      const processControl = compact.locator('xpath=ancestor::*[@data-chat-turn][1]').locator('[data-turn-process]')
      if (await processControl.getAttribute('aria-expanded') === 'false') await processControl.click()
      await compact.locator('[data-disclosure-row]').click()
      const summary = compact.locator('[data-context-handoff]')
      await summary.waitFor()
      assert.ok((await summary.textContent()).includes(handoff.objective))
      const pendingSection = summary.locator('[data-handoff-section="pending"]')
      await pendingSection.getByRole('heading', { name: pending, exact: true }).waitFor()
      assert.ok((await pendingSection.textContent()).includes(handoff.pending[0]))
      await pendingSection.scrollIntoViewIfNeeded()
      const bounds = await pendingSection.boundingBox()
      assert.ok(bounds && bounds.x >= 0 && bounds.x + bounds.width <= page.viewportSize().width)
      if (process.env.TERNILO_E2E_ARTIFACT_DIR) {
        await mkdir(process.env.TERNILO_E2E_ARTIFACT_DIR, { recursive: true })
        await page.screenshot({ path: path.join(process.env.TERNILO_E2E_ARTIFACT_DIR, `context-handoff-${locale}.png`) })
      }
      await context.close()
    }
    assert.deepEqual(errors, [])
    console.log(JSON.stringify({ structuredCheckpoint: true, originalHistoryRetained: true, survivedRestart: true, locales: ['zh', 'en', 'ko'], errors }))
  } catch (error) { console.error(app?.diagnostics().slice(-5000)); throw error }
  finally { await browser?.close(); await stopProcess(app); await upstream?.close(); await rm(directory, { recursive: true, force: true }) }
})
