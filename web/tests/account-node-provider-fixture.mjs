import assert from 'node:assert/strict'
import { createServer } from 'node:http'
import { readFile } from 'node:fs/promises'
import path from 'node:path'
import { until } from './model-device-fixture.mjs'

export async function upstream(proof, key, answer = `${proof} finished this task.`) {
  const calls = []
  let pause
  const server = createServer(async (request, response) => {
    if (request.headers.authorization !== `Bearer ${key}`) { response.writeHead(401).end(); return }
    if (request.url === '/v1/models') { response.writeHead(200, { 'content-type': 'application/json' }).end(JSON.stringify({ data: [{ id: 'same-model' }, { id: 'discovered' }] })); return }
    if (request.url !== '/v1/chat/completions') { response.writeHead(404).end(); return }
    const chunks = []; for await (const chunk of request) chunks.push(chunk)
    const body = JSON.parse(Buffer.concat(chunks).toString()); calls.push(body)
    if (pause?.matches(body)) { const current = pause; pause = undefined; current.started(); await current.wait }
    const latest = body.messages.findLastIndex(message => message.role === 'user')
    const write = body.tools?.some(tool => tool.function?.name === 'write_file') && !body.messages.slice(latest + 1).some(message => message.role === 'tool')
    const delta = write ? { role: 'assistant', tool_calls: [{ index: 0, id: `write-${calls.length}`, type: 'function', function: { name: 'write_file', arguments: JSON.stringify({ path: 'provider-source.txt', content: proof }) } }] } : { role: 'assistant', content: answer }
    const usage = { prompt_tokens: 30, completion_tokens: 12, total_tokens: 42 }
    const payload = { id: `test-${calls.length}`, model: body.model, choices: [{ index: 0, message: { role: 'assistant', content: `${proof} task` }, finish_reason: 'stop' }], usage }
    if (!body.stream) { response.writeHead(200, { 'content-type': 'application/json' }).end(JSON.stringify(payload)); return }
    const event = value => `data: ${JSON.stringify({ id: payload.id, model: body.model, ...value })}\n\n`
    response.writeHead(200, { 'content-type': 'text/event-stream' }).end(event({ choices: [{ index: 0, delta, finish_reason: null }] }) + event({ choices: [{ index: 0, delta: {}, finish_reason: write ? 'tool_calls' : 'stop' }] }) + event({ choices: [], usage }) + 'data: [DONE]\n\n')
  })
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve))
  return { calls, baseUrl: `http://127.0.0.1:${server.address().port}/v1`, holdNext(matches) {
    let started, release
    const began = new Promise(resolve => { started = resolve })
    const wait = new Promise(resolve => { release = resolve })
    pause = { matches, started, wait }
    return { began, release }
  }, close: () => new Promise(resolve => { server.closeAllConnections(); server.close(resolve) }) }
}

export function profile(baseUrl) {
  return { id: 'same', display_name: '同名 Provider', base_url: baseUrl, protocol: 'openai-chat-completions', api_key_ref: 'SAME_PROVIDER_KEY', defaults: { context_window: 32000, max_output_tokens: 2048 }, models: [{ id: 'same-model', settings: { mode: 'inherit' } }], timeout_ms: 0, max_attempts: 1, retry_base_delay_ms: 25 }
}

export function reasoningProfile(baseUrl, id = 'same') {
  const provider = profile(baseUrl)
  return { ...provider, id, defaults: { ...provider.defaults, reasoning: { default_effort: 'medium', efforts: { low: 'low', medium: 'medium', high: 'high' } } } }
}

export async function chooseReasoning(page, effort) {
  await page.locator('[data-input-bar] [data-model-picker]').click()
  await page.getByRole('menuitem', { name: /^推理强度/ }).click()
  const [saved] = await Promise.all([
    page.waitForResponse(response => /\/api\/v1\/sessions\/[^/]+$/.test(new URL(response.url()).pathname) && response.request().method() === 'PATCH'),
    page.getByRole('menuitem', { name: effort, exact: true }).click(),
  ])
  assert.equal(saved.status(), 200)
  await until(() => page.getByRole('menu').count(), count => count === 0, 'reasoning menu closed')
}

