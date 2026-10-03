import assert from 'node:assert/strict'
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { chromium } from 'playwright'
import { freePort, initializeServer, repository, serverRequest, startProcess, stopProcess, waitForHttp } from './platform-e2e-fixture.mjs'
import { localApi, until } from './model-device-fixture.mjs'
import { profile, task, upstream } from './account-node-provider-fixture.mjs'

test('Server forwards models through the source computer while tools stay on the execution computer', { timeout: 240000 }, async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-computer-models-'))
  const artifacts = process.env.TERNILO_E2E_ARTIFACT_DIR ? path.join(process.env.TERNILO_E2E_ARTIFACT_DIR, 'computer-model-forwarding') : undefined
  if (artifacts) await mkdir(artifacts, { recursive: true })
  const processes = [], models = [], errors = [], requests = []
  let browser, page, release
  const result = { status: 'running', checks: [], errors }
  try {
    const origin = `http://127.0.0.1:${await freePort()}`
    const cluster = process.env.TERNILO_E2E_CLUSTER === '1'
    const server = await initializeServer({ directory: path.join(directory, 'server'), origin,
      environment: cluster ? { TERNILO_SERVER_CLUSTER_URL: origin } : {},
      databaseUrl: process.env.TERNILO_E2E_DATABASE_URL, migrationDatabaseUrl: process.env.TERNILO_E2E_MIGRATION_DATABASE_URL, mode: 'single_user' })
    processes.push(server)
    let executionGateway = origin
    if (cluster) {
      executionGateway = `http://127.0.0.1:${await freePort()}`
      const config = JSON.parse(await readFile(server.configPath, 'utf8'))
      const peerDirectory = path.join(directory, 'peer')
      await mkdir(peerDirectory)
      await writeFile(path.join(peerDirectory, 'config.json'), JSON.stringify({ ...config, listen: new URL(executionGateway).host, cluster_url: executionGateway }), { mode: 0o600 })
      const peer = startProcess(process.env.TERNILO_E2E_SERVER_BINARY ?? path.join(repository, 'target/debug/ternilo-server'), ['serve', '--config-dir', peerDirectory])
      processes.push(peer)
      await waitForHttp(`${executionGateway}/readyz`, peer)
    }
    const identity = server.owner.session
    const owner = (resource, options = {}) => serverRequest(origin, resource, { token: identity.access_token, tenantId: identity.personal_tenant_id, ...options })
    const startComputer = async (name, suffix, key, gateway = origin) => {
      const { enrollment } = await owner(`/tenants/${identity.personal_tenant_id}/my-computer-enrollments`, { body: { name, ttl_seconds: 600 } })
      const { credential } = await owner('/enrollments/consume', { body: { token: enrollment.token } })
      const localOrigin = `http://127.0.0.1:${await freePort()}`
      const data = path.join(directory, suffix)
      const args = ['serve', '--listen', new URL(localOrigin).host, '--data-dir', data, '--node-id', enrollment.executor_id, '--gateway-url', `${gateway.replace('http:', 'ws:')}/api/v1/executors/connect`, '--allow-insecure-gateway']
      const environment = { TERNILO_LOCAL_TOKEN: credential.token }
      let nodeProcess = startProcess(process.env.TERNILO_E2E_NODE_BINARY ?? path.join(repository, 'target/debug/ternilo'), args, environment)
      processes.push(nodeProcess)
      await waitForHttp(localOrigin, nodeProcess)
      const local = await localApi(localOrigin)
      const model = await upstream(suffix, key); models.push(model)
      // Configure keys directly on their source computers, never via Server.
      await local('/credentials', { body: { name: 'SAME_PROVIDER_KEY', value: key } })
      await local('/providers', { body: profile(model.baseUrl) })
      await until(() => owner('/model-computers'), value => value.some(item => item.executor_id === enrollment.executor_id && item.connected), 'computer connects')
      return { id: enrollment.executor_id, data, local, model, stop: () => stopProcess(nodeProcess), async restart() {
        nodeProcess = startProcess(process.env.TERNILO_E2E_NODE_BINARY ?? path.join(repository, 'target/debug/ternilo'), args, environment)
        processes.push(nodeProcess); await waitForHttp(localOrigin, nodeProcess)
        await until(() => owner('/model-computers'), value => value.some(item => item.executor_id === enrollment.executor_id && item.connected), 'same computer reconnects')
      } }
    }
    const execution = await startComputer('执行电脑', 'execution', 'execution-only-fixture-key', executionGateway)
    const sourceKey = 'source-only-fixture-key'
    const source = await startComputer('模型电脑', 'source', sourceKey)
    const folder = path.join(directory, 'workspace'); await mkdir(folder)
    const { workspace } = await owner('/workspaces', { body: { project_id: identity.personal_project_id, name: '远程工作区', placement: 'local_node', executor_id: execution.id, path: folder } })
    const session = await owner('/sessions', { body: { workspace_id: workspace.workspace_id } })
    const id = session.identity.session_id
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
    page = await browser.newPage({ viewport: { width: 1440, height: 980 }, hasTouch: true, serviceWorkers: 'block' })
    page.on('pageerror', error => errors.push(error.message))
    page.on('console', message => { if (message.type() === 'error') errors.push(message.text()) })
    page.on('request', request => requests.push({ method: request.method(), url: new URL(request.url()) }))
    page.on('response', response => { if (response.status() >= 400) errors.push(`HTTP ${response.status()} ${new URL(response.url()).pathname}`) })
    await page.goto(origin)
    await page.getByLabel('用户名', { exact: true }).fill(server.owner.username)
    await page.getByLabel('密码', { exact: true }).fill(server.owner.password)
    await page.getByRole('button', { name: '登录', exact: true }).click()
    await page.locator(`[data-sidebar-session-row][data-session-id="${id}"] [data-sidebar-session-button]`).click()
    const providerRequests = () => requests.filter(row => row.url.pathname.endsWith('/providers') && row.url.searchParams.get('executor_id') === source.id)
    assert.equal(providerRequests().length, 0)
    await page.locator('[data-input-bar] [data-model-picker]').click()
    const modelMenu = page.getByRole('menuitem', { name: /^模型/ })
    await modelMenu.hover()
    await modelMenu.press('ArrowRight')
    const picker = page.locator('[data-computer-model-picker]')
    await picker.getByRole('menuitem', { name: '模型电脑', exact: true }).waitFor()
    assert.equal(providerRequests().length, 0, 'source catalogs wait until a computer is expanded')
    await picker.getByRole('menuitem', { name: '模型电脑', exact: true }).click()
    const choice = picker.locator('[data-model-source="computer"]').getByRole('menuitem').filter({ hasText: 'same-model' })
    await choice.waitFor()
    assert.equal(providerRequests().length, 1)
    if (artifacts) await page.screenshot({ path: path.join(artifacts, 'computer-model-picker.png') })
    const changed = page.waitForResponse(response => response.request().method() === 'PATCH' && new URL(response.url()).pathname.endsWith(`/sessions/${id}`))
    await choice.click()
    assert.equal((await changed).status(), 200)
    await until(() => owner(`/model-options?session_id=${id}`), value => value.current?.available && value.current.selection.provider === 'computer_provider', 'source selection is ready')
    const localSession = (await execution.local('/state')).sessions[0]
    assert.equal(localSession.server_model.binding.executor_id, source.id)
    assert.equal((await execution.local('/default-model')).provider, 'profile_default')
    await task(page, owner, id, folder, 'source')
    assert.equal(execution.model.calls.length, 0, 'same-name execution Provider is not used')
    assert.equal((await source.local('/state')).sessions.length, 0, 'source does not create an agent or execute tools')
    await assert.rejects(readFile(path.join(source.data, 'provider-source.txt')), /ENOENT/)
    const usage = await owner('/computer-model-requests?limit=50')
    assert.ok(usage.requests.length >= 2)
    for (const request of usage.requests) {
      assert.equal(request.execution_executor_id, execution.id)
      assert.equal(request.source_executor_id, source.id)
      assert.equal(request.actor_user_id, identity.user.user_id)
      assert.equal(request.model_owner_user_id, identity.user.user_id)
      assert.ok(request.attempts.some(attempt => attempt.report?.usage?.input_tokens > 0))
      assert.equal(request.state, 'completed')
    }
    assert.ok(!JSON.stringify(await owner(`/sessions/${id}/events`)).includes(sourceKey))
    result.checks.push('on-demand selection, same-name source isolation, source-only credential, execution-only tools, reported usage attribution')

    const serviceRoot = `/tenants/${identity.personal_tenant_id}/service-accounts`
    const { service_account: serviceAccount } = await owner(serviceRoot, { body: { name: '跨电脑模型任务' } })
    const serviceGrant = await owner(`${serviceRoot}/${serviceAccount.service_account_id}/credentials`, { body: { name: '执行', scopes: ['resource.read', 'run.execute'], expires_at_ms: Date.now() + 600000 } })
    const access = { view: true, submit: true, stop: true, configure: false }
    await owner(`${serviceRoot}/${serviceAccount.service_account_id}/workspaces/${workspace.workspace_id}`, { method: 'PUT', body: { permissions: access, expected_permissions: null } })
    const service = (resource, options = {}) => serverRequest(origin, resource, { token: serviceGrant.access_token, tenantId: identity.personal_tenant_id, ...options })
    const serviceBefore = (await owner(`/sessions/${id}/events`)).length
    await service(`/sessions/${id}/queue`, { body: { content: { kind: 'prompt', input: 'Complete a task through the shared computer model.' }, attachments: [] } })
    const serviceEvents = await until(() => owner(`/sessions/${id}/events`), events => events.slice(serviceBefore).some(event => ['turn_finished', 'turn_failed'].includes(event.type)), 'service account computer-model task completes')
    assert.equal(serviceEvents.slice(serviceBefore).some(event => event.type === 'turn_failed'), false, JSON.stringify(serviceEvents.slice(serviceBefore)))
    const serviceUsage = await owner('/computer-model-requests?limit=50')
    assert.ok(serviceUsage.requests.some(request => request.actor_user_id === serviceAccount.service_account_id && request.model_owner_user_id === identity.user.user_id && request.resource_owner_user_id === identity.user.user_id))
    await owner(`${serviceRoot}/${serviceAccount.service_account_id}/workspaces/${workspace.workspace_id}`, { method: 'PUT', body: { permissions: null, expected_permissions: access } })
    const beforeDenied = source.model.calls.length
    await assert.rejects(() => service(`/sessions/${id}/queue`, { body: { content: { kind: 'prompt', input: 'Not authorized anymore.' }, attachments: [] } }))
    assert.equal(source.model.calls.length, beforeDenied)
    result.checks.push('service account task preserves submitter and source owner; revoked workspace access cannot start another model request')

    // The model binding cannot be installed as a local or managed default.
    const selection = { provider: 'computer_provider', executor_id: source.id, provider_id: 'same', model: 'same-model' }
    await assert.rejects(() => execution.local('/default-model', { method: 'PUT', body: selection }), /Server models/)
    const beforeLocal = source.model.calls.length
    await assert.rejects(() => execution.local(`/sessions/${localSession.identity.session_id}`, { method: 'PATCH', body: { model: selection } }))
    assert.equal(source.model.calls.length, beforeLocal)

    const hold = source.model.holdNext(body => body.stream && body.tools?.length && JSON.stringify(body.messages.findLast(message => message.role === 'user')).includes('hold-source-request'))
    release = hold.release
    const before = (await owner(`/sessions/${id}/events`)).length
    await page.getByRole('textbox', { name: '输入任务', exact: true }).fill('hold-source-request')
    await page.getByRole('button', { name: '发送', exact: true }).click()
    await Promise.race([hold.began, new Promise((_, reject) => setTimeout(() => reject(new Error('source request did not start')), 10000))])
    await source.local('/credentials/SAME_PROVIDER_KEY', { method: 'DELETE' })
    await until(() => owner(`/sessions/${id}/events`), events => events.slice(before).some(event => event.type === 'turn_failed'), 'source revocation stops active task')
    release(); release = undefined
    assert.equal((await owner(`/model-options?session_id=${id}`)).current.available, false)
    assert.equal(execution.model.calls.length, 0)
    result.checks.push('local entry rejection and source credential revocation without fallback')

    await source.local('/credentials', { body: { name: 'SAME_PROVIDER_KEY', value: sourceKey } })
    await source.stop()
    await until(() => owner('/model-computers'), items => items.some(item => item.executor_id === source.id && !item.connected), 'source goes offline')
    assert.equal((await owner(`/model-options?session_id=${id}`)).current.available, false)
    await source.restart()
    assert.equal((await owner(`/model-options?session_id=${id}`)).current.available, true)
    assert.equal((await owner(`/model-options?session_id=${id}`)).current.selection.executor_id, source.id)
    result.checks.push('offline source is unavailable and reconnection preserves the selected identity')

    await page.goto(`${origin}/models?tab=usage&usage_source=device`)
    await page.getByText('跨电脑模型调用', { exact: true }).click()
    const firstRequest = page.locator('[data-forwarded-model-request]').first()
    await firstRequest.waitFor()
    await firstRequest.locator('summary').click()
    for (const width of [1440, 390, 320]) {
      await page.setViewportSize({ width, height: 980 })
      await firstRequest.scrollIntoViewIfNeeded()
      await page.waitForFunction(() => document.documentElement.scrollWidth === window.innerWidth)
      if (artifacts) await page.screenshot({ path: path.join(artifacts, `forwarded-model-usage-${width}.png`) })
    }
    assert.deepEqual(errors, [])
    result.status = 'passed'
  } catch (error) {
    result.status = 'failed'
    result.error = String(error?.stack ?? error)
    result.diagnostics = processes.map(process => process.diagnostics())
    if (artifacts && page) await page.screenshot({ path: path.join(artifacts, 'failure.png') }).catch(() => {})
    throw error
  } finally {
    release?.()
    await browser?.close()
    for (const process of processes.reverse()) await stopProcess(process)
    for (const model of models) await model.close()
    if (artifacts) await writeFile(path.join(artifacts, 'result.json'), JSON.stringify(result, null, 2))
    await rm(directory, { recursive: true, force: true })
  }
})
