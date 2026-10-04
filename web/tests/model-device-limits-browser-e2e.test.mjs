import assert from 'node:assert/strict'
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { chromium } from 'playwright'
import { freePort, initializeServer, serverRequest, startProcess, stopProcess, waitForHttp } from './platform-e2e-fixture.mjs'
import { chooseModel, closeModels, localApi, modelFixture, openModels, seedModels, until } from './model-device-fixture.mjs'
import { profile, upstream } from './account-node-provider-fixture.mjs'
import { selectChoice } from './browser-select-fixture.mjs'
import {
  assertBlocked, authorizeDevice, controlledUpstream, deadline, deviceClient, evidenceRecorder,
  fillLimits, finishHeld, holdRequest, layouts, login, unlimited, usageEquals, verifyEmbeddedAssets,
} from './model-device-limits-fixture.mjs'

test('device limits span account providers and platform budgets in real Server and independent Ternilo', { timeout: 420_000 }, async () => {
  const serverBinary = process.env.TERNILO_E2E_SERVER_BINARY
  const localBinary = process.env.TERNILO_E2E_LOCAL_BINARY ?? process.env.TERNILO_E2E_NODE_BINARY
  assert.ok(serverBinary && localBinary, 'Set final-build SERVER and LOCAL (or NODE) binary paths explicitly; this test never builds or uses implicit old binaries')
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-device-limits-'))
  const artifacts = path.join(process.env.TERNILO_E2E_ARTIFACT_DIR ?? directory, `run-${Date.now()}`)
  await mkdir(artifacts, { recursive: true })
  const evidence = evidenceRecorder(artifacts)
  const processes = [], sources = [], pages = []
  let browser
  try {
    const environment = {
      ...Object.fromEntries(Object.keys(process.env).filter(key => key.startsWith('TERNILO_')).map(key => [key, undefined])),
      HOME: path.join(directory, 'home'), XDG_STATE_HOME: path.join(directory, 'state'),
      XDG_CONFIG_HOME: path.join(directory, 'config'), XDG_DATA_HOME: path.join(directory, 'data'),
    }
    await mkdir(environment.HOME)
    const platformBase = await modelFixture('device-limit-local-task')
    sources.push(platformBase)
    const accountBase = await upstream('device-limit-account-task', 'synthetic-device-limit-upstream-key')
    sources.push(accountBase)
    const platform = await controlledUpstream(platformBase)
    sources.push(platform)
    const account = await controlledUpstream(accountBase)
    sources.push(account)
    const server = await initializeServer({ directory: path.join(directory, 'server'), origin: `http://127.0.0.1:${await freePort()}`, binary: serverBinary, environment })
    processes.push(server)
    const seeded = await seedModels(server, platform)
    const ownerToken = server.owner.session.access_token
    const owner = (resource, options = {}) => evidence.http(server.origin, `/api/v1${resource}`, { token: ownerToken, ...options })
    const personal = (resource, options = {}) => serverRequest(server.origin, resource, { token: ownerToken, tenantId: server.owner.session.personal_tenant_id, ...options })
    await personal('/credentials', { body: { name: 'DEVICE_LIMIT_UPSTREAM_KEY', value: 'synthetic-device-limit-upstream-key' } })
    await personal('/providers', { body: {
      ...profile(account.baseUrl), id: 'device-limit-private', display_name: 'Device limit account source', api_key_ref: 'DEVICE_LIMIT_UPSTREAM_KEY',
      models: [{ id: 'account-model', display_name: 'Account Model', settings: { mode: 'inherit' } }],
    } })
    const registration = await owner('/admin/registration')
    await owner('/admin/registration', { method: 'PATCH', body: { mode: 'open', require_approval: false, oidc_only: false, revision: registration.revision } })
    const memberCredentials = { username: 'limits-member', email: 'limits-member@example.test', password: 'synthetic-limits-member-password' }
    const { session: memberSession } = await evidence.http(server.origin, '/api/v1/auth/register', { method: 'POST', body: memberCredentials, status: 201 })
    const origin = `http://127.0.0.1:${await freePort()}`
    const localProcess = startProcess(localBinary, ['serve', '--listen', new URL(origin).host, '--data-dir', path.join(directory, 'local')], environment)
    processes.push(localProcess)
    await waitForHttp(origin, localProcess)
    const local = await localApi(origin)
    const folder = path.join(directory, 'workspace')
    await mkdir(folder)
    const workspace = await local('/workspaces', { body: { path: folder } })
    const session = await local('/sessions', { body: { workspace_id: workspace.workspace_id } })
    const localSessionId = session.identity.session_id
    await verifyEmbeddedAssets([server.origin, origin], evidence)
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
    const openPage = async name => {
      const page = await browser.newPage({ viewport: { width: 1440, height: 960 }, hasTouch: true, serviceWorkers: 'block', locale: 'zh-CN', timezoneId: 'Asia/Shanghai', reducedMotion: 'reduce', colorScheme: 'dark' })
      page.setDefaultTimeout(15_000)
      evidence.observe(page, name)
      pages.push({ page, name })
      return page
    }
    const localPage = await openPage('local')
    const approval = await openPage('owner')
    evidence.supersededRead('local', `/api/v1/sessions/${localSessionId}/workspace`)
    const openLocalModels = async () => {
      await localPage.locator('[data-app-frame]').waitFor()
      const sidebar = localPage.getByRole('button', { name: '打开侧边栏', exact: true })
      if (await sidebar.isVisible()) await sidebar.click()
      return openModels(localPage)
    }
    await localPage.goto(origin)
    evidence.action('initial-local-connection-approval')
    const settings = await openLocalModels()
    await settings.getByRole('button', { name: '连接 Server', exact: true }).click()
    await settings.getByLabel('Server 地址', { exact: true }).fill(server.origin)
    await settings.getByLabel('连接名称', { exact: true }).fill('Device limits Local')
    await settings.getByRole('button', { name: '连接 Server', exact: true }).last().click()
    const code = await settings.locator('[data-device-user-code]').textContent()
    await approval.goto(await settings.getByRole('link', { name: '前往 Server 确认', exact: true }).getAttribute('href'))
    await login(approval, server.owner)
    await approval.getByText(code, { exact: true }).waitFor()
    await selectChoice(approval.getByLabel('允许使用的模型范围', { exact: true }), 'selected')
    for (const grant of [seeded.first, seeded.second]) await approval.locator(`[data-device-grant="${grant.grant_id}"]`).getByRole('checkbox').first().check()
    await approval.locator('[data-device-provider="device-limit-private"]').getByRole('checkbox').first().check()
    const scope = {
      kind: 'selected', grants: [seeded.first, seeded.second].map(grant => ({ grant_id: grant.grant_id, model_ids: ['account-model'] })),
      providers: [{ provider_id: 'device-limit-private', model_ids: ['account-model'] }],
    }
    const initialLimits = { monthly_tokens: 2_000_000, max_concurrent_requests: 4, requests_per_minute: 30, expires_at_ms: Date.now() + 3_600_000 }
    const approvalRoot = approval.locator('[data-model-device-approval]')
    const verifyInvalid = async (root, submit, field, value, message) => {
      await fillLimits(root, initialLimits)
      const before = evidence.result.network.filter(entry => entry.page === 'owner' && ['POST', 'PATCH'].includes(entry.method)).length
      await root.locator(`input[name="${field}"]`).fill(value)
      await submit.click()
      await root.getByRole('alert').filter({ hasText: message }).waitFor()
      assert.equal(await root.locator(`input[name="${field}"]`).getAttribute('aria-invalid'), 'true')
      assert.equal(evidence.result.network.filter(entry => entry.page === 'owner' && ['POST', 'PATCH'].includes(entry.method)).length, before, 'Invalid limits are rejected before HTTP submission')
    }
    await verifyInvalid(approvalRoot, approval.getByRole('button', { name: '允许连接', exact: true }), 'monthly_tokens', '0', /每月.*整数/)
    await evidence.capture(approval, 'approval-invalid-monthly', approvalRoot.locator('[data-device-limits]'))
    await verifyInvalid(approvalRoot, approval.getByRole('button', { name: '允许连接', exact: true }), 'max_concurrent_requests', '-1', /同时.*整数/)
    await verifyInvalid(approvalRoot, approval.getByRole('button', { name: '允许连接', exact: true }), 'requests_per_minute', '10001', /每分钟.*整数/)
    await verifyInvalid(approvalRoot, approval.getByRole('button', { name: '允许连接', exact: true }), 'expires_at_ms', '2000-01-01T00:00', /请选择未来.*到期时间/)
    await fillLimits(approvalRoot, initialLimits)
    await layouts(approval, approvalRoot, 'approval', evidence, async () => {
      await approval.getByText(code, { exact: true }).waitFor()
      await selectChoice(approval.getByLabel('允许使用的模型范围', { exact: true }), 'selected')
      for (const grant of [seeded.first, seeded.second]) await approval.locator(`[data-device-grant="${grant.grant_id}"]`).getByRole('checkbox').first().check()
      await approval.locator('[data-device-provider="device-limit-private"]').getByRole('checkbox').first().check()
      await fillLimits(approvalRoot, initialLimits)
    })
    const decision = approval.waitForResponse(response => new URL(response.url()).pathname === '/api/v1/model-access/device-authorization' && response.request().method() === 'POST')
    await approval.getByRole('button', { name: '允许连接', exact: true }).click()
    const approved = await decision
    assert.equal(approved.status(), 204)
    assert.deepEqual(approved.request().postDataJSON().limits, initialLimits)
    assert.deepEqual(approved.request().postDataJSON().scope, scope)
    await approval.getByText('已允许连接。回到 Ternilo 即可继续。', { exact: true }).waitFor()
    const connections = await until(() => local('/model-connections'), values => values.length === 1, 'Local saved approved device')
    const localDevice = connections[0].session.identity
    assert.deepEqual(localDevice.limits, initialLimits)
    assert.equal(connections[0].session.grants.length, 2)
    assert.equal(connections[0].session.providers.length, 1)
    const connectionId = connections[0].connection_id
    const connectionCard = localPage.locator(`[data-model-connection="${connectionId}"]`)
    const assertLocalLimits = async limits => {
      const information = connectionCard.locator('[data-connection-limits]')
      await information.getByText(`每月 token 上限：${limits.monthly_tokens?.toLocaleString('zh-CN') ?? '不另限制'}`, { exact: true }).waitFor()
      await information.getByText(`同时请求上限：${limits.max_concurrent_requests?.toLocaleString('zh-CN') ?? '不另限制'}`, { exact: true }).waitFor()
      await information.getByText(`每分钟请求上限：${limits.requests_per_minute?.toLocaleString('zh-CN') ?? '不另限制'}`, { exact: true }).waitFor()
      const expiry = limits.expires_at_ms === null ? '未设置到期时间' : `有效期至 ${await localPage.evaluate(timestamp => new Intl.DateTimeFormat('zh-CN', { dateStyle: 'medium', timeStyle: 'short' }).format(timestamp), limits.expires_at_ms)}`
      await information.getByText(expiry, { exact: true }).waitFor()
      assert.equal(await information.locator('input, button, select').count(), 0, 'Local limits are read-only metadata, not a second management form')
    }
    await assertLocalLimits(initialLimits)
    await evidence.capture(localPage, 'local-approved-limits-1440', connectionCard)
    await closeModels(localPage)
    evidence.check('initial-approval-four-limits-and-identity', { deviceId: localDevice.device_id, limits: initialLimits })

    evidence.action('independent-local-real-task')
    const providers = await local('/providers')
    const alpha = providers.find(provider => provider.display_name.includes('Budget Alpha'))
    assert.ok(alpha)
    await chooseModel(localPage, alpha.id)
    await localPage.getByRole('textbox', { name: '输入任务', exact: true }).fill('Create the device-limit proof file with this explicitly selected budget.')
    await localPage.getByRole('button', { name: '发送', exact: true }).click()
    const events = await until(() => local(`/sessions/${localSessionId}/events`), values => values.some(event => ['turn_finished', 'turn_failed'].includes(event.type)), 'independent Local task finished')
    assert.equal(events.some(event => event.type === 'turn_failed'), false, JSON.stringify(events))
    assert.equal(await readFile(path.join(folder, 'model-device-proof.txt'), 'utf8'), 'device-limit-local-task')
    await until(() => local(`/sessions/${localSessionId}/queue`), queue => !queue.active_run_id, 'Local task idle')
    const localResource = `/model-access/devices/${localDevice.device_id}`
    const localUsage = await until(() => owner(`${localResource}/usage`), usage => usage.active_requests === 0 && usage.used_tokens >= 84 && usage.reserved_tokens === 0, 'Local usage fully settled')
    assert.equal(localUsage.month, new Date().toISOString().slice(0, 7), 'Usage is an UTC calendar month, not the browser timezone')
    assert.equal(account.calls.length, 0, 'Platform Local task never switches to the account Provider')
    assert.equal((await owner('/model-access/keys?limit=50')).keys.length, 0)
    assert.equal((await personal('/execution-targets')).executors.length, 0, 'This Ternilo was not enrolled as a Node or Worker')
    const localRows = (await owner('/model-access/requests?limit=50')).requests.filter(entry => entry.key_id === localDevice.device_id)
    assert.ok(localRows.length >= 2 && localRows.every(entry => entry.grant_id === seeded.first.grant_id && entry.accounted_tokens === 42))
    await evidence.capture(localPage, 'local-real-task')
    evidence.check('independent-local-file-task-and-42-token-settlement', { localUsage, requests: localRows.map(entry => entry.request_id) })

    evidence.action('device-limits-edit-and-usage-ui')
    await approval.goto(`${server.origin}/models?tab=access&access=devices`)
    const row = id => approval.locator(`[data-model-device="${id}"]`)
    const edit = async id => {
      await row(id).getByRole('button', { name: '编辑限制', exact: true }).click()
      const dialog = approval.getByRole('dialog', { name: '编辑限制', exact: true })
      await dialog.locator('[data-device-usage][aria-busy="false"] [data-device-used]').waitFor()
      return dialog
    }
    const dismiss = async dialog => {
      await dialog.getByRole('button', { name: '取消', exact: true }).click()
      await dialog.waitFor({ state: 'hidden' })
    }
    let dialog = await edit(localDevice.device_id)
    assert.equal(Number((await dialog.locator('[data-device-used]').textContent()).replaceAll(',', '')), localUsage.used_tokens)
    assert.equal(Number(await dialog.locator('[data-device-reserved]').textContent()), 0)
    assert.equal(Number(await dialog.locator('[data-device-active]').textContent()), 0)
    await dialog.getByText(new RegExp(`${localUsage.month}.*UTC|UTC.*${localUsage.month}`)).waitFor()
    await layouts(approval, dialog, 'edit-and-usage', evidence, () => edit(localDevice.device_id))
    await fillLimits(dialog, unlimited)
    await dismiss(dialog)
    assert.deepEqual((await owner('/model-access/devices?limit=50')).devices.find(device => device.device_id === localDevice.device_id).limits, initialLimits)
    dialog = await edit(localDevice.device_id)
    await verifyInvalid(dialog, dialog.getByRole('button', { name: '保存', exact: true }), 'monthly_tokens', '1.5', /每月.*整数/)
    await verifyInvalid(dialog, dialog.getByRole('button', { name: '保存', exact: true }), 'max_concurrent_requests', '0', /同时.*整数/)
    await verifyInvalid(dialog, dialog.getByRole('button', { name: '保存', exact: true }), 'expires_at_ms', '2000-01-01T00:00', /请选择未来.*到期时间/)
    await evidence.capture(approval, 'edit-invalid-expiry', dialog)
    const editedLimits = { monthly_tokens: 1_000_000, max_concurrent_requests: 2, requests_per_minute: 20, expires_at_ms: Date.now() + 1_800_000 }
    let cachedLimits = initialLimits
    for (const limits of [editedLimits, unlimited]) {
      await fillLimits(dialog, limits)
      const saved = approval.waitForResponse(response => new URL(response.url()).pathname === `/api/v1${localResource}` && response.request().method() === 'PATCH')
      await dialog.getByRole('button', { name: '保存', exact: true }).click()
      const savedResponse = await saved
      assert.equal(savedResponse.status(), 200)
      assert.deepEqual(savedResponse.request().postDataJSON(), limits)
      assert.deepEqual((await savedResponse.json()).limits, limits)
      await dialog.waitFor({ state: 'hidden' })
      await openLocalModels()
      assert.deepEqual((await local('/model-connections'))[0].session.identity.limits, cachedLimits, 'Server edits do not pretend to update the Local cached identity before refresh')
      await assertLocalLimits(cachedLimits)
      const refreshed = localPage.waitForResponse(response => new URL(response.url()).pathname === `/api/v1/model-connections/${connectionId}/refresh` && response.request().method() === 'POST')
      await connectionCard.getByRole('button', { name: '刷新 Device limits Local 的模型', exact: true }).click()
      assert.equal((await refreshed).status(), 200)
      await until(() => local('/model-connections'), values => JSON.stringify(values[0].session.identity.limits) === JSON.stringify(limits), 'Refreshed Local identity carries Server limits')
      await assertLocalLimits(limits)
      if (limits === editedLimits) await layouts(localPage, connectionCard, 'local-refreshed-limits', evidence, openLocalModels)
      else await evidence.capture(localPage, 'local-refreshed-unlimited-320', connectionCard)
      await closeModels(localPage)
      cachedLimits = limits
      if (limits === editedLimits) dialog = await edit(localDevice.device_id)
    }
    evidence.check('edit-cancel-validation-save-null-and-usage-ui', { localDevice: localDevice.device_id })

    evidence.action('independent-http-code-device')
    const authorized = await authorizeDevice(server, evidence, 'HTTP quota device', scope, unlimited)
    const client = deviceClient(server, evidence, authorized)
    const routes = [
      { name: 'account', path: '/v1/device-account/device-limit-private', model: 'account-model', source: account },
      { name: 'alpha', path: `/v1/device/${seeded.first.grant_id}`, model: 'account-model', source: platform },
      { name: 'beta', path: `/v1/device/${seeded.second.grant_id}`, model: 'account-model', source: platform },
    ]
    evidence.action('rolling-request-rate-shared-across-sources')
    const rateDevice = await authorizeDevice(server, evidence, 'HTTP rate device', scope, { ...unlimited, requests_per_minute: 3 })
    const rateClient = deviceClient(server, evidence, rateDevice)
    for (const route of routes) assert.equal((await rateClient.request(route, `rate-${route.name}`)).usage.total_tokens, 42)
    await assertBlocked(rateClient, routes, 'request rate limit', evidence)
    await rateClient.patch(unlimited)
    assert.equal((await rateClient.request(routes[0], 'rate-cleared')).usage.total_tokens, 42)
    await usageEquals(rateClient, { used_tokens: 168, reserved_tokens: 0, active_requests: 0 })
    evidence.check('clearing-rate-reopens-admission-without-resetting-usage')
    assert.deepEqual(await client.usage(), { month: new Date().toISOString().slice(0, 7), used_tokens: 0, reserved_tokens: 0, active_requests: 0 })
    for (const route of routes) {
      const result = await client.request(route, `initial-${route.name}`)
      assert.equal(result.usage.total_tokens, 42)
    }
    await usageEquals(client, { used_tokens: 126, reserved_tokens: 0, active_requests: 0 })
    for (const [invalid, error] of [
      [{ ...unlimited, monthly_tokens: 0 }, /model device limits must be positive/],
      [{ ...unlimited, max_concurrent_requests: 0 }, /model device concurrency must be between 1 and 10000/],
      [{ ...unlimited, requests_per_minute: 0 }, /model device requests per minute must be between 1 and 10000/],
      [{ ...unlimited, expires_at_ms: Date.now() - 1 }, /model device expiry must be in the future/],
    ]) {
      await evidence.http(server.origin, client.resource, { token: ownerToken, method: 'PATCH', body: invalid, status: 400, error })
    }
    assert.deepEqual((await evidence.http(server.origin, '/v1/model-device', { token: authorized.token })).identity.limits, unlimited)
    evidence.check('all-null-three-origins-and-server-invalid-validation')

    evidence.action('shared-concurrency-and-lower-limit-accepted-streams')
    await client.patch({ ...unlimited, max_concurrent_requests: 3 })
    const held = []
    for (const route of routes) held.push(await holdRequest(client, route, `lower-concurrency-${route.name}`))
    const pendingUsage = await usageEquals(client, { used_tokens: 126, active_requests: 3 })
    assert.ok(pendingUsage.reserved_tokens > 0)
    await client.patch({ ...unlimited, max_concurrent_requests: 1 })
    await assertBlocked(client, routes, 'concurrent request limit', evidence)
    await new Promise(resolve => setTimeout(resolve, 2_500))
    await finishHeld(client, held, 252)
    evidence.check('lowering-concurrency-keeps-three-accepted-streams', { pendingUsage })

    evidence.action('one-account-call-blocks-idle-platform-budgets')
    const onlyAccountHeld = await holdRequest(client, routes[0], 'account-occupies-shared-device-concurrency')
    await usageEquals(client, { used_tokens: 252, active_requests: 1 })
    await assertBlocked(client, routes, 'concurrent request limit', evidence)
    await finishHeld(client, [onlyAccountHeld], 294)

    evidence.action('lower-monthly-budget-with-accepted-stream')
    const monthlyHeld = await holdRequest(client, routes[0], 'lower-monthly-account')
    const beforeLower = await usageEquals(client, { used_tokens: 294, active_requests: 1 })
    assert.ok(beforeLower.reserved_tokens > 0)
    await client.patch({ ...unlimited, monthly_tokens: 1 })
    await assertBlocked(client, routes, 'monthly token limit', evidence)
    await new Promise(resolve => setTimeout(resolve, 2_500))
    await finishHeld(client, [monthlyHeld], 336)
    assert.deepEqual(await owner(`${localResource}/usage`), localUsage, 'The other device has an independent ledger')
    evidence.check('lower-monthly-budget-keeps-accepted-stream')

    evidence.action('unknown-usage-reservation-never-becomes-zero')
    await client.patch(unlimited)
    const unknownMarker = 'unknown-usage-budget-alpha'
    platform.control(unknownMarker, 'unknown')
    const unknownDelivery = await client.request(routes[1], unknownMarker, { stream: true })
    const unknownEnd = await deadline(unknownDelivery.completion, 'usage-less upstream completion')
    assert.equal(unknownEnd.error, null)
    assert.match(unknownEnd.text, /\[DONE\]/)
    const unknownUsage = await usageEquals(client, { used_tokens: 336, active_requests: 0 })
    assert.ok(unknownUsage.reserved_tokens > 1024, 'Unknown usage retains its positive reservation, not a zero charge')
    const unknownRow = (await owner('/model-access/requests?limit=50')).requests.find(entry => entry.request_id === unknownDelivery.entry.requestId)
    assert.equal(unknownRow.accounted_tokens, null)
    assert.equal(unknownRow.reserved_tokens, unknownUsage.reserved_tokens)
    await client.patch({ ...unlimited, monthly_tokens: unknownUsage.used_tokens + unknownUsage.reserved_tokens + 1024 })
    await assertBlocked(client, routes, 'monthly token limit', evidence)
    await client.patch(unlimited)
    for (const route of routes) assert.equal((await client.request(route, `after-unknown-${route.name}`)).usage.total_tokens, 42)
    await usageEquals(client, { used_tokens: 462, reserved_tokens: unknownUsage.reserved_tokens, active_requests: 0 })
    const ledger = (await owner('/model-access/requests?limit=50')).requests.filter(entry => entry.key_id === client.id)
    assert.ok(ledger.some(entry => entry.source === 'user_provider' && entry.grant_id === null))
    for (const grant of [seeded.first, seeded.second]) assert.ok(ledger.some(entry => entry.grant_id === grant.grant_id))
    assert.equal(ledger.filter(entry => entry.accounted_tokens === 42).length, 11)
    assert.equal(ledger.length, 12, 'Rejected attempts neither switch budget nor insert accepted requests')
    await approval.goto(`${server.origin}/models?tab=access&access=devices`)
    dialog = await edit(client.id)
    assert.equal(Number((await dialog.locator('[data-device-used]').textContent()).replaceAll(',', '')), 462)
    assert.equal(Number((await dialog.locator('[data-device-reserved]').textContent()).replaceAll(',', '')), unknownUsage.reserved_tokens)
    const refreshedUsage = approval.waitForResponse(response => new URL(response.url()).pathname === `${client.resource}/usage` && response.request().method() === 'GET')
    await dialog.locator('[data-device-usage]').getByRole('button', { name: '刷新', exact: true }).click()
    assert.equal((await refreshedUsage).status(), 200)
    await dialog.locator('[data-device-usage][aria-busy="false"] [data-device-reserved]').waitFor()
    assert.equal(Number((await dialog.locator('[data-device-reserved]').textContent()).replaceAll(',', '')), unknownUsage.reserved_tokens)
    await evidence.capture(approval, 'unknown-usage-preserved-320', dialog)
    await dismiss(dialog)
    evidence.check('unknown-reservation-retained-and-actual-42-token-settlement', { unknownUsage, deviceId: client.id })

    evidence.action('cross-account-browser-isolation')
    const otherDevice = await authorizeDevice(server, evidence, 'Member private device', { kind: 'account' }, unlimited, memberSession.access_token)
    const memberPage = await openPage('member')
    await memberPage.setViewportSize({ width: 390, height: 844 })
    await memberPage.goto(`${server.origin}/models?tab=access&access=devices`)
    await login(memberPage, memberCredentials)
    await memberPage.locator(`[data-model-device="${otherDevice.session.identity.device_id}"]`).waitFor()
    assert.equal(await memberPage.locator(`[data-model-device="${localDevice.device_id}"], [data-model-device="${client.id}"]`).count(), 0)
    for (const method of ['PATCH', 'GET']) {
      const pathname = `${client.resource}${method === 'GET' ? '/usage' : ''}`
      evidence.expectBrowserError('member', method, pathname, 403, /model device belongs to another account/)
      const forbidden = await memberPage.evaluate(async ({ pathname, method, token, limits }) => {
        const response = await fetch(pathname, { method, headers: { authorization: `Bearer ${token}`, 'content-type': 'application/json' }, ...(method === 'PATCH' ? { body: JSON.stringify(limits) } : {}) })
        return { status: response.status, body: await response.text() }
      }, { pathname, method, token: memberSession.access_token, limits: unlimited })
      assert.equal(forbidden.status, 403)
      assert.match(forbidden.body, /model device belongs to another account/)
    }
    for (const method of ['PATCH', 'GET']) {
      await evidence.http(server.origin, `/api/v1/model-access/devices/${otherDevice.session.identity.device_id}${method === 'GET' ? '/usage' : ''}`, {
        token: ownerToken, method, ...(method === 'PATCH' ? { body: unlimited } : {}), status: 403, error: /model device belongs to another account/,
      })
    }
    await evidence.capture(memberPage, 'member-only-own-device-390')
    evidence.check('only-owner-can-read-usage-or-edit-including-platform-admin')

    evidence.action('expired-device-all-origins-stream-recheck')
    const expiring = await authorizeDevice(server, evidence, 'Expiring device', scope, { monthly_tokens: 1_000_000, max_concurrent_requests: 3, expires_at_ms: Date.now() + 60_000 })
    const expiryClient = deviceClient(server, evidence, expiring)
    const expiryStreams = []
    for (const route of routes) expiryStreams.push(await holdRequest(expiryClient, route, `expiry-${route.name}`))
    const beforeExpiry = await usageEquals(expiryClient, { used_tokens: 0, active_requests: 3 })
    const expiresAt = Date.now() + 5_000
    await expiryClient.patch({ ...unlimited, expires_at_ms: expiresAt })
    await approval.goto(`${server.origin}/models?tab=access&access=devices`)
    await row(expiryClient.id).waitFor()
    for (const stream of expiryStreams) {
      const ended = await deadline(stream.delivery.completion, `expiry stops ${stream.route.name}`, 20_000)
      assert.equal(ended.error, null)
      assert.match(ended.text, /"error"/)
      assert.match(ended.text, /invalid, expired, or revoked/)
      assert.doesNotMatch(ended.text, /Released successfully|\[DONE\]/)
      await deadline(stream.control.closedPromise, 'Server closed expired upstream')
    }
    await usageEquals(expiryClient, { used_tokens: 0, active_requests: 0, reserved_tokens: beforeExpiry.reserved_tokens })
    const beforeDenied = routes.map(route => route.source.calls.length)
    for (const route of routes) {
      await evidence.http(server.origin, `${route.path}/models`, { token: expiring.token, status: 401, error: /invalid, expired, or revoked/ })
      await expiryClient.request(route, `after-expiry-${route.name}`, { status: 401, error: /invalid, expired, or revoked/ })
    }
    for (const method of ['GET', 'DELETE']) await evidence.http(server.origin, '/v1/model-device', { token: expiring.token, method, status: 401, error: /invalid, expired, or revoked/ })
    await evidence.http(server.origin, expiryClient.resource, { token: ownerToken, method: 'PATCH', body: { ...unlimited, expires_at_ms: Date.now() + 60_000 }, status: 409, error: /expired or revoked model devices require a new authorization/ })
    assert.deepEqual(routes.map(route => route.source.calls.length), beforeDenied)
    await row(expiryClient.id).getByText('已到期', { exact: true }).waitFor()
    assert.equal(await row(expiryClient.id).getByRole('button', { name: '编辑限制', exact: true }).isDisabled(), true)
    await layouts(approval, row(expiryClient.id), 'expired-device', evidence, () => row(expiryClient.id).waitFor())
    await row(expiryClient.id).getByRole('button', { name: '查看用量', exact: true }).click()
    const expiredUsage = approval.getByRole('dialog', { name: '设备调用用量', exact: true })
    await expiredUsage.locator('[data-device-usage][aria-busy="false"] [data-device-reserved]').waitFor()
    assert.equal(Number((await expiredUsage.locator('[data-device-reserved]').textContent()).replaceAll(',', '')), beforeExpiry.reserved_tokens)
    assert.equal(Number(await expiredUsage.locator('[data-device-active]').textContent()), 0)
    await evidence.capture(approval, 'expired-usage-readable-320', expiredUsage)
    await approval.keyboard.press('Escape')
    await expiredUsage.waitFor({ state: 'hidden' })
    evidence.check('expiry-stops-all-three-active-streams-and-denies-catalog-and-next-call', { deviceId: expiryClient.id, beforeExpiry })

    evidence.action('revoked-device-stale-editor-real-error')
    await approval.goto(`${server.origin}/models?tab=access&access=devices`)
    dialog = await edit(client.id)
    await fillLimits(dialog, { ...unlimited, monthly_tokens: 500_000 })
    await owner(`/model-access/devices/${client.id}`, { method: 'DELETE', status: 204 })
    evidence.expectBrowserError('owner', 'PATCH', client.resource, 409, /expired or revoked model devices require a new authorization/)
    await dialog.getByRole('button', { name: '保存', exact: true }).click()
    await dialog.getByRole('alert').filter({ hasText: /expired or revoked model devices require a new authorization/ }).waitFor()
    assert.equal(await dialog.locator('input[name="monthly_tokens"]').inputValue(), '500000', 'A failed save preserves the draft')
    await evidence.capture(approval, 'revoked-edit-server-conflict-320', dialog)
    await dismiss(dialog)
    await approval.goto(`${server.origin}/models?tab=access&access=devices`)
    await row(client.id).getByText('已撤销', { exact: true }).waitFor()
    assert.equal(await row(client.id).getByRole('button', { name: '编辑限制', exact: true }).isDisabled(), true)
    await evidence.http(server.origin, client.resource, { token: ownerToken, method: 'PATCH', body: unlimited, status: 409, error: /expired or revoked model devices require a new authorization/ })
    for (const method of ['GET', 'DELETE']) await evidence.http(server.origin, '/v1/model-device', { token: authorized.token, method, status: 401, error: /invalid, expired, or revoked/ })
    evidence.check('revoked-device-cannot-be-reactivated-and-real-save-error-preserves-draft')

    evidence.action('local-expired-refresh-is-policy-denial-not-boot-authentication')
    const preservedConnections = await local('/model-connections')
    const preservedSelection = (await local('/state')).sessions.find(value => value.identity.session_id === localSessionId).model
    const localExpiry = Date.now() + 2_000
    await owner(localResource, { method: 'PATCH', body: { ...unlimited, expires_at_ms: localExpiry } })
    await new Promise(resolve => setTimeout(resolve, Math.max(0, localExpiry - Date.now()) + 100))
    await openLocalModels()
    const refreshPath = `/api/v1/model-connections/${connectionId}/refresh`
    const expiredMessage = /model Server authorization is invalid, expired or revoked; reconnect or check account access/
    evidence.expectBrowserError('local', 'POST', refreshPath, 403, expiredMessage)
    const deniedRefresh = localPage.waitForResponse(response => new URL(response.url()).pathname === refreshPath && response.request().method() === 'POST')
    await connectionCard.getByRole('button', { name: '刷新 Device limits Local 的模型', exact: true }).click()
    assert.equal((await deniedRefresh).status(), 403)
    await localPage.getByRole('alert').filter({ hasText: expiredMessage }).waitFor()
    assert.deepEqual(await local('/model-connections'), preservedConnections, 'Expired Server authorization does not delete the cached Local connection')
    assert.deepEqual((await local('/state')).sessions.find(value => value.identity.session_id === localSessionId).model, preservedSelection, 'A failed refresh never switches provider or budget')
    assert.equal(await connectionCard.count(), 1)
    assert.equal(await localPage.getByRole('dialog', { name: '连接 Ternilo 节点', exact: true }).count(), 0)
    assert.equal(await localPage.getByLabel('访问令牌', { exact: true }).count(), 0)
    await evidence.capture(localPage, 'local-expired-refresh-retains-connection-320', connectionCard)
    await closeModels(localPage)
    await localPage.getByRole('textbox', { name: '输入任务', exact: true }).waitFor()
    evidence.check('local-expired-refresh-403-retains-connection-selection-and-boot-session')

    await verifyEmbeddedAssets([server.origin, origin], evidence)
    assert.deepEqual(platform.failures, [])
    assert.deepEqual(account.failures, [])
    evidence.result.upstreams = { platform: platform.calls, account: account.calls }
    assert.ok(evidence.result.assets.some(entry => entry.browser && new URL(entry.url).origin === origin))
    assert.ok(evidence.result.assets.some(entry => entry.browser && new URL(entry.url).origin === server.origin))
    await evidence.clean()
    evidence.result.status = 'passed'
  } catch (error) {
    evidence.result.status = 'failed'
    evidence.result.failure = { message: error.message, stack: error.stack }
    for (const { page, name } of pages) await evidence.capture(page, `failure-${name}`).catch(() => {})
    throw error
  } finally {
    evidence.beginCleanup()
    await browser?.close()
    for (const process of processes.reverse()) {
      await stopProcess(process)
      evidence.result.processes.push({ pid: process.child.pid, exitCode: process.child.exitCode, signalCode: process.child.signalCode })
    }
    for (const source of sources.reverse()) await source.close()
    await writeFile(path.join(artifacts, 'processes.log'), processes.map(process => process.diagnostics()).join('\n'))
    await evidence.save()
    await rm(directory, { recursive: true, force: true })
    console.log(`Device limits artifacts: ${artifacts}`)
  }
})