export async function refreshReasoning({ page, owner, local, sessionId, nodeSessionId, configure, screenshot }) {
  const saved = (await local('/state')).sessions.find(session => session.identity.session_id === nodeSessionId).server_model
  assert.equal(saved.defaults.reasoning ?? null, null)
  await configure()
  const options = await owner(`/model-options?session_id=${sessionId}`)
  assert.equal(options.current.available, true)
  assert.equal(options.current.model.defaults.reasoning ?? null, null, 'accepted defaults do not change when the catalog changes')
  assert.equal(options.current.selectable_reasoning.efforts.high, 'high')
  assert.deepEqual((await local('/state')).sessions.find(session => session.identity.session_id === nodeSessionId).server_model, saved)
  await page.locator('[data-input-bar] [data-model-picker]').click()
  await page.getByRole('menuitem', { name: /^推理强度/ }).click()
  await page.getByRole('menuitem', { name: 'high', exact: true }).waitFor()
  if (screenshot) await page.screenshot({ path: screenshot })
  await page.getByRole('menuitem', { name: 'high', exact: true }).click()
  await until(() => owner(`/model-options?session_id=${sessionId}`), value => value.current?.selection.reasoning_effort === 'high', 'fresh reasoning level saved')
  assert.deepEqual((await local('/state')).sessions.find(session => session.identity.session_id === nodeSessionId).server_model.binding, saved.binding)
}

export async function settings(page) {
  await page.getByRole('button', { name: '我的模型', exact: true }).click()
  const dialog = page.locator('[data-model-access]')
  await dialog.waitFor()
  await dialog.locator('[data-provider-scope="account"] [data-models-state="ready"], [data-provider-scope="account"] [data-models-state="empty"]').waitFor()
  return dialog
}
export async function closeSettings(page) { await page.getByRole('link', { name: '返回工作台', exact: true }).click() }

export async function computerModels(page, tenantId, executorId) {
  await page.getByRole('button', { name: '设备本地', exact: true }).click()
  const space = page.getByRole('combobox', { name: '选择模型所在空间', exact: true })
  if (await space.getAttribute('data-source-value') !== tenantId) {
    await space.click()
    await page.locator(`[role="option"][data-source-id="${tenantId}"]`).click()
  }
  const computer = executorId ? page.locator(`[data-model-computer="${executorId}"]`) : page.locator('[data-model-computer]').first()
  await computer.waitFor()
  const toggle = computer.getByRole('button', { name: /^(查看|收起)模型$/ })
  if (await toggle.getAttribute('aria-expanded') !== 'true') await toggle.click()
  const source = page.locator('[data-provider-scope="node"]')
  await source.waitFor()
  return source
}

export async function choose(page, source, provider = 'same', model = 'same-model') {
  await page.locator('[data-input-bar] [data-model-picker]').click()
  await page.getByRole('menuitem', { name: /^模型/ }).click()
  const [saved] = await Promise.all([
    page.waitForResponse(response => /\/api\/v1\/sessions\/[^/]+$/.test(new URL(response.url()).pathname) && response.request().method() === 'PATCH', { timeout: 15000 }),
    page.locator(`[data-model-source="${source}"] [data-model-provider="${provider}"]`).getByRole('menuitem').filter({ hasText: model }).click({ timeout: 10000 }),
  ])
  assert.equal(saved.status(), 200, 'the selected source is committed before the next action')
  await until(() => page.getByRole('menu').count(), count => count === 0, 'model menu closed')
}
export async function task(page, request, session, folder, proof) {
  const previous = await request(`/sessions/${session}/events`)
  await page.getByRole('textbox', { name: '输入任务', exact: true }).fill(`Write a proof using ${proof}`)
  await page.getByRole('button', { name: '发送', exact: true }).click()
  await until(async () => (await request(`/sessions/${session}/events`)).slice(previous.length).filter(event => ['turn_finished', 'turn_failed'].includes(event.type)), events => events.length > 0, 'model task completion')
  const events = await request(`/sessions/${session}/events`)
  assert.equal(events.slice(previous.length).some(event => event.type === 'turn_failed'), false, JSON.stringify(events.slice(previous.length)))
  assert.equal(await readFile(path.join(folder, 'provider-source.txt'), 'utf8'), proof)
  await until(() => request(`/sessions/${session}/queue`), queue => !queue.active_run_id && !queue.items.some(item => item.placement === 'queued'), 'task queue idle')
}

