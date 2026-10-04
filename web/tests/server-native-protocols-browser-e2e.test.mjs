import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { chromium } from 'playwright'
import { selectChoice } from './browser-select-fixture.mjs'
import { choose, chooseReasoning } from './account-node-provider-fixture.mjs'
import { localApi, until } from './model-device-fixture.mjs'
import { freePort, initializeServer, repository, serverRequest, startProcess, stopProcess, waitForHttp } from './platform-e2e-fixture.mjs'
import { nativeFixture, thinking, settleNativeViewport } from './native-model-fixture.mjs'

async function configureProvider({ page, server, request, scope, protocol, upstream, artifacts }) {
  await page.setViewportSize({ width: 1366, height: 900 })
  const providerId = `${scope}-native`
  await page.goto(`${server.origin}${scope === 'account' ? '/models' : '/admin/models'}`)
  if (scope === 'account') {
    await page.locator('[data-provider-scope="account"] header').getByRole('button', { name: '添加 Provider', exact: true }).click()
  } else {
    await page.getByRole('tab', { name: '上游接入', exact: true }).click()
    await page.getByRole('button', { name: '添加上游', exact: true }).click()
  }
  const editor = page.locator('[data-provider-editor="new"]')
  await editor.getByLabel('Provider ID', { exact: true }).fill(providerId)
  await editor.getByLabel('显示名称', { exact: true }).fill(`${scope} native Provider`)
  await selectChoice(editor.getByLabel('API 协议', { exact: true }), protocol)
  await editor.getByLabel('API 地址', { exact: true }).fill(upstream.baseUrl)
  await editor.getByLabel('API Key', { exact: true }).fill(upstream.apiKey)
  await editor.getByRole('switch', { name: '启用 Provider 默认模型设置 的推理强度' }).click()
  await selectChoice(editor.locator('[id$="-provider-defaults-default-effort"]'), 'medium')
  const discoveryPath = scope === 'account' ? '/api/v1/providers/discover' : '/api/v1/admin/models/providers/discover'
  const discovered = page.waitForResponse(response => new URL(response.url()).pathname === discoveryPath && response.request().method() === 'POST')
  await editor.getByRole('button', { name: '获取可用模型', exact: true }).click()
  const discovery = await discovered
  assert.equal(discovery.status(), 200)
  assert.equal(discovery.request().postDataJSON().protocol, protocol)
  const models = await discovery.json()
  assert.equal(models[0].id, 'native-fixture')
  assert.equal(models[0].settings.upstream.context_window, 64000)
  assert.equal(models[0].settings.upstream.max_output_tokens, 8192)
  await page.getByRole('dialog', { name: '选择要添加的模型' }).getByRole('button', { name: '应用所选', exact: true }).click()
  assert.equal(await editor.getByLabel('模型 ID 1', { exact: true }).inputValue(), 'native-fixture')
  for (const width of [1366, 390, 320]) {
    await page.setViewportSize({ width, height: 900 })
    await editor.getByLabel('API 协议', { exact: true }).scrollIntoViewIfNeeded()
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true)
    const bounds = await editor.boundingBox()
    assert.ok(bounds && bounds.x >= 0 && bounds.x + bounds.width <= width + 1)
    await editor.getByLabel('API 协议', { exact: true }).click()
    await page.screenshot({ path: path.join(artifacts, `${protocol}-${scope}-editor-${width}.png`), fullPage: true, mask: [editor.getByLabel('API Key', { exact: true })] })
    await page.keyboard.press('Escape')
  }
  const endpoint = scope === 'account' ? '/providers' : '/admin/models/providers'
  const saving = page.waitForResponse(response => new URL(response.url()).pathname === `/api/v1${endpoint}` && response.request().method() === 'POST')
  await editor.getByRole('button', { name: '添加 Provider', exact: true }).click()
  assert.equal((await saving).status(), 200)
  await editor.waitFor({ state: 'hidden' })
  const saved = scope === 'account'
    ? (await request(endpoint)).find(value => value.id === providerId)
    : (await request(endpoint)).providers.find(value => value.profile.id === providerId).profile
  assert.equal(saved.protocol, protocol)
  assert.equal(saved.defaults.reasoning.default_effort, 'medium')
  assert.equal(saved.models[0].settings.upstream.max_output_tokens, 8192)
  assert.equal(JSON.stringify(saved).includes(upstream.apiKey), false)
  assert.equal(upstream.discoveries.length, 1)
  return providerId
}

