import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { chromium } from 'playwright'
import { selectSpace, freePort, initializeServer, repository, serverRequest, startProcess, stopProcess, waitForHttp } from './platform-e2e-fixture.mjs'
import { localApi, until } from './model-device-fixture.mjs'
import { sharedAccountTask } from './account-node-sharing-fixture.mjs'
import { choose, chooseReasoning, closeSettings, computerModels, platformNodeTask, profile, reasoningProfile, refreshReasoning, settings, task, upstream } from './account-node-provider-fixture.mjs'
const binary = process.env.TERNILO_E2E_NODE_BINARY ?? path.join(repository, 'target/debug/ternilo')

test('Server account and computer Providers stay independent and execute on the selected computer', { timeout: 240_000 }, async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-provider-sources-'))
  const artifacts = process.env.TERNILO_E2E_ARTIFACT_DIR ?? directory
  await mkdir(artifacts, { recursive: true })
  const processes = [], upstreams = [], errors = []
  const resourceErrors = [], expectedResourceErrors = new Set()
  const requests = new Map(), network = []
  let phase = 'account-configuration'
  const environment = { XDG_STATE_HOME: path.join(directory, 'state') }
  let restartingNode = false
  const restartReads = new Set()
  const restartPaths = pathname => /^\/api\/v1\/(catalog|agent-presets|providers|credentials)$/.test(pathname) || /^\/api\/v1\/sessions\/[^/]+\/(commands|workspace|plugins)$/.test(pathname)
  let browser, page, node
  try {
    const account = await upstream('account-source', 'account-private-key', 'Working in `<local-workspace>`'); upstreams.push(account)
    const computer = await upstream('computer-source', 'computer-private-key'); upstreams.push(computer)
    const server = await initializeServer({ directory: path.join(directory, 'server'), origin: `http://127.0.0.1:${await freePort()}`, mode: 'single_user', environment }); processes.push(server)
    let tenantId = server.owner.session.personal_tenant_id
    const owner = (resource, options = {}) => serverRequest(server.origin, resource, { token: server.owner.session.access_token, tenantId, ...options })
    const accountRequest = (resource, options = {}) => owner(resource, { tenantId: server.owner.session.personal_tenant_id, ...options })
    await accountRequest('/credentials', { body: { name: 'SAME_PROVIDER_KEY', value: 'account-private-key' } })
    await accountRequest('/providers', { body: profile(account.baseUrl) })
    await owner('/admin/models/providers', { body: { profile: { ...profile(account.baseUrl), id: 'platform-source', api_key_ref: null }, enabled: true, api_key: 'account-private-key' } })
    await owner('/admin/models/publications', { body: { model_id: 'same-model', display_name: '平台同名模型', provider_id: 'platform-source', upstream_model: 'same-model', enabled: true } })
    await owner('/admin/models/grants', { body: { name: '平台独立预算', subject: { kind: 'user', id: server.owner.session.user.user_id }, model_ids: ['same-model'], monthly_tokens: 100000, max_concurrent_requests: 2, allow_resource_sharing: false } })
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
    page = await browser.newPage({ locale: 'zh-CN', viewport: { width: 1366, height: 900 }, hasTouch: true, serviceWorkers: 'block' })
    page.on('pageerror', error => errors.push(error.message))
    page.on('request', request => { if (request.url().includes('/api/v1/')) requests.set(request, { phase, started: Date.now() }) })
    page.on('request', request => { if (restartingNode && request.method() === 'GET' && restartPaths(new URL(request.url()).pathname)) restartReads.add(request.url()) })
    page.on('console', message => {
      if (message.type() !== 'error') return
      const status = message.text().match(/Failed to load resource:.*status of (\d+)/)?.[1]
      if (status) resourceErrors.push({ key: `${status} ${message.location().url}`, text: message.text() })
      else errors.push(message.text())
    })
    page.on('response', async response => {
      const started = requests.get(response.request())
      if (started) network.push({ phase: started.phase, response_phase: phase, method: response.request().method(), path: `${new URL(response.url()).pathname}${new URL(response.url()).search}`, status: response.status(), elapsed_ms: Date.now() - started.started })
      if (response.status() < 400) return
      const restartingRead = response.request().method() === 'GET' && restartPaths(new URL(response.url()).pathname) && (restartingNode || restartReads.has(response.url()))
      const body = await response.text()
      const cancelledByShutdown = response.status() === 409 && body === JSON.stringify({ error: { code: 'cancelled', message: 'local application is shutting down' } })
      if (restartingRead && (response.status() === 503 || cancelledByShutdown)) expectedResourceErrors.add(`${response.status()} ${response.url()}`)
      else errors.push(`${response.status()} ${new URL(response.url()).pathname}: ${body}`)
    })
    await page.goto(server.origin)
    await page.getByLabel('用户名', { exact: true }).fill(server.owner.username)
    await page.getByLabel('密码', { exact: true }).fill(server.owner.password)
    await page.getByRole('button', { name: '登录', exact: true }).click()
    await settings(page)
    await page.locator('[data-provider-scope="account"]').getByText('同名 Provider', { exact: true }).waitFor()
    assert.equal(await page.getByRole('combobox', { name: '账号配置空间', exact: true }).count(), 0)
    assert.equal(await page.locator('[data-provider-scope="node"]').count(), 0)
    await page.locator('[data-account-model-default]').getByRole('button').click()
    await page.getByRole('menuitem', { name: /^模型/ }).click()
    await page.locator('[data-model-provider="same"]').getByRole('menuitem').filter({ hasText: 'same-model' }).click()
    await until(() => accountRequest('/default-model'), value => value.provider === 'named_provider' && value.provider_id === 'same', 'personal default saved without a chat')
    await until(() => page.getByRole('menu').count(), count => count === 0, 'default model menu closed')
    await page.locator('[data-model-source-filter="platform"]').click()
    await page.getByText('平台独立预算', { exact: true }).waitFor()
    assert.equal(await page.locator('[data-provider-scope]').count(), 0)
    await page.screenshot({ path: path.join(artifacts, 'platform-model-center.png'), fullPage: true })
    await closeSettings(page)
    const { tenant } = await owner('/tenants', { body: { slug: 'provider-team', display_name: 'Provider team' } })
    tenantId = tenant.tenant_id
    await page.reload()
    await selectSpace(page, tenantId)
    const enrolled = await owner(`/tenants/${tenantId}/my-computer-enrollments`, { body: { name: 'provider-node', project_id: null, ttl_seconds: 600 } })
    const enrolledComputerId = enrolled.enrollment.executor_id
    const credential = await owner('/enrollments/consume', { body: { token: enrolled.enrollment.token } })
    const origin = `http://127.0.0.1:${await freePort()}`, data = path.join(directory, 'node-data')
    const args = ['serve', '--listen', new URL(origin).host, '--data-dir', data, '--node-id', enrolledComputerId, '--gateway-url', `${server.origin.replace('http:', 'ws:')}/api/v1/executors/connect`, '--allow-insecure-gateway']
    const env = { ...environment, TERNILO_LOCAL_TOKEN: credential.credential.token }
    node = startProcess(binary, args, env); processes.push(node); await waitForHttp(origin, node)
    let local = await localApi(origin)
    await local('/credentials', { body: { name: 'SAME_PROVIDER_KEY', value: 'computer-private-key' } })
    await local('/providers', { body: profile(computer.baseUrl) })
    await until(() => owner('/model-computers'), computers => computers.some(item => item.connected), 'computer without a workspace')
    assert.equal((await owner('/state')).workspaces.length, 0)
    assert.equal((await owner('/providers?executor_id=provider-node'))[0].base_url, computer.baseUrl)
    const folder = path.join(directory, 'workspace'); await mkdir(folder)
    const workspace = await local('/workspaces', { body: { path: folder } })
    const session = await local('/sessions', { body: { workspace_id: workspace.workspace_id } })
    const state = await until(() => owner('/state'), state => state.sessions.length === 1, 'mapped session')
    const serverId = state.sessions[0].identity.session_id
    await page.reload()
    await page.locator(`[data-sidebar-session-row][data-session-id="${serverId}"]`).click()
    await settings(page)
    await page.locator('[data-provider-scope="account"]').getByRole('button', { name: '编辑', exact: true }).click()
    assert.equal(await page.locator('[data-provider-editor="same"] input[type="password"]').inputValue(), '')
    await page.screenshot({ path: path.join(artifacts, 'account-model-center.png'), fullPage: true })
    const accountSaved = page.waitForResponse(response => new URL(response.url()).pathname === '/api/v1/providers' && response.request().method() === 'POST')
    await page.locator('[data-provider-editor="same"]').getByRole('button', { name: '保存', exact: true }).click()
    const accountResponse = await accountSaved
    assert.equal(accountResponse.status(), 200)
    assert.equal(accountResponse.request().headers()['x-ternilo-tenant'], server.owner.session.personal_tenant_id)
    const currentModel = async () => (await owner('/state')).sessions.find(item => item.identity.session_id === serverId).model
    const previousModel = await currentModel()
    await computerModels(page, tenantId)
    await page.locator('[data-provider-scope="node"]').getByRole('button', { name: '编辑', exact: true }).click()
    assert.equal(await page.locator('[data-provider-editor="same"] input[type="password"]').inputValue(), '')
    assert.equal(await page.evaluate(() => localStorage.getItem('ternilo.current-session')), serverId, 'model management does not change the chat target')
    await page.screenshot({ path: path.join(artifacts, 'provider-sources-desktop.png'), fullPage: true })
    const nodeSaved = page.waitForResponse(response => new URL(response.url()).pathname === '/api/v1/providers' && response.request().method() === 'POST')
    await page.locator('[data-provider-editor="same"]').getByRole('button', { name: '保存', exact: true }).click()
    const nodeResponse = await nodeSaved
    assert.equal(nodeResponse.status(), 200)
    assert.equal(nodeResponse.request().headers()['x-ternilo-tenant'], tenantId)
    assert.equal(new URL(nodeResponse.url()).searchParams.get('executor_id'), 'provider-node')
    assert.equal(await page.locator('[data-model-computer]').count(), 1, 'a workspace and its sessions do not duplicate the computer')
    await assert.rejects(() => owner(`/providers?executor_id=provider-node&session_id=${serverId}`), /400/)
    assert.deepEqual(await currentModel(), previousModel, 'saving a device Provider does not choose a model for another chat')
    await closeSettings(page)
    const priorComputer = computer.calls.length
    phase = 'account-task'
    await choose(page, 'account')
    await task(page, owner, serverId, folder, 'account-source')
    await refreshReasoning({ page, owner, local, sessionId: serverId, nodeSessionId: session.identity.session_id,
      configure: () => accountRequest('/providers', { body: reasoningProfile(account.baseUrl) }),
      screenshot: path.join(artifacts, 'account-reasoning-refreshed-desktop.png'),
    })
    const beforeReasoning = account.calls.length
    await task(page, owner, serverId, folder, 'account-source')
    const reasoningCalls = account.calls.slice(beforeReasoning).filter(call => call.stream)
    assert.ok(reasoningCalls.length > 0 && reasoningCalls.every(call => call.reasoning_effort === 'high'))
    const limited = reasoningProfile(account.baseUrl)
    limited.defaults.reasoning = { default_effort: 'low', efforts: { low: 'low' } }
    await accountRequest('/providers', { body: limited })
    const reduced = await owner(`/model-options?session_id=${serverId}`)
    assert.equal(reduced.current.available, false, 'removing the accepted wire value makes the old selection unavailable')
    assert.deepEqual(reduced.current.selectable_reasoning.efforts, { low: 'low' })
    await chooseReasoning(page, 'low')
    const beforeLow = account.calls.length
    await task(page, owner, serverId, folder, 'account-source')
    assert.ok(account.calls.slice(beforeLow).some(call => call.stream && call.reasoning_effort === 'low'))
    await page.locator('[data-session-workspace-path]').filter({ hasText: folder }).waitFor()
    assert.match(await page.locator('[data-session-workspace]').innerText(), /provider-node/)
    await page.locator('[data-role="assistant"] code').filter({ hasText: folder }).last().waitFor()
    assert.ok((await owner(`/sessions/${serverId}/events`)).some(event => event.type === 'assistant_message' && JSON.stringify(event.response).includes('<local-workspace>')), 'real paths are restored for display only, not persisted to the Server transcript')
    await page.screenshot({ path: path.join(artifacts, 'workspace-location-desktop.png') })
    await page.setViewportSize({ width: 390, height: 844 })
    await page.evaluate(async () => {
      await new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve)))
      await Promise.all(document.getAnimations().filter(animation => animation.effect?.getTiming().iterations !== Infinity).map(animation => animation.finished.catch(() => {})))
    })
    const notifications = page.getByRole('button', { name: '关闭通知', exact: true })
    while (await notifications.count()) await notifications.first().click()
    await page.locator('[data-session-workspace-path]').filter({ hasText: folder }).waitFor()
    assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth))
    await page.screenshot({ path: path.join(artifacts, 'workspace-location-mobile.png') })
    await page.setViewportSize({ width: 1366, height: 900 })
    assert.ok(account.calls.length > 0); assert.equal(computer.calls.length, priorComputer)
    const usage = await owner('/model-access/requests')
    assert.ok(usage.requests.some(request => request.origin === 'client_device' && request.source === 'user_provider' && request.actor_user_id === server.owner.session.user.user_id && request.model_beneficiary_user_id === server.owner.session.user.user_id))
    await settings(page)
    await page.getByRole('link', { name: '用量', exact: true }).click()
    await page.getByRole('button', { name: '账号自有', exact: true }).click()
    await page.locator('[data-model-request]').first().waitFor()
    const ownUsage = await owner('/model-access/usage?source=user_provider')
    const grantedUsage = await owner('/model-access/usage?source=platform_grant')
    assert.ok(ownUsage.used_tokens > 0)
    assert.equal(grantedUsage.request_count, 0)
    await page.getByRole('button', { name: '平台授权', exact: true }).click()
    await until(() => page.locator('[data-model-request]').count(), count => count === 0, 'filtered usage list')
    await page.getByRole('button', { name: '设备本地', exact: true }).click()
    await page.getByText(/设备本地用量尚未汇总/).waitFor()
    await page.getByRole('link', { name: '授权与接入', exact: true }).click()
    await page.getByRole('button', { name: '创建接入密钥', exact: true }).waitFor()
    const gap = await page.locator('[data-model-keys]').evaluate(element => {
      const intro = element.firstElementChild.getBoundingClientRect()
      const directory = element.querySelector('section').getBoundingClientRect()
      return directory.top - intro.bottom
    })
    assert.ok(gap >= 16, 'key instructions and directory have breathing room')
    await page.screenshot({ path: path.join(artifacts, 'model-center-access-desktop.png'), fullPage: true })
    await closeSettings(page)
    const priorAccount = account.calls.length
    phase = 'platform-task-and-revocation'
    await platformNodeTask({ page, owner, local, sessionId: serverId, nodeSessionId: session.identity.session_id, folder, account, computer, artifacts })
    const accountAfterPlatform = account.calls.length
    phase = 'device-task'
    await choose(page, 'node')
    await local('/providers', { body: reasoningProfile(computer.baseUrl) })
    await chooseReasoning(page, 'high')
    const beforeDeviceReasoning = computer.calls.length
    await task(page, owner, serverId, folder, 'computer-source')
    assert.ok(computer.calls.slice(beforeDeviceReasoning).some(call => call.stream && call.reasoning_effort === 'high'))
    assert.ok(accountAfterPlatform > priorAccount)
    assert.equal(account.calls.length, accountAfterPlatform)
    assert.equal((await accountRequest('/providers'))[0].base_url, account.baseUrl)
    assert.equal((await local('/providers'))[0].base_url, computer.baseUrl)
    await choose(page, 'account')
    await page.waitForLoadState('networkidle')
    phase = 'node-restart'
    restartingNode = true
    await stopProcess(node)
    node = startProcess(binary, args, env); processes.push(node); await waitForHttp(origin, node); local = await localApi(origin)
    assert.equal((await local('/state')).sessions.find(item => item.identity.session_id === session.identity.session_id).model.provider, 'account_provider')
    await until(() => owner('/state'), state => state.workspaces.some(workspace => workspace.status === 'online'), 'Node reconnects')
    await page.reload()
    restartingNode = false
    restartReads.clear()
    phase = 'account-task-after-restart'
    await task(page, owner, serverId, folder, 'account-source')
    const memberUpstream = await upstream('member-source', 'member-private-key'); upstreams.push(memberUpstream)
    phase = 'shared-account-and-background-child'
    await sharedAccountTask({ browser, page, server, owner, tenantId, sessionId: serverId, folder, upstream: memberUpstream })
    phase = 'mobile-model-management'
    await page.setViewportSize({ width: 390, height: 844 })
    await page.getByRole('button', { name: /^(打开|展开)侧边栏$/ }).click()
    await settings(page)
    await computerModels(page, tenantId)
    assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth))
    await page.screenshot({ path: path.join(artifacts, 'provider-sources-mobile.png'), fullPage: true })
    await page.getByRole('link', { name: '授权与接入', exact: true }).tap()
    await page.getByRole('tab', { name: '模型授权设备', exact: true }).tap()
    await page.getByText('尚未连接设备。请在 Ternilo 的模型设置中发起连接。', { exact: true }).waitFor()
    await page.setViewportSize({ width: 320, height: 740 })
    assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth))
    assert.equal(await page.getByRole('link', { name: '授权与接入', exact: true }).getAttribute('aria-current'), 'page')
    await page.screenshot({ path: path.join(artifacts, 'model-center-access-mobile.png'), fullPage: true })
    await page.getByRole('link', { name: '模型', exact: true }).tap()
    await page.locator('[data-provider-scope="node"] [data-models-state="ready"]').waitFor()
    assert.equal(await page.getByRole('button', { name: '设备本地', exact: true }).getAttribute('aria-pressed'), 'true')
    await page.reload()
    await page.locator('[data-provider-scope="node"] [data-models-state="ready"]').waitFor()
    assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth))
    await page.setViewportSize({ width: 390, height: 844 })
    const nodeScope = page.locator('[data-provider-scope="node"]')
    await nodeScope.getByRole('button', { name: /删除/ }).click()
    await page.getByRole('dialog').last().getByRole('button', { name: '删除', exact: true }).click()
    await until(() => local('/providers'), providers => providers.length === 0, 'computer source deleted')
    assert.equal((await accountRequest('/providers')).length, 1)
    await closeSettings(page)
    await page.getByRole('button', { name: /^(打开|展开)侧边栏$/ }).click()
    await page.getByRole('button', { name: '用户设置', exact: true }).click()
    assert.equal(await page.locator('[data-user-settings]').getByRole('button', { name: '模型', exact: true }).count(), 0)
    assert.equal(await page.locator('[data-user-settings] a[href="/models"]').count(), 0)
    await page.goto(`${server.origin}/settings/models`)
    await page.waitForURL(`${server.origin}/models`)
    await page.locator('[data-provider-scope="account"]').getByText('同名 Provider', { exact: true }).waitFor()
    for (const target of [origin, server.origin]) for (const asset of ['app.js', 'app.css']) {
      const served = await (await fetch(`${target}/assets/${asset}`)).arrayBuffer()
      const built = await readFile(path.join(repository, `web/dist/assets/${asset}`))
      assert.equal(createHash('sha256').update(Buffer.from(served)).digest('hex'), createHash('sha256').update(built).digest('hex'))
    }
    assert.deepEqual([...errors, ...resourceErrors.filter(error => !expectedResourceErrors.has(error.key)).map(error => error.text)], [])
  } catch (error) {
    await page?.screenshot({ path: path.join(artifacts, 'provider-sources-failure.png'), fullPage: true }).catch(() => {})
    error.message += `\n${processes.map(item => item.diagnostics()).join('\n')}`; throw error
  } finally {
    await writeFile(path.join(artifacts, 'provider-network.json'), JSON.stringify(network, null, 2))
    await writeFile(path.join(artifacts, 'provider-processes.log'), processes.map(item => item.diagnostics()).join('\n'))
    await browser?.close()
    for (const process of processes.reverse()) await stopProcess(process)
    for (const source of upstreams) await source.close()
    await rm(directory, { recursive: true, force: true })
  }
})
