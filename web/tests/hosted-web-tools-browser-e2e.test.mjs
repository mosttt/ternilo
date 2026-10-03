import assert from 'node:assert/strict'
import { createServer } from 'node:http'
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { chromium } from 'playwright'
import { freePort, initializeServer, repository, serverRequest, startProcess, stopProcess, waitForHttp } from './platform-e2e-fixture.mjs'
import { localApi, openModels, closeModels, approveConnection, until } from './model-device-fixture.mjs'

const searchResult = { type: 'web_search_tool_result', tool_use_id: 'srv-search', content: [{ type: 'web_search_result', url: 'https://example.com/source', title: 'Search source', encrypted_content: 'retain-search-content-exactly' }] }
const searchCitation = { type: 'web_search_result_location', url: 'https://example.com/source', title: 'Search source', encrypted_index: 'retain-citation-index', cited_text: 'Search evidence' }
const fetchResult = { type: 'web_fetch_tool_result', tool_use_id: 'srv-fetch', content: { type: 'web_fetch_result', url: 'https://example.com/document', content: { type: 'document', source: { type: 'text', media_type: 'text/plain', data: 'Fetched evidence' }, title: 'Fetched document', citations: { enabled: true } } } }
const fetchCitation = { type: 'char_location', document_index: 0, document_title: 'Fetched document', start_char_index: 0, end_char_index: 7, cited_text: 'Fetched' }

async function modelService() {
  const calls = [], failures = []
  const server = createServer(async (request, response) => {
    try {
      const chunks = []; for await (const chunk of request) chunks.push(chunk)
      const body = JSON.parse(Buffer.concat(chunks)); calls.push({ body, key: request.headers['x-api-key'] })
      assert.equal(request.url, '/v1/messages')
      assert.equal(request.headers['anthropic-version'], '2023-06-01')
      for (const [name, type] of [['web_search', 'web_search_20250305'], ['web_fetch', 'web_fetch_20250910']]) {
        const tools = body.tools.filter(tool => tool.name === name)
        assert.equal(tools.length, 1); assert.equal(tools[0].type, type); assert.equal(tools[0].max_uses, 2)
        assert.deepEqual(tools[0].allowed_domains, ['example.com'])
      }
      const blocks = body.messages.flatMap(message => message.content)
      const continued = blocks.some(block => block.type === 'web_search_tool_result')
      const finished = blocks.some(block => block.type === 'tool_result')
      if (continued) assert.deepEqual(blocks.find(block => block.type === 'web_search_tool_result'), searchResult)
      if (finished) assert.deepEqual(blocks.find(block => block.type === 'web_fetch_tool_result'), fetchResult)
      const output = finished ? [{ type: 'text', text: 'Hosted search and fetch completed with a local file tool.', citations: [searchCitation, fetchCitation] }]
        : continued ? [{ type: 'server_tool_use', id: 'srv-fetch', name: 'web_fetch', input: { url: 'https://example.com/document' } }, fetchResult, { type: 'tool_use', id: 'local-read', name: 'read_file', input: { path: 'fixture.txt' } }]
          : [{ type: 'server_tool_use', id: 'srv-search', name: 'web_search', input: { query: 'fixture evidence' } }, searchResult]
      const frames = [{ type: 'message_start', message: { id: `msg-${calls.length}`, type: 'message', model: body.model, role: 'assistant', content: [], usage: { input_tokens: 100, output_tokens: 0 } } }]
      output.forEach((block, index) => {
        const input = block.input, citations = block.citations
        frames.push({ type: 'content_block_start', index, content_block: { ...block, ...(input ? { input: {} } : {}), ...(citations ? { citations: [] } : {}) } })
        if (input) frames.push({ type: 'content_block_delta', index, delta: { type: 'input_json_delta', partial_json: JSON.stringify(input) } })
        for (const citation of citations ?? []) frames.push({ type: 'content_block_delta', index, delta: { type: 'citations_delta', citation } })
        frames.push({ type: 'content_block_stop', index })
      })
      frames.push({ type: 'message_delta', delta: { stop_reason: finished ? 'end_turn' : continued ? 'tool_use' : 'pause_turn' }, usage: { output_tokens: 20 } }, { type: 'message_stop' })
      response.writeHead(200, { 'content-type': 'text/event-stream' }).end(frames.map(frame => `event: ${frame.type}\ndata: ${JSON.stringify(frame)}\n\n`).join(''))
    } catch (error) { failures.push(error.message); response.writeHead(500).end('fixture rejected request') }
  })
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve))
  return { baseUrl: `http://127.0.0.1:${server.address().port}/v1`, calls, failures, close: () => new Promise(resolve => { server.closeAllConnections(); server.close(resolve) }) }
}

