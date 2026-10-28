import assert from 'node:assert/strict'
import { createServer } from 'node:http'
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { chromium } from 'playwright'
import { freePort, initializeServer, repository, serverRequest, startProcess, stopProcess, waitForHttp } from './platform-e2e-fixture.mjs'
import { localApi, openModels, closeModels, approveConnection, until } from './model-device-fixture.mjs'

const searchCall = { type: 'web_search_call', id: 'ws-search', status: 'completed', action: { type: 'search', query: 'fixture evidence' } }
const citation = { type: 'url_citation', url: 'https://example.com/source', title: 'Search source', start_index: 0, end_index: 8 }

async function modelService() {
  const calls = [], failures = []
  const server = createServer(async (request, response) => {
    try {
      const chunks = []; for await (const chunk of request) chunks.push(chunk)
      const body = JSON.parse(Buffer.concat(chunks)); calls.push({ body, key: request.headers.authorization?.replace(/^Bearer /, '') })
      assert.equal(request.url, '/v1/responses')
      assert.equal(body.model, 'deepseek-v4-pro')
      assert.equal(body.tools.filter(tool => tool.type === 'web_search').length, 1)
      assert.equal(body.tools.filter(tool => tool.name === 'web_search').length, 0)
      assert.equal(body.max_tool_calls, undefined)
      const finished = body.input.some(item => item.type === 'function_call_output')
      if (finished) assert.deepEqual(body.input.find(item => item.type === 'web_search_call'), searchCall)
      const output = finished
        ? [{ type: 'message', role: 'assistant', content: [{ type: 'output_text', text: 'DeepSeek search completed with a local file tool.', annotations: [citation] }] }]
        : [{ type: 'reasoning', summary: [], content: [{ type: 'reasoning_text', text: 'Search for evidence, then check the local file.' }] }, searchCall, { type: 'function_call', call_id: 'local-read', name: 'read_file', arguments: JSON.stringify({ path: 'fixture.txt' }) }]
      const frames = [{ type: 'response.created', response: { id: `resp-${calls.length}`, status: 'in_progress' } }]
      output.forEach((item, output_index) => {
        frames.push({ type: 'response.output_item.added', output_index, item })
        if (item.type === 'web_search_call') for (const state of ['in_progress', 'searching', 'completed']) frames.push({ type: `response.web_search_call.${state}`, output_index, item_id: item.id })
        if (item.type === 'message') frames.push({ type: 'response.output_text.delta', output_index, content_index: 0, delta: item.content[0].text })
        frames.push({ type: 'response.output_item.done', output_index, item })
      })
      frames.push({ type: 'response.completed', response: { id: `resp-${calls.length}`, status: 'completed', output, usage: { input_tokens: 100, output_tokens: 20 } } })
      response.writeHead(200, { 'content-type': 'text/event-stream' }).end(frames.map((frame, sequence_number) => `event: ${frame.type}\ndata: ${JSON.stringify({ ...frame, sequence_number })}\n\n`).join(''))
    } catch (error) { failures.push(error.message); response.writeHead(500).end('fixture rejected request') }
  })
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve))
  return { baseUrl: `http://127.0.0.1:${server.address().port}/v1`, calls, failures, close: () => new Promise(resolve => { server.closeAllConnections(); server.close(resolve) }) }
}