async function runNativeTask({ page, request, local, sessionId, nodeSessionId, protocol, scope, upstream, folder, ownerId, grantId }) {
  const previous = await request(`/sessions/${sessionId}/events`)
  const lastSeq = previous.at(-1)?.seq ?? -1
  const proof = `${protocol} ${scope} actual Node file`
  await writeFile(path.join(folder, 'fixture.txt'), proof)
  await page.getByRole('textbox', { name: '输入任务', exact: true }).fill(`请使用 ${scope} 模型读取 fixture.txt 并报告内容。`)
  await page.getByRole('button', { name: '发送', exact: true }).click()
  const events = await until(() => request(`/sessions/${sessionId}/events`), events => events.some(event => event.seq > lastSeq && ['turn_finished', 'turn_failed'].includes(event.type)), `${protocol} ${scope} Node task completes`)
  const turn = events.filter(event => event.seq > lastSeq)
  assert.equal(turn.some(event => event.type === 'turn_failed'), false, JSON.stringify({ turn, upstreamFailures: upstream.failures }))
  await page.getByText(upstream.answer, { exact: true }).waitFor()
  assert.equal(upstream.requests.length, 2, 'one real tool request and one result-bearing model continuation')
  assert.deepEqual(upstream.failures, [])
  assert.equal(upstream.signatureChecks.length, 1)
  assert.equal(upstream.signatureChecks[0].signature, upstream.signature)
  assert.ok(upstream.signatureChecks[0].toolResult.includes(proof))
  assert.ok(upstream.requests.every(body => protocol === 'google-gemini'
    ? body.generationConfig.thinkingConfig.thinkingLevel === 'high'
    : body.output_config.effort === 'high'))
  const input = turn.find(event => event.type === 'user_message')
  assert.ok(input?.run_id)
  const assistant = turn.filter(event => event.type === 'assistant_message' && event.run_id === input.run_id)
  assert.equal(assistant.length, 2)
  assert.equal(assistant[0].response.reasoning_content, thinking)
  assert.equal(assistant[0].response.provider_state.protocol, protocol)
  assert.equal(assistant[0].response.provider_state.model, 'native-fixture', 'opaque state remains bound to the actual upstream model across the public alias')
  assert.ok(JSON.stringify(assistant[0].response.provider_state.blocks).includes(upstream.signature))
  if (scope === 'platform') assert.equal(assistant[0].response.model, 'public-native', 'public response and opaque upstream state are distinct')
  const nodeEvents = await until(() => local(`/sessions/${nodeSessionId}/events`), values => values.some(event => event.type === 'turn_finished' && event.run_id === input.run_id), 'actual Node event store completes')
  const tool = nodeEvents.find(event => event.type === 'tool_call_finished' && event.run_id === input.run_id)
  assert.equal(tool?.name, 'read_file')
  assert.equal(tool.output.is_error, false)
  assert.ok(tool.output.content.includes(proof))
  const nodeAssistant = nodeEvents.find(event => event.type === 'assistant_message' && event.run_id === input.run_id && event.response.tool_calls.length)
  assert.deepEqual(nodeAssistant.response.provider_state, assistant[0].response.provider_state, 'Node and Server both persist the signed native state')
  const source = scope === 'account' ? 'user_provider' : 'platform_grant'
  const requestIds = assistant.map(event => event.response.provider_request_id)
  const ledger = await until(() => request(`/model-access/requests?source=${source}&limit=50`), value => requestIds.every(id => value.requests.some(record => record.request_id === id && record.state === 'completed')), 'both brokered native model calls settle')
  for (const requestId of requestIds) {
    const record = ledger.requests.find(record => record.request_id === requestId)
    assert.equal(record.protocol, protocol)
    assert.equal(record.origin, 'client_device')
    assert.equal(record.source, source)
    assert.equal(record.actor_user_id, ownerId)
    assert.equal(record.model_beneficiary_user_id, ownerId)
    assert.ok(record.accounted_tokens > 0)
    if (scope === 'platform') assert.equal(record.grant_id, grantId)
  }
  await until(() => request(`/sessions/${sessionId}/queue`), queue => !queue.active_run_id && !queue.items.some(item => item.placement === 'queued'), 'Node queue returns idle')
}

