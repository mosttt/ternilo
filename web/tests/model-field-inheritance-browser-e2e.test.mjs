import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { createServer } from 'node:http'
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { chromium } from 'playwright'
import { selectChoice } from './browser-select-fixture.mjs'
import { freePort, initializeServer, repository, serverRequest, startProcess, stopProcess, waitForHttp } from './platform-e2e-fixture.mjs'
import { localApi, openModels, until } from './model-device-fixture.mjs'

async function checkDiscoverySearch(page, artifacts, scope) {
  const dialog = page.getByRole('dialog', { name: '选择要添加的模型' })
  const search = dialog.getByRole('searchbox')
  for (const width of [1366, 390, 320]) {
    await page.setViewportSize({ width, height: 900 })
    await search.fill(' CAPACITY MODEL ')
    assert.equal(await dialog.getByRole('checkbox').count(), 1)
    await dialog.getByRole('button', { name: '取消结果选择', exact: true }).click()
    await search.fill('disabled-')
    assert.equal(await dialog.getByRole('checkbox').isChecked(), true)
    await search.fill('missing-model')
    assert.equal(await dialog.getByRole('checkbox').count(), 0)
    assert.equal(await dialog.getByRole('button', { name: '全选结果', exact: true }).isDisabled(), true)
    await search.fill('CAPACITY-')
    assert.equal(await dialog.getByRole('checkbox').isChecked(), false)
    await dialog.getByRole('button', { name: '全选结果', exact: true }).click()
    await search.press('Enter')
    assert.equal(await dialog.isVisible(), true)
    assert.ok(await dialog.evaluate(element => element.getBoundingClientRect().right <= innerWidth && element.getBoundingClientRect().left >= 0))
    await page.screenshot({ path: path.join(artifacts, `${scope}-search-${width}.png`) })
    await search.fill('')
    assert.equal(await dialog.getByRole('checkbox').count(), 2)
    assert.equal(await dialog.getByRole('checkbox').evaluateAll(elements => elements.every(element => element.checked)), true)
  }
  await page.setViewportSize({ width: 1366, height: 900 })
}

async function upstream() {
  let capacity = 1048576
  const calls = []
  const server = createServer(async (request, response) => {
    if (request.url === '/v1/models') {
      response.writeHead(200, { 'content-type': 'application/json' }).end(JSON.stringify({ data: [
        { id: 'capacity-model', name: 'Capacity model', context_window: capacity, max_output_tokens: 393216 },
        { id: 'disabled-model', name: 'Disabled model', reasoning: false },
      ] }))
      return
    }
    if (request.url !== '/v1/chat/completions') { response.writeHead(404).end(); return }
    const chunks = []
    for await (const chunk of request) chunks.push(chunk)
    const body = JSON.parse(Buffer.concat(chunks).toString())
    calls.push(body)
    const usage = { prompt_tokens: 30, completion_tokens: 12, total_tokens: 42 }
    const message = { role: 'assistant', content: 'Field inheritance verified.' }
    if (!body.stream) { response.writeHead(200, { 'content-type': 'application/json' }).end(JSON.stringify({ id: 'title', model: body.model, choices: [{ index: 0, message, finish_reason: 'stop' }], usage })); return }
    const event = value => `data: ${JSON.stringify({ id: 'fields', model: body.model, ...value })}\n\n`
    response.writeHead(200, { 'content-type': 'text/event-stream' }).end(event({ choices: [{ index: 0, delta: message, finish_reason: null }] }) + event({ choices: [{ index: 0, delta: {}, finish_reason: 'stop' }], usage }) + 'data: [DONE]\n\n')
  })
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve))
  return { calls, baseUrl: `http://127.0.0.1:${server.address().port}/v1`, update: () => { capacity = 2000000 }, close: () => new Promise(resolve => { server.closeAllConnections(); server.close(resolve) }) }
}