test('DeepSeek Responses search preserves sources and history across local, Server, computer and connected models', { timeout: 180000 }, async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-deepseek-hosted-'))
  const artifacts = process.env.TERNILO_E2E_ARTIFACT_DIR ? path.join(process.env.TERNILO_E2E_ARTIFACT_DIR, 'deepseek-hosted') : undefined
  if (artifacts) await mkdir(artifacts, { recursive: true })
  const upstream = await modelService(), processes = [], errors = []
  let browser, page
  try {
    const server = await initializeServer({ directory: path.join(directory, 'server'), origin: `http://127.0.0.1:${await freePort()}`, mode: 'single_user',
      databaseUrl: process.env.TERNILO_E2E_DATABASE_URL, migrationDatabaseUrl: process.env.TERNILO_E2E_MIGRATION_DATABASE_URL })
    processes.push(server)
    const identity = server.owner.session
    const owner = (resource, options = {}) => serverRequest(server.origin, resource, { token: identity.access_token, tenantId: identity.personal_tenant_id, ...options })
    const { enrollment } = await owner(`/tenants/${identity.personal_tenant_id}/my-computer-enrollments`, { body: { name: 'Hosted tools computer', ttl_seconds: 600 } })
    const { credential } = await owner('/enrollments/consume', { body: { token: enrollment.token } })
    const localOrigin = `http://127.0.0.1:${await freePort()}`
    const node = startProcess(process.env.TERNILO_E2E_NODE_BINARY ?? path.join(repository, 'target/debug/ternilo'), ['serve', '--data-dir', path.join(directory, 'local'), '--listen', new URL(localOrigin).host,
      '--node-id', enrollment.executor_id, '--gateway-url', `${server.origin.replace('http:', 'ws:')}/api/v1/executors/connect`, '--allow-insecure-gateway'], { TERNILO_LOCAL_TOKEN: credential.token })
    processes.push(node); await waitForHttp(localOrigin, node)
    const local = await localApi(localOrigin)
    const profile = { id: 'hosted', display_name: 'Hosted DeepSeek', base_url: upstream.baseUrl, protocol: 'deepseek-responses', api_key_ref: 'HOSTED_KEY', defaults: { context_window: 32000, max_output_tokens: 2048 }, models: [{ id: 'deepseek-v4-pro', settings: { mode: 'inherit' } }], timeout_ms: 10000, max_attempts: 1, retry_base_delay_ms: 25 }
    await local('/credentials', { body: { name: 'HOSTED_KEY', value: 'local-hosted-key' } })
    await local('/providers', { body: profile })
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
    page = await browser.newPage({ locale: 'zh-CN', viewport: { width: 1440, height: 1000 }, serviceWorkers: 'block' })
    page.on('pageerror', error => errors.push(error.message))
    page.on('console', message => { if (message.type() === 'error') errors.push(message.text()) })
    page.on('response', response => { if (response.status() >= 400) errors.push(`HTTP ${response.status()} ${new URL(response.url()).pathname}`) })
    await page.goto(localOrigin)
    const settings = await openModels(page)
    await settings.getByRole('button', { name: '编辑', exact: true }).click()
    const editor = page.locator('[data-provider-editor="hosted"]')
    await editor.locator('summary').filter({ hasText: '自定义设置' }).click()
    await editor.getByLabel('托管网页搜索', { exact: true }).check()
    assert.equal(await editor.getByLabel('托管网页读取', { exact: true }).count(), 0)
    assert.equal(await editor.getByLabel('每次请求每种工具最多调用次数', { exact: true }).count(), 0)
    await editor.getByRole('button', { name: '保存', exact: true }).click()
    await editor.waitFor({ state: 'hidden' })
    await closeModels(page)
    const saved = (await local('/providers')).find(provider => provider.id === 'hosted')
    assert.equal(saved.hosted_tools.web_search, true); assert.equal(saved.hosted_tools.web_fetch, false); assert.equal(saved.hosted_tools.max_uses, undefined)
    for (const [locale, text] of Object.entries({
      zh: { settings: '设置', models: '模型', edit: '编辑', save: '保存', title: 'DeepSeek 内置搜索' },
      en: { settings: 'Settings', models: 'Models', edit: 'Edit', save: 'Save', title: 'DeepSeek built-in search' },
      ko: { settings: '설정', models: '모델', edit: '편집', save: '저장', title: 'DeepSeek 내장 검색' },
    })) {
      await page.evaluate(locale => { localStorage.setItem('ternilo.locale', locale); localStorage.setItem('ternilo.theme', locale === 'en' ? 'light' : 'dark') }, locale); await page.reload()
      await page.getByRole('button', { name: text.settings, exact: true }).click()
      const surface = page.locator('[data-settings-surface]')
      await surface.getByRole('button', { name: text.models, exact: true }).click()
      await surface.getByRole('button', { name: text.edit, exact: true }).click()
      const current = surface.locator('[data-provider-editor="hosted"]')
      await current.locator('details > summary').first().click()
      const hosted = current.locator('[data-hosted-tools]')
      await hosted.getByText(text.title, { exact: true }).waitFor()
      assert.equal(await hosted.getByRole('checkbox').isChecked(), true)
      assert.equal(await hosted.getByRole('spinbutton').count(), 0)
      await page.setViewportSize({ width: 390, height: 844 }); await hosted.scrollIntoViewIfNeeded()
      assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth))
      if (artifacts) await page.screenshot({ path: path.join(artifacts, `settings-${locale}.png`) })
      await hosted.getByRole('checkbox').uncheck()
      await current.getByRole('button', { name: text.save, exact: true }).click(); await current.waitFor({ state: 'hidden' })
      assert.equal((await local('/providers')).find(item => item.id === 'hosted').hosted_tools, undefined)
      await page.setViewportSize({ width: 1440, height: 1000 })
      await surface.getByRole('button', { name: text.edit, exact: true }).click(); await current.locator('details > summary').first().click()
      await current.locator('[data-hosted-tools]').getByRole('checkbox').check()
      await current.getByRole('button', { name: text.save, exact: true }).click(); await current.waitFor({ state: 'hidden' })
      assert.equal((await local('/providers')).find(item => item.id === 'hosted').hosted_tools.web_search, true)
      await surface.locator('[data-settings-toolbar] > button').last().click()
    }
    await local('/providers', { body: { ...profile, models: [{ id: 'deepseek-flash', settings: { mode: 'inherit' } }] } })
    await page.evaluate(() => localStorage.setItem('ternilo.locale', 'zh')); await page.reload()
    const flashSettings = await openModels(page)
    await flashSettings.getByRole('button', { name: '编辑', exact: true }).click()
    const flashEditor = page.locator('[data-provider-editor="hosted"]')
    await flashEditor.locator('details > summary').first().click()
    assert.equal(await flashEditor.locator('[data-hosted-tools]').getByRole('checkbox').isEnabled(), false)
    await closeModels(page)
    await local('/providers', { body: saved })
    await owner('/credentials', { body: { name: 'HOSTED_KEY', value: 'server-hosted-key' } })
    await owner('/providers', { body: saved })
    const { enrollment: sourceEnrollment } = await owner(`/tenants/${identity.personal_tenant_id}/my-computer-enrollments`, { body: { name: 'Hosted source computer', ttl_seconds: 600 } })
    const { credential: sourceCredential } = await owner('/enrollments/consume', { body: { token: sourceEnrollment.token } })
    const sourceOrigin = `http://127.0.0.1:${await freePort()}`
    const sourceNode = startProcess(process.env.TERNILO_E2E_NODE_BINARY ?? path.join(repository, 'target/debug/ternilo'), ['serve', '--data-dir', path.join(directory, 'source'), '--listen', new URL(sourceOrigin).host,
      '--node-id', sourceEnrollment.executor_id, '--gateway-url', `${server.origin.replace('http:', 'ws:')}/api/v1/executors/connect`, '--allow-insecure-gateway'], { TERNILO_LOCAL_TOKEN: sourceCredential.token })
    processes.push(sourceNode); await waitForHttp(sourceOrigin, sourceNode)
    const source = await localApi(sourceOrigin)
    await source('/credentials', { body: { name: 'HOSTED_KEY', value: 'computer-hosted-key' } })
    await source('/providers', { body: saved })
    await until(() => owner('/model-computers'), rows => rows.some(row => row.executor_id === sourceEnrollment.executor_id && row.connected), 'model source connects')
    for (const scope of ['local', 'server', 'computer']) {
      const folder = path.join(directory, scope + '-workspace'); await mkdir(folder); await writeFile(path.join(folder, 'fixture.txt'), 'Local file evidence')
      let session, api
      if (scope === 'local') {
        api = local
        const workspace = await local('/workspaces', { body: { path: folder } })
        session = await local('/sessions', { body: { workspace_id: workspace.workspace_id } })
        await local(`/sessions/${session.identity.session_id}`, { method: 'PATCH', body: { title: 'Local hosted tools', model: { provider: 'named_provider', provider_id: 'hosted', model: 'deepseek-v4-pro' } } })
        await page.evaluate(id => { localStorage.setItem('ternilo.current-session', id); localStorage.setItem('ternilo.transcript-view', 'normal') }, session.identity.session_id)
        await page.reload()
      } else {
        api = owner
        await until(() => owner('/model-computers'), rows => rows.some(row => row.executor_id === enrollment.executor_id && row.connected), 'Node connects')
        const title = `${scope} hosted tools`
        const { workspace } = await owner('/workspaces', { body: { project_id: identity.personal_project_id, name: title, placement: 'local_node', executor_id: enrollment.executor_id, path: folder } })
        session = await owner('/sessions', { body: { workspace_id: workspace.workspace_id } })
        const model = scope === 'server' ? { provider: 'account_provider', owner_user_id: identity.user.user_id, provider_id: 'hosted', model: 'deepseek-v4-pro' }
          : { provider: 'computer_provider', executor_id: sourceEnrollment.executor_id, provider_id: 'hosted', model: 'deepseek-v4-pro' }
        await owner(`/sessions/${session.identity.session_id}`, { method: 'PATCH', body: { title, model } })
        await page.goto(server.origin)
        if (scope === 'server') {
          await page.getByLabel('用户名', { exact: true }).fill(server.owner.username)
          await page.getByLabel('密码', { exact: true }).fill(server.owner.password)
          await page.getByRole('button', { name: '登录', exact: true }).click()
        }
        const workspaceButton = page.locator('[data-sidebar-workspace-button]').filter({ hasText: title })
        await workspaceButton.waitFor()
        if (await workspaceButton.getAttribute('aria-expanded') !== 'true') await workspaceButton.click()
        await page.locator(`[data-sidebar-session-row][data-session-id="${session.identity.session_id}"] [data-sidebar-session-button]`).click()
        await page.evaluate(() => localStorage.setItem('ternilo.transcript-view', 'normal')); await page.reload()
      }
      const id = session.identity.session_id, before = upstream.calls.length
      await page.getByRole('textbox', { name: '输入任务', exact: true }).fill('Search and fetch evidence, then read fixture.txt and answer.')
      await page.getByRole('button', { name: '发送', exact: true }).click()
      const events = await until(() => api(`/sessions/${id}/events`), events => events.some(event => ['turn_finished', 'turn_failed'].includes(event.type)), `${scope} hosted turn finishes`)
      assert.equal(events.some(event => event.type === 'turn_failed'), false, JSON.stringify({ events, failures: upstream.failures }))
      assert.equal(upstream.calls.length - before, 2)
      assert.ok(upstream.calls.slice(before).every(call => call.key === `${scope}-hosted-key`))
      assert.equal(events.filter(event => event.type === 'tool_call_finished').length, 1)
      assert.deepEqual(events.filter(event => event.type === 'assistant_message').map(event => event.step), [1, 2])
      await page.getByText('DeepSeek search completed with a local file tool.', { exact: true }).waitFor()
      const sources = page.locator('[data-provider-sources]').last()
      assert.equal(await sources.getByRole('link', { name: 'Search source', exact: true }).getAttribute('href'), 'https://example.com/source')
      await page.reload(); await page.getByRole('link', { name: 'Search source', exact: true }).last().waitFor()
      await page.setViewportSize({ width: 390, height: 844 }); assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth))
      if (artifacts) await page.screenshot({ path: path.join(artifacts, `${scope}-sources.png`) })
      await page.setViewportSize({ width: 1440, height: 1000 })
    }
    assert.deepEqual(upstream.failures, []); assert.deepEqual(errors, [])
    assert.equal((await source('/state')).sessions.length, 0, 'model source never executes the Agent or file tools')
    const connectedOrigin = `http://127.0.0.1:${await freePort()}`
    const connectedNode = startProcess(process.env.TERNILO_E2E_NODE_BINARY ?? path.join(repository, 'target/debug/ternilo'), ['serve', '--data-dir', path.join(directory, 'connected'), '--listen', new URL(connectedOrigin).host])
    processes.push(connectedNode); await waitForHttp(connectedOrigin, connectedNode)
    const connected = await localApi(connectedOrigin), approval = await browser.newPage({ locale: 'zh-CN', serviceWorkers: 'block', hasTouch: true })
    approval.on('pageerror', error => errors.push(error.message))
    await page.goto(connectedOrigin)
    await approveConnection(page, approval, server, 'Hosted account connection', null, 'hosted')
    const connectedProviders = await connected('/providers')
    const connectedProfile = connectedProviders.find(provider => provider.base_url.includes('/device-account/'))
    assert.ok(connectedProfile)
    const connectedFolder = path.join(directory, 'connected-workspace'); await mkdir(connectedFolder); await writeFile(path.join(connectedFolder, 'fixture.txt'), 'Connected local evidence')
    const connectedWorkspace = await connected('/workspaces', { body: { path: connectedFolder } })
    const connectedSession = await connected('/sessions', { body: { workspace_id: connectedWorkspace.workspace_id } })
    const connectedId = connectedSession.identity.session_id
    await connected(`/sessions/${connectedId}`, { method: 'PATCH', body: { title: 'Connected hosted tools', model: { provider: 'named_provider', provider_id: connectedProfile.id, model: connectedProfile.models[0].id } } })
    await page.evaluate(id => { localStorage.setItem('ternilo.current-session', id); localStorage.setItem('ternilo.transcript-view', 'normal') }, connectedId); await page.reload()
    const beforeConnected = upstream.calls.length
    await page.getByRole('textbox', { name: '输入任务', exact: true }).fill('Search and fetch evidence, then read fixture.txt and answer.')
    await page.getByRole('button', { name: '发送', exact: true }).click()
    const connectedEvents = await until(() => connected(`/sessions/${connectedId}/events`), events => events.some(event => ['turn_finished', 'turn_failed'].includes(event.type)), 'connected hosted task')
    assert.equal(connectedEvents.some(event => event.type === 'turn_failed'), false, JSON.stringify({ connectedEvents, failures: upstream.failures }))
    assert.equal(upstream.calls.length - beforeConnected, 2)
    assert.ok(upstream.calls.slice(beforeConnected).every(call => call.key === 'server-hosted-key'))
    await page.getByRole('link', { name: 'Search source', exact: true }).last().waitFor()
    if (artifacts) await page.screenshot({ path: path.join(artifacts, 'connected-sources.png') })
    await approval.close()
    assert.deepEqual(upstream.failures, []); assert.deepEqual(errors, [])
    if (artifacts) await writeFile(path.join(artifacts, 'result.json'), JSON.stringify({ status: 'passed', calls: upstream.calls.length, errors }))
  } catch (error) {
    if (artifacts && page) await page.screenshot({ path: path.join(artifacts, 'failure.png') }).catch(() => {})
    throw error
  } finally {
    await browser?.close(); for (const process of processes.reverse()) await stopProcess(process)
    await upstream.close(); await rm(directory, { recursive: true, force: true })
  }
})