test('Claude hosted tools persist citations, resume pause_turn and mix with local tools through local and Server models', { timeout: 180000 }, async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-hosted-web-'))
  const artifacts = process.env.TERNILO_E2E_ARTIFACT_DIR ? path.join(process.env.TERNILO_E2E_ARTIFACT_DIR, 'hosted-web') : undefined
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
    const profile = { id: 'hosted', display_name: 'Hosted Claude', base_url: upstream.baseUrl, protocol: 'anthropic-messages', api_key_ref: 'HOSTED_KEY', defaults: { context_window: 32000, max_output_tokens: 2048 }, models: [{ id: 'fixture', settings: { mode: 'inherit' } }], timeout_ms: 10000, max_attempts: 1, retry_base_delay_ms: 25 }
    await local('/credentials', { body: { name: 'HOSTED_KEY', value: 'local-hosted-key' } })
    await local('/providers', { body: profile })
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
    page = await browser.newPage({ viewport: { width: 1440, height: 1000 }, serviceWorkers: 'block' })
    page.on('pageerror', error => errors.push(error.message))
    page.on('console', message => { if (message.type() === 'error') errors.push(message.text()) })
    page.on('response', response => { if (response.status() >= 400) errors.push(`HTTP ${response.status()} ${new URL(response.url()).pathname}`) })
    await page.goto(localOrigin)
    const settings = await openModels(page)
    await settings.getByRole('button', { name: '编辑', exact: true }).click()
    const editor = page.locator('[data-provider-editor="hosted"]')
    await editor.locator('summary').filter({ hasText: '自定义设置' }).click()
    await editor.getByLabel('托管网页搜索', { exact: true }).check()
    await editor.getByLabel('托管网页读取', { exact: true }).check()
    await editor.getByLabel('每次请求每种工具最多调用次数', { exact: true }).fill('2')
    await editor.getByLabel('域名（逗号分隔，留空不限制）', { exact: true }).fill('example.com')
    await editor.getByRole('button', { name: '保存', exact: true }).click()
    await closeModels(page)
    const saved = (await local('/providers')).find(provider => provider.id === 'hosted')
    assert.equal(saved.hosted_tools.web_search, true); assert.equal(saved.hosted_tools.web_fetch, true)
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
        await local(`/sessions/${session.identity.session_id}`, { method: 'PATCH', body: { title: 'Local hosted tools', model: { provider: 'named_provider', provider_id: 'hosted', model: 'fixture' } } })
        await page.evaluate(id => { localStorage.setItem('ternilo.current-session', id); localStorage.setItem('ternilo.transcript-view', 'normal') }, session.identity.session_id)
        await page.reload()
      } else {
        api = owner
        await until(() => owner('/model-computers'), rows => rows.some(row => row.executor_id === enrollment.executor_id && row.connected), 'Node connects')
        const title = `${scope} hosted tools`
        const { workspace } = await owner('/workspaces', { body: { project_id: identity.personal_project_id, name: title, placement: 'local_node', executor_id: enrollment.executor_id, path: folder } })
        session = await owner('/sessions', { body: { workspace_id: workspace.workspace_id } })
        const model = scope === 'server' ? { provider: 'account_provider', owner_user_id: identity.user.user_id, provider_id: 'hosted', model: 'fixture' }
          : { provider: 'computer_provider', executor_id: sourceEnrollment.executor_id, provider_id: 'hosted', model: 'fixture' }
        await owner(`/sessions/${session.identity.session_id}`, { method: 'PATCH', body: { title, model } })
        await page.goto(server.origin)
        if (scope === 'server') {
          await page.getByLabel('用户名', { exact: true }).fill(server.owner.username)
          await page.getByLabel('密码', { exact: true }).fill(server.owner.password)
          await page.getByRole('button', { name: '登录', exact: true }).click()
        }
        await page.locator('[data-sidebar-workspace-button]').filter({ hasText: title }).click()
        await page.locator(`[data-sidebar-session-row][data-session-id="${session.identity.session_id}"] [data-sidebar-session-button]`).click()
        await page.evaluate(() => localStorage.setItem('ternilo.transcript-view', 'normal')); await page.reload()
      }
      const id = session.identity.session_id, before = upstream.calls.length
      await page.getByRole('textbox', { name: '输入任务', exact: true }).fill('Search and fetch evidence, then read fixture.txt and answer.')
      await page.getByRole('button', { name: '发送', exact: true }).click()
      const events = await until(() => api(`/sessions/${id}/events`), events => events.some(event => ['turn_finished', 'turn_failed'].includes(event.type)), `${scope} hosted turn finishes`)
      assert.equal(events.some(event => event.type === 'turn_failed'), false, JSON.stringify({ events, failures: upstream.failures }))
      assert.equal(upstream.calls.length - before, 3)
      assert.ok(upstream.calls.slice(before).every(call => call.key === `${scope}-hosted-key`))
      assert.equal(events.filter(event => event.type === 'tool_call_finished').length, 1)
      assert.deepEqual(events.filter(event => event.type === 'assistant_message').map(event => event.step), [1, 2, 3])
      await page.getByText('Hosted search and fetch completed with a local file tool.', { exact: true }).waitFor()
      const sources = page.locator('[data-provider-sources]').last()
      assert.equal(await sources.getByRole('link', { name: 'Search source', exact: true }).getAttribute('href'), 'https://example.com/source')
      assert.equal(await sources.getByRole('link', { name: 'Fetched document', exact: true }).getAttribute('href'), 'https://example.com/document')
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
    const connected = await localApi(connectedOrigin), approval = await browser.newPage({ serviceWorkers: 'block', hasTouch: true })
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
    assert.equal(upstream.calls.length - beforeConnected, 3)
    assert.ok(upstream.calls.slice(beforeConnected).every(call => call.key === 'server-hosted-key'))
    await page.getByRole('link', { name: 'Fetched document', exact: true }).last().waitFor()
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