test('account and platform discovery keep upstream capacities, inherit absent reasoning, and preserve manual values on refresh', { timeout: 240000 }, async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-model-fields-'))
  const artifacts = process.env.TERNILO_E2E_ARTIFACT_DIR ?? directory
  await mkdir(artifacts, { recursive: true })
  const processes = [], errors = []
  const source = await upstream()
  let browser, page
  try {
    const server = await initializeServer({ directory: path.join(directory, 'server'), origin: `http://127.0.0.1:${await freePort()}`, mode: 'single_user', environment: { XDG_STATE_HOME: path.join(directory, 'state') } })
    processes.push(server)
    const tenantId = server.owner.session.personal_tenant_id
    const request = (resource, options = {}) => serverRequest(server.origin, resource, { token: server.owner.session.access_token, tenantId, ...options })
    const enrolled = await request(`/tenants/${tenantId}/my-computer-enrollments`, { body: { executor_id: 'fields', project_id: null, ttl_seconds: 600 } })
    const credential = await request('/enrollments/consume', { body: { token: enrolled.enrollment.token } })
    const origin = `http://127.0.0.1:${await freePort()}`
    const node = startProcess(process.env.TERNILO_E2E_NODE_BINARY ?? path.join(repository, 'target/debug/ternilo'), ['serve', '--listen', new URL(origin).host, '--data-dir', path.join(directory, 'node'), '--node-id', 'fields', '--gateway-url', `${server.origin.replace('http:', 'ws:')}/api/v1/executors/connect`, '--allow-insecure-gateway'], { TERNILO_LOCAL_TOKEN: credential.credential.token })
    processes.push(node)
    await waitForHttp(origin, node)
    const local = await localApi(origin)
    const folder = path.join(directory, 'workspace')
    await mkdir(folder)
    const workspace = await local('/workspaces', { body: { path: folder } })
    await local('/sessions', { body: { workspace_id: workspace.workspace_id } })
    const state = await until(() => request('/state'), value => value.sessions.length === 1, 'Node session registered')
    const sessionId = state.sessions[0].identity.session_id
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
    page = await browser.newPage({ viewport: { width: 1366, height: 900 }, hasTouch: true, serviceWorkers: 'block' })
    page.on('pageerror', error => errors.push(error.message))
    page.on('console', message => { if (message.type() === 'error') errors.push(message.text()) })
    page.on('response', response => { if (response.status() >= 400) errors.push(`${response.status()} ${new URL(response.url()).pathname}`) })
    await page.goto(`${server.origin}/models`)
    await page.getByLabel('用户名', { exact: true }).fill(server.owner.username)
    await page.getByLabel('密码', { exact: true }).fill(server.owner.password)
    await page.getByRole('button', { name: '登录', exact: true }).click()
    await page.locator('[data-provider-scope="account"]').waitFor()

    for (const scope of ['account', 'platform']) {
      const profile = { id: `${scope}-fields`, display_name: `${scope} fields`, base_url: source.baseUrl, protocol: 'openai-chat-completions', api_key_ref: null,
        defaults: { context_window: 128000, max_output_tokens: 16000, reasoning: { default_effort: 'max', efforts: { low: 'low', high: 'high', max: 'max' } } },
        models: [{ id: 'kept', settings: { mode: 'override', context_window: 64000, max_output_tokens: 4000 } }], timeout_ms: 0, max_attempts: 1, retry_base_delay_ms: 25 }
      const endpoint = scope === 'account' ? '/providers' : '/admin/models/providers'
      await request(endpoint, { body: scope === 'account' ? profile : { profile, enabled: true } })
      const read = async () => scope === 'account' ? (await request(endpoint)).find(value => value.id === profile.id) : (await request(endpoint)).providers.find(value => value.profile.id === profile.id).profile
      await page.goto(`${server.origin}${scope === 'account' ? '/models' : '/admin/models'}`)
      const openEditor = async () => {
        if (scope === 'platform') { await page.getByRole('tab', { name: '上游接入', exact: true }).click(); await page.locator(`[data-model-provider="${profile.id}"]`).getByRole('button', { name: '编辑', exact: true }).click() }
        else await page.locator('[data-provider-scope="account"]').getByRole('button', { name: '编辑', exact: true }).click()
        const editor = page.locator(`[data-provider-editor="${profile.id}"]`)
        await editor.locator('summary').filter({ hasText: '自定义设置' }).click()
        return editor
      }
      const discover = async editor => {
        await editor.getByRole('button', { name: '获取可用模型', exact: true }).click()
        await checkDiscoverySearch(page, artifacts, scope)
        await page.getByRole('dialog', { name: '选择要添加的模型' }).getByRole('button', { name: '应用所选', exact: true }).click()
      }
      let editor = await openEditor()
      await discover(editor)
      await editor.getByRole('button', { name: '模型详细设置 2', exact: true }).click()
      const fields = editor.locator('[data-model-settings="capacity-model"]')
      assert.equal(await fields.getByLabel('上下文窗口来源').innerText(), '自动 · 上游提供')
      assert.equal(await fields.getByLabel('推理强度', { exact: true }).innerText(), '自动 · Provider 默认')
      await fields.locator('[data-effective-reasoning]').filter({ hasText: '默认 max' }).waitFor()
      for (const width of [1366, 390, 320]) {
        await page.setViewportSize({ width, height: 900 })
        await fields.scrollIntoViewIfNeeded()
        assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth))
        await page.screenshot({ path: path.join(artifacts, `${scope}-${width}.png`) })
      }
      await editor.getByRole('button', { name: '模型详细设置 3', exact: true }).click()
      const disabled = editor.locator('[data-model-settings="disabled-model"]')
      assert.equal(await disabled.getByLabel('推理强度', { exact: true }).innerText(), '自动 · 上游提供')
      await disabled.locator('[data-effective-reasoning]').filter({ hasText: '未启用' }).waitFor()
      await editor.getByRole('button', { name: '保存', exact: true }).click()
      await until(read, value => value.models.length === 3, 'discovered models saved')
      const saved = await read()
      assert.deepEqual(saved.models[1].settings.overrides, {})
      assert.equal(saved.models[1].settings.upstream.max_output_tokens, 393216)
      let selection
      if (scope === 'account') selection = { provider: 'account_provider', owner_user_id: server.owner.session.user.user_id, provider_id: profile.id, model: 'capacity-model' }
      else {
        await request('/admin/models/publications', { body: { model_id: 'fields-model', display_name: 'Fields model', provider_id: profile.id, upstream_model: 'capacity-model', enabled: true } })
        const grant = await request('/admin/models/grants', { body: { name: 'Fields budget', subject: { kind: 'user', id: server.owner.session.user.user_id }, model_ids: ['fields-model'], monthly_tokens: 2000000, max_concurrent_requests: 2, allow_resource_sharing: false } })
        selection = { provider: 'platform_model', model_id: 'fields-model', grant_id: grant.grant_id }
      }
      await request(`/sessions/${sessionId}`, { method: 'PATCH', body: { model: selection } })
      const options = await request(`/model-options?session_id=${sessionId}`)
      assert.equal(options.current.model.defaults.max_output_tokens, 393216)
      assert.equal(options.current.selectable_reasoning.default_effort, 'max')
      await page.setViewportSize({ width: 1366, height: 900 })
      await page.goto(server.origin)
      const row = page.locator(`[data-sidebar-session-row][data-session-id="${sessionId}"]`)
      await row.waitFor()
      if (!await row.isVisible()) await page.locator('[data-sidebar-workspace-button]').first().click()
      await row.click()
      await page.locator('[data-input-bar] [data-model-picker]').click()
      await page.getByRole('menuitem', { name: /^推理强度/ }).click()
      await page.getByRole('menuitem', { name: 'max', exact: true }).waitFor()
      await page.screenshot({ path: path.join(artifacts, `${scope}-reasoning-menu.png`) })
      await page.getByRole('menuitem', { name: 'high', exact: true }).click()
      await until(() => request(`/model-options?session_id=${sessionId}`), value => value.current.selection.reasoning_effort === 'high', 'inherited reasoning can be selected in the conversation')
      const before = source.calls.length
      const accepted = await request(`/sessions/${sessionId}/queue`, { body: { content: { kind: 'prompt', input: `${scope} inheritance proof` } } })
      const events = await until(() => request(`/sessions/${sessionId}/events`), values => values.some(event => event.run_id === accepted.run_id && ['turn_finished', 'turn_failed'].includes(event.type)), 'model task finishes')
      assert.equal(events.some(event => event.run_id === accepted.run_id && event.type === 'turn_failed'), false, JSON.stringify(events))
      const streamed = source.calls.slice(before).filter(call => call.stream)
      assert.ok(streamed.length > 0)
      assert.ok(streamed.every(call => call.reasoning_effort === 'high' && call.max_tokens === 393216), JSON.stringify(streamed.map(call => ({ reasoning: call.reasoning_effort, output: call.max_tokens }))))

      await page.setViewportSize({ width: 1366, height: 900 })
      await page.goto(`${server.origin}${scope === 'account' ? '/models' : '/admin/models'}`)
      editor = await openEditor()
      await editor.getByRole('button', { name: '模型详细设置 2', exact: true }).click()
      await selectChoice(editor.locator('[data-model-settings="capacity-model"]').getByLabel('上下文窗口来源'), 'custom')
      await editor.locator('[id$="-model-1-context"]').fill('256K')
      await selectChoice(editor.locator('[id$="-model-1-reasoning-source"]'), 'disabled')
      source.update()
      await discover(editor)
      assert.equal(await editor.locator('[id$="-model-1-context"]').inputValue(), '256K')
      assert.equal(await editor.locator('[id$="-model-1-reasoning-source"]').getAttribute('data-choice-value'), 'disabled')
      await editor.getByRole('button', { name: '保存', exact: true }).click()
      const updated = await until(read, value => value.models[1].settings.upstream.context_window === 2000000 && value.models[1].settings.overrides.context_window === 256000, 'upstream refresh preserves manual values')
      assert.deepEqual(updated.models[1].settings.overrides.reasoning, { mode: 'disabled' })
      assert.equal(updated.models[0].settings.overrides.context_window, 64000)
      editor = await openEditor()
      await editor.getByRole('button', { name: '模型详细设置 2', exact: true }).click()
      await selectChoice(editor.locator('[data-model-settings="capacity-model"]').getByLabel('上下文窗口来源'), 'automatic')
      await selectChoice(editor.locator('[id$="-model-1-reasoning-source"]'), 'automatic')
      assert.equal(await editor.locator('[id$="-model-1-context"]').inputValue(), '2M')
      await editor.locator('[data-model-settings="capacity-model"] [data-effective-reasoning]').filter({ hasText: '默认 max' }).waitFor()
      await editor.getByRole('button', { name: '保存', exact: true }).click()
      await until(read, value => Object.keys(value.models[1].settings.overrides).length === 0, 'automatic restored independently')
    }
    await local('/providers', { body: {
      id: 'local-search', display_name: 'Local search', base_url: source.baseUrl, protocol: 'openai-chat-completions', api_key_ref: null,
      defaults: { context_window: 128000, max_output_tokens: 16000 },
      models: [{ id: 'kept', settings: { mode: 'inherit' } }], timeout_ms: 0, max_attempts: 1, retry_base_delay_ms: 25,
    } })
    await page.goto(origin)
    const settings = await openModels(page)
    await settings.getByRole('button', { name: '编辑', exact: true }).click()
    const editor = settings.locator('[data-provider-editor="local-search"]')
    await editor.locator('summary').filter({ hasText: '自定义设置' }).click()
    await editor.getByRole('button', { name: '获取可用模型', exact: true }).click()
    await checkDiscoverySearch(page, artifacts, 'local')
    await page.getByRole('dialog', { name: '选择要添加的模型' }).getByRole('button', { name: '应用所选', exact: true }).click()
    await editor.getByRole('button', { name: '保存', exact: true }).click()
    await until(() => local('/providers'), values => values.find(value => value.id === 'local-search')?.models.length === 3, 'local discovered models persist')
    for (const target of [origin, server.origin]) {
      const actual = await (await fetch(`${target}/assets/app.js`)).arrayBuffer()
      const expected = await readFile(path.join(repository, 'web/dist/assets/app.js'))
      assert.equal(createHash('sha256').update(Buffer.from(actual)).digest('hex'), createHash('sha256').update(expected).digest('hex'))
    }
    assert.deepEqual(errors, [])
  } catch (error) {
    await page?.screenshot({ path: path.join(artifacts, 'failure.png') }).catch(() => {})
    error.message += `\n${processes.map(process => process.diagnostics()).join('\n')}`
    throw error
  } finally {
    await writeFile(path.join(artifacts, 'processes.log'), processes.map(process => process.diagnostics()).join('\n'))
    await browser?.close()
    for (const process of processes.reverse()) await stopProcess(process)
    await source.close()
    await rm(directory, { recursive: true, force: true })
  }
})