export async function platformNodeTask({ page, owner, local, sessionId, nodeSessionId, folder, account, computer, artifacts }) {
  const originalDefault = await local('/default-model')
  const beforeComputer = computer.calls.length
  await page.locator('[data-input-bar] [data-model-picker]').click()
  await page.getByRole('menuitem', { name: /^模型/ }).click()
  await page.locator('[data-model-source="platform"]').getByRole('group', { name: '平台独立预算', exact: true }).getByRole('menuitem', { name: /平台同名模型/ }).click()
  await until(() => page.getByRole('menu').count(), count => count === 0, 'model menu closed')
  await until(() => owner(`/model-options?session_id=${sessionId}`), value => value.current?.selection.provider === 'platform_model' && value.current.available, 'Node platform model ready')
  const options = await owner(`/model-options?session_id=${sessionId}`)
  const grantId = options.current.selection.grant_id
  await page.setViewportSize({ width: 390, height: 844 })
  await refreshReasoning({ page, owner, local, sessionId, nodeSessionId,
    configure: () => owner('/admin/models/providers', { body: { profile: { ...reasoningProfile(account.baseUrl, 'platform-source'), api_key_ref: null }, enabled: true } }),
    screenshot: path.join(artifacts, 'platform-reasoning-refreshed-mobile.png'),
  })
  await page.setViewportSize({ width: 1366, height: 900 })
  assert.deepEqual(await local('/default-model'), originalDefault, 'a platform grant is per session, not the computer default')
  assert.equal((await local('/state')).sessions.find(session => session.identity.session_id === nodeSessionId).server_model.binding.grant_id, grantId)
  const beforeReasoning = account.calls.length
  await task(page, owner, sessionId, folder, 'account-source')
  const reasoningCalls = account.calls.slice(beforeReasoning).filter(call => call.stream)
  assert.ok(reasoningCalls.length > 0 && reasoningCalls.every(call => call.reasoning_effort === 'high'), 'selected platform reasoning reaches the upstream')
  assert.equal(computer.calls.length, beforeComputer)
  const usage = await owner('/model-access/requests?source=platform_grant&limit=50')
  assert.ok(usage.requests.some(request => request.origin === 'client_device' && request.grant_id === grantId && request.accounted_tokens > 0))
  const events = await owner(`/sessions/${sessionId}/events`)
  const input = events.findLast(event => event.type === 'user_message')
  assert.equal(input.provenance.run_id, input.run_id, 'Server accepted input is bound to the actual model run')
  const beforeRevocation = account.calls.length
  await owner(`/admin/models/grants/${grantId}`, { method: 'DELETE' })
  await page.reload()
  await page.getByText('当前模型不可用', { exact: true }).waitFor()
  const revoked = await owner(`/model-options?session_id=${sessionId}`)
  assert.equal(revoked.current.available, false)
  assert.equal(revoked.current.selectable_reasoning, null)
  assert.equal(revoked.current.selection.grant_id, grantId)
  assert.equal(account.calls.length, beforeRevocation, 'revocation never silently selects the same-name account Provider')
}
