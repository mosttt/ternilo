import assert from 'node:assert/strict'
import { createServer } from 'node:http'
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { chromium } from 'playwright'
import { freePort, repository, startProcess, stopProcess, waitForHttp } from './platform-e2e-fixture.mjs'
import { localApi, until } from './model-device-fixture.mjs'

test('search services resolve host credentials, render results and cancel in-flight requests', { timeout: 120000 }, async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-search-services-'))
  const artifacts = process.env.TERNILO_E2E_ARTIFACT_DIR ? path.join(process.env.TERNILO_E2E_ARTIFACT_DIR, 'search-services') : undefined
  if (artifacts) await mkdir(artifacts, { recursive: true })
  const calls = [], modelCalls = [], errors = [], key = 'fixture-search-secret'
  let provider = 'brave', mode = 'success', cancelled = false, app, browser, page
  const upstream = createServer(async (request, response) => {
    const chunks = []; for await (const chunk of request) chunks.push(chunk)
    const body = chunks.length ? JSON.parse(Buffer.concat(chunks)) : null
    if (request.url === '/v1/chat/completions') {
      modelCalls.push(body)
      const latest = body.messages.findLastIndex(message => message.role === 'user')
      const search = !body.messages.slice(latest + 1).some(message => message.role === 'tool')
      const delta = search ? { role: 'assistant', tool_calls: [{ index: 0, id: `search-${modelCalls.length}`, type: 'function', function: { name: 'web_search', arguments: JSON.stringify({ query: body.messages[latest].content, limit: 1, language: 'en' }) } }] } : { role: 'assistant', content: 'Search results ready.' }
      const frame = value => `data: ${JSON.stringify({ id: 'fixture', model: 'fixture', object: 'chat.completion.chunk', ...value })}\n\n`
      response.writeHead(200, { 'content-type': 'text/event-stream' }).end(frame({ choices: [{ index: 0, delta, finish_reason: null }] })
        + frame({ choices: [{ index: 0, delta: {}, finish_reason: search ? 'tool_calls' : 'stop' }] })
        + frame({ choices: [], usage: { prompt_tokens: 20, completion_tokens: 10, total_tokens: 30 } }) + 'data: [DONE]\n\n')
      return
    }
    calls.push({ provider, url: request.url, key: provider === 'brave' ? request.headers['x-subscription-token'] : request.headers.authorization, body })
    if (mode === 'slow') { response.on('close', () => { cancelled = true }); return }
    if (mode === 'denied') { response.writeHead(401).end(JSON.stringify({ error: `${key} private-provider-detail` })); return }
    const result = { title: `${provider} search result`, url: 'https://example.com/search-proof', description: 'Verified search snippet', content: 'Verified search snippet', engine: 'fixture' }
    const data = provider === 'brave' ? { type: 'search', web: { results: [result] } } : { results: [result] }
    response.writeHead(200, { 'content-type': 'application/json' }).end(JSON.stringify(data))
  })
  await new Promise(resolve => upstream.listen(0, '127.0.0.1', resolve))
  try {
    const origin = `http://127.0.0.1:${await freePort()}`
    app = startProcess(process.env.TERNILO_E2E_NODE_BINARY ?? path.join(repository, 'target/debug/ternilo'), ['serve', '--data-dir', directory, '--listen', new URL(origin).host])
    await waitForHttp(origin, app)
    const api = await localApi(origin)
    await mkdir(path.join(directory, 'workspace'))
    const workspace = await api('/workspaces', { body: { path: path.join(directory, 'workspace') } })
    await api('/credentials', { body: { name: 'SEARCH_KEY', value: key } })
    await api('/providers', { body: { id: 'search-model', display_name: 'Search fixture', base_url: `http://127.0.0.1:${upstream.address().port}/v1`, protocol: 'openai-chat-completions', api_key_ref: null,
      defaults: { context_window: 32000, max_output_tokens: 2048 }, models: [{ id: 'fixture', settings: { mode: 'inherit' } }], timeout_ms: 10000, max_attempts: 1, retry_base_delay_ms: 25 } })
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
    page = await browser.newPage({ locale: 'zh-CN', viewport: { width: 1440, height: 960 }, serviceWorkers: 'block' })
    page.on('pageerror', error => errors.push(error.message))
    page.on('console', message => { if (message.type() === 'error') errors.push(message.text()) })
    page.on('response', response => { if (response.status() >= 400) errors.push(`HTTP ${response.status()} ${new URL(response.url()).pathname}`) })
    await page.goto(origin)
    await page.evaluate(() => localStorage.setItem('ternilo.transcript-view', 'normal'))
    for (provider of ['brave', 'tavily', 'searxng']) {
      mode = 'success'
      const session = await api('/sessions', { body: { workspace_id: workspace.workspace_id, agent_preset: 'standard' } })
      const id = session.identity.session_id
      await api(`/sessions/${id}`, { method: 'PATCH', body: { title: `${provider} search`, model: { provider: 'named_provider', provider_id: 'search-model', model: 'fixture' }, profile_plugins: [
        { id: 'search', kind: `ternilo.web.search.${provider}`, enabled: true, config: { base_url: `http://127.0.0.1:${upstream.address().port}`, api_key_env: 'SEARCH_KEY', max_results: 3, timeout_ms: 30000 } },
      ] } })
      await page.evaluate(id => localStorage.setItem('ternilo.current-session', id), id)
      await page.reload()
      const input = page.getByRole('textbox', { name: '输入任务', exact: true })
      await input.fill('Search for fixture documentation')
      await page.getByRole('button', { name: '发送', exact: true }).click()
      const read = () => api(`/sessions/${id}/events`)
      let events = await until(read, events => events.some(event => event.type === 'turn_finished'), `${provider} tool loop`)
      const result = events.find(event => event.type === 'tool_call_finished' && event.name === 'web_search')
      assert.ok(result && !result.output.is_error, JSON.stringify(events))
      assert.equal(JSON.parse(result.output.content)[0].url, 'https://example.com/search-proof')
      assert.ok(!JSON.stringify(events).includes(key))
      await page.getByText('搜索网页', { exact: true }).first().click()
      await page.getByText('Verified search snippet', { exact: false }).first().waitFor()
      if (artifacts) await page.screenshot({ path: path.join(artifacts, `${provider}-results.png`) })
      assert.equal(calls.at(-1).key, provider === 'brave' ? key : `Bearer ${key}`)
      mode = 'denied'
      await input.fill('Search for denied fixture')
      await page.getByRole('button', { name: '发送', exact: true }).click()
      events = await until(read, events => events.filter(event => event.type === 'tool_call_finished' && event.name === 'web_search').length >= 2, 'safe provider error')
      assert.ok(JSON.stringify(events).includes('HTTP 401'))
      assert.ok(!JSON.stringify(events).includes(key) && !JSON.stringify(events).includes('private-provider-detail'))
      await until(read, events => events.filter(event => ['turn_finished', 'turn_failed'].includes(event.type)).length >= 2, 'error turn settled')
      mode = 'slow'; cancelled = false
      const previous = calls.length
      await input.fill('Search for slow fixture')
      await page.getByRole('button', { name: '发送', exact: true }).click()
      await until(async () => calls.length, value => value > previous, 'in-flight search')
      await page.getByRole('button', { name: '停止运行', exact: true }).click()
      await until(async () => cancelled, Boolean, 'upstream connection cancelled')
      await until(read, events => events.some(event => event.type === 'turn_cancelled'), 'cancelled turn')
      await page.setViewportSize({ width: 390, height: 844 })
      assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth))
      if (artifacts) await page.screenshot({ path: path.join(artifacts, `${provider}-mobile.png`) })
      await page.setViewportSize({ width: 1440, height: 960 })
    }
    assert.deepEqual(errors, [])
    assert.ok(modelCalls.some(call => call.messages.some(message => message.role === 'tool' && message.content.includes('Verified search snippet'))))
    if (artifacts) await writeFile(path.join(artifacts, 'result.json'), JSON.stringify({ status: 'passed', calls: calls.length, modelCalls: modelCalls.length, errors }))
  } catch (error) {
    if (artifacts && page) await page.screenshot({ path: path.join(artifacts, 'failure.png') }).catch(() => {})
    throw error
  } finally {
    await browser?.close()
    if (app) await stopProcess(app)
    upstream.closeAllConnections(); await new Promise(resolve => upstream.close(resolve))
    await rm(directory, { recursive: true, force: true })
  }
})