for (const protocol of ['google-gemini', 'anthropic-messages']) {
  test(`${protocol}: Server account and platform Providers broker real Node tool loops with signed history`, { timeout: 210_000 }, async context => {
    const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-server-native-'))
    const artifacts = process.env.TERNILO_E2E_ARTIFACT_DIR ?? directory
    await mkdir(artifacts, { recursive: true })
    const processes = [], sources = [], errors = [], network = []
    let browser, page
    try {
      const environment = { XDG_STATE_HOME: path.join(directory, 'state') }
      const server = await initializeServer({ directory: path.join(directory, 'server'), origin: `http://127.0.0.1:${await freePort()}`, mode: 'single_user', environment })
      processes.push(server)
      const tenantId = server.owner.session.personal_tenant_id
      const request = (resource, options = {}) => serverRequest(server.origin, resource, { token: server.owner.session.access_token, tenantId, ...options })
      const enrollment = await request(`/tenants/${tenantId}/my-computer-enrollments`, { body: { name: 'native-node', project_id: null, ttl_seconds: 600 } })
      const executorId = enrollment.enrollment.executor_id
      const credential = await request('/enrollments/consume', { body: { token: enrollment.enrollment.token } })
      const nodeOrigin = `http://127.0.0.1:${await freePort()}`
      const node = startProcess(process.env.TERNILO_E2E_NODE_BINARY ?? path.join(repository, 'target/debug/ternilo'), [
        'serve', '--listen', new URL(nodeOrigin).host, '--data-dir', path.join(directory, 'node'), '--node-id', executorId,
        '--gateway-url', `${server.origin.replace('http:', 'ws:')}/api/v1/executors/connect`, '--allow-insecure-gateway',
      ], { ...environment, TERNILO_LOCAL_TOKEN: credential.credential.token })
      processes.push(node)
      await waitForHttp(nodeOrigin, node)
      const local = await localApi(nodeOrigin)
      const folder = path.join(directory, 'workspace')
      await mkdir(folder)
      const workspace = await local('/workspaces', { body: { path: folder } })
      const session = await local('/sessions', { body: { workspace_id: workspace.workspace_id, agent_preset: 'standard' } })
      const nodeSessionId = session.identity.session_id
      const state = await until(() => request('/state'), value => value.sessions.length === 1, 'real native Node session registered')
      const sessionId = state.sessions[0].identity.session_id
      browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
      page = await browser.newPage({ locale: 'zh-CN', viewport: { width: 1366, height: 900 }, hasTouch: true, serviceWorkers: 'block' })
      page.on('pageerror', error => errors.push(error.message))
      page.on('console', message => { if (message.type() === 'error') errors.push(message.text()) })
      page.on('response', response => {
        const pathname = new URL(response.url()).pathname
        if (pathname.startsWith('/api/v1/')) network.push({ method: response.request().method(), path: pathname, status: response.status() })
        if (response.status() >= 400) errors.push(`${response.status()} ${pathname}`)
      })
      await page.goto(`${server.origin}/models`)
      await page.getByLabel('用户名', { exact: true }).fill(server.owner.username)
      await page.getByLabel('密码', { exact: true }).fill(server.owner.password)
      await page.getByRole('button', { name: '登录', exact: true }).click()
      await page.locator('[data-provider-scope="account"]').waitFor()
      for (const scope of ['account', 'platform']) {
        const upstream = await nativeFixture(protocol, { label: `${scope}-${protocol}`, apiKey: `${scope}-native-fixture-key`, answer: `${scope} 原生模型已读取 Node 文件。`, expectedContent: `${protocol} ${scope} actual Node file` })
        sources.push(upstream)
        const providerId = await configureProvider({ page, server, request, scope, protocol, upstream, artifacts })
        let grantId
        if (scope === 'platform') {
          await request('/admin/models/publications', { body: { model_id: 'public-native', display_name: 'Native platform model', provider_id: providerId, upstream_model: 'native-fixture', enabled: true } })
          const grant = await request('/admin/models/grants', { body: { name: 'Native budget', subject: { kind: 'user', id: server.owner.session.user.user_id }, model_ids: ['public-native'], monthly_tokens: 2_000_000, max_concurrent_requests: 2, allow_resource_sharing: false } })
          grantId = grant.grant_id
        }
        await page.setViewportSize({ width: 1366, height: 900 })
        await page.goto(server.origin)
        await page.locator(`[data-sidebar-session-row][data-session-id="${sessionId}"]`).click()
        if (scope === 'account') await choose(page, 'account', providerId, 'native-fixture')
        else {
          await page.locator('[data-input-bar] [data-model-picker]').click()
          await page.getByRole('menuitem', { name: /^模型/ }).click()
          await page.locator('[data-model-source="platform"]').getByRole('group', { name: 'Native budget', exact: true }).getByRole('menuitem', { name: /Native platform model/ }).click()
          await until(() => page.getByRole('menu').count(), count => count === 0, 'platform picker closes')
        }
        await until(() => request(`/model-options?session_id=${sessionId}`), value => value.current?.selection.provider === (scope === 'account' ? 'account_provider' : 'platform_model') && value.current.available, 'native Server source bound to Node')
        await page.setViewportSize({ width: scope === 'platform' ? 390 : 1366, height: 900 })
        await settleNativeViewport(page)
        await chooseReasoning(page, 'high')
        const bound = (await local('/state')).sessions.find(value => value.identity.session_id === nodeSessionId)
        assert.equal(bound.server_model.protocol, protocol)
        assert.equal(bound.server_model.reasoning_effort, 'high')
        assert.equal((await local('/providers')).length, 0, 'the Node has no direct native Provider or upstream credential')
        await runNativeTask({ page, request, local, sessionId, nodeSessionId, protocol, scope, upstream, folder, ownerId: server.owner.session.user.user_id, grantId })
        await page.reload()
        await page.getByText(upstream.answer, { exact: true }).waitFor()
        assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true)
        await page.screenshot({ path: path.join(artifacts, `${protocol}-${scope}-node-answer.png`), fullPage: true })
      }
      for (const origin of [server.origin, nodeOrigin]) for (const asset of ['app.js', 'app.css']) {
        const served = Buffer.from(await (await fetch(`${origin}/assets/${asset}`)).arrayBuffer())
        const built = await readFile(path.join(repository, 'web/dist/assets', asset))
        assert.equal(createHash('sha256').update(served).digest('hex'), createHash('sha256').update(built).digest('hex'))
      }
      assert.deepEqual(errors, [])
      context.diagnostic(`${protocol}: account + platform discovery/save, Server→Node model binding, actual read_file, native signature round trip, persisted Node/Server events and source-specific ledger passed`)
    } catch (error) {
      await page?.screenshot({ path: path.join(artifacts, `${protocol}-server-failure.png`), fullPage: true }).catch(() => {})
      error.message += `\nFixture failures: ${JSON.stringify(sources.map(source => source.failures))}\n${processes.map(process => process.diagnostics()).join('\n')}`
      throw error
    } finally {
      await writeFile(path.join(artifacts, `${protocol}-network.json`), JSON.stringify({ network, errors, upstream: sources.map(source => ({ discoveries: source.discoveries, signatureChecks: source.signatureChecks, requests: source.requests, failures: source.failures })) }, null, 2))
      await writeFile(path.join(artifacts, `${protocol}-processes.log`), processes.map(process => process.diagnostics()).join('\n'))
      await browser?.close()
      for (const process of processes.reverse()) await stopProcess(process)
      for (const source of sources) await source.close()
      await rm(directory, { recursive: true, force: true })
    }
  })
}
