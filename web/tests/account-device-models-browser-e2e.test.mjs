import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { mkdir, mkdtemp, readFile, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { chromium } from 'playwright'
import { freePort, initializeServer, repository, serverRequest, startProcess, stopProcess, waitForHttp } from './platform-e2e-fixture.mjs'
import { approveConnection, chooseModel, closeModels, localApi, modelFixture, openModels, seedModels, until } from './model-device-fixture.mjs'
import { profile, upstream } from './account-node-provider-fixture.mjs'

test('a standalone device can use explicitly delegated account models without obtaining account login or upstream credentials', { timeout: 240_000 }, async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-account-device-'))
  const artifacts = process.env.TERNILO_E2E_ARTIFACT_DIR ?? directory
  await mkdir(artifacts, { recursive: true })
  const processes = [], sources = [], errors = []
  let browser, local, approval, release
  try {
    const platform = await modelFixture('platform-device'); sources.push(platform)
    const account = await upstream('account-owned-device', 'account-device-private-key'); sources.push(account)
    const server = await initializeServer({ directory: path.join(directory, 'server'), origin: `http://127.0.0.1:${await freePort()}`, mode: 'single_user' }); processes.push(server)
    const access = await seedModels(server, platform)
    const personal = (resource, options = {}) => serverRequest(server.origin, resource, { token: server.owner.session.access_token, tenantId: server.owner.session.personal_tenant_id, ...options })
    const accountProfile = { ...profile(account.baseUrl), id: 'private-source', display_name: 'My private source', api_key_ref: 'ACCOUNT_DEVICE_KEY', models: [{ id: 'account-model', display_name: 'Account Model', settings: { mode: 'inherit' } }] }
    await personal('/credentials', { body: { name: 'ACCOUNT_DEVICE_KEY', value: 'account-device-private-key' } })
    await personal('/providers', { body: accountProfile })
    const origin = `http://127.0.0.1:${await freePort()}`
    const data = path.join(directory, 'local')
    const application = startProcess(process.env.TERNILO_E2E_NODE_BINARY ?? path.join(repository, 'target/debug/ternilo'), ['serve', '--listen', new URL(origin).host, '--data-dir', data], { XDG_STATE_HOME: path.join(directory, 'state') }); processes.push(application)
    await waitForHttp(origin, application)
    const request = await localApi(origin)
    const folder = path.join(directory, 'workspace'); await mkdir(folder)
    const workspace = await request('/workspaces', { body: { path: folder } })
    const session = await request('/sessions', { body: { workspace_id: workspace.workspace_id } })
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
    local = await browser.newPage({ viewport: { width: 1366, height: 950 }, hasTouch: true, serviceWorkers: 'block' })
    approval = await browser.newPage({ viewport: { width: 390, height: 844 }, hasTouch: true, serviceWorkers: 'block' })
    for (const page of [local, approval]) {
      page.on('pageerror', error => errors.push(error.message))
      page.on('console', message => { if (message.type() === 'error') errors.push(message.text()) })
      page.on('response', response => { if (response.status() >= 400) errors.push(`${response.status()} ${new URL(response.url()).pathname}`) })
    }
    await local.goto(origin)
    await approveConnection(local, approval, server, 'Platform only')
    assert.deepEqual((await request('/model-connections'))[0].session.providers, [], 'default authorization does not expand to private models')
    await approveConnection(local, approval, server, 'Private account', undefined, 'private-source')
    const connection = (await request('/model-connections')).find(value => value.name === 'Private account')
    assert.equal(connection.session.grants.length, 0)
    assert.equal(connection.session.providers[0].provider_id, 'private-source')
    const model = (await request('/providers')).find(value => value.id.startsWith('server_a_'))
    assert.ok(model.base_url.startsWith(`${server.origin}/v1/device-account/`))
    assert.equal(await local.evaluate(() => sessionStorage.getItem('ternilo.native.session')), null)
    const stored = await readFile(path.join(data, 'secrets/model-connections.json'), 'utf8')
    for (const secret of ['account-device-private-key', 'ACCOUNT_DEVICE_KEY', account.baseUrl, 'ter_d_']) assert.ok(!stored.includes(secret))
    await chooseModel(local, model.id)
    const submit = async text => {
      const before = (await request(`/sessions/${session.identity.session_id}/events`)).length
      await local.getByRole('textbox', { name: '输入任务', exact: true }).fill(text)
      await local.getByRole('button', { name: '发送', exact: true }).click()
      return before
    }
    const completed = async (before, type) => until(() => request(`/sessions/${session.identity.session_id}/events`), events => events.slice(before).some(event => event.type === type), type)
    await completed(await submit('Write a file with my account-owned model'), 'turn_finished')
    assert.equal(await readFile(path.join(folder, 'provider-source.txt'), 'utf8'), 'account-owned-device')
    assert.equal(platform.calls.length, 0)
    const usage = await access.request('/model-access/requests?source=user_provider&limit=50')
    assert.ok(usage.requests.some(value => value.key_id === connection.session.identity.device_id && value.source === 'user_provider' && value.grant_id === null && value.accounted_tokens > 0))
    await local.locator('[data-model-picker]').click()
    await local.getByRole('menuitem', { name: /^模型/ }).click()
    await local.getByText('账号自有 · 已连接 Server', { exact: true }).waitFor()
    await local.getByText('平台授权 · 已连接 Server', { exact: true }).waitFor()
    await local.screenshot({ path: path.join(artifacts, 'account-device-sources.png') })
    await local.keyboard.press('Escape'); await local.keyboard.press('Escape')
    await until(() => local.getByRole('menu').count(), count => count === 0, 'model menus closed')
    const hold = account.holdNext(body => body.stream && JSON.stringify(body.messages.findLast(message => message.role === 'user')).includes('hold-account-stream'))
    release = hold.release
    const before = await submit('hold-account-stream')
    await Promise.race([hold.began, new Promise((_, reject) => setTimeout(() => reject(new Error('upstream did not start')), 10000))])
    await personal('/credentials/ACCOUNT_DEVICE_KEY', { method: 'DELETE' })
    await completed(before, 'turn_failed')
    release(); release = undefined
    await openModels(local)
    await local.getByRole('button', { name: '刷新 Private account 的模型', exact: true }).click()
    await until(() => request('/providers'), providers => !providers.some(value => value.id === model.id), 'deleted key removed the account catalog')
    await closeModels(local)
    assert.equal((await request('/state')).sessions.find(value => value.identity.session_id === session.identity.session_id).model.provider_id, model.id)
    assert.equal(platform.calls.length, 0, 'never fall back to the same-name platform model')
    await personal('/credentials', { body: { name: 'ACCOUNT_DEVICE_KEY', value: 'account-device-private-key' } })
    await openModels(local)
    await local.getByRole('button', { name: '刷新 Private account 的模型', exact: true }).click()
    await until(() => request('/providers'), providers => providers.some(value => value.id === model.id), 'account model restored')
    await closeModels(local)
    await access.request(`/model-access/devices/${connection.session.identity.device_id}`, { method: 'DELETE' })
    const previousCalls = account.calls.length
    await completed(await submit('Try the revoked account model'), 'turn_failed')
    assert.equal(account.calls.length, previousCalls)
    const publicConnection = (await request('/providers')).find(value => value.display_name.includes('Platform only') && value.display_name.includes('Budget Alpha'))
    await chooseModel(local, publicConnection.id)
    await completed(await submit('Explicitly choose the platform budget'), 'turn_finished')
    assert.ok(platform.calls.length > 0)
    await approval.goto(`${server.origin}/models?tab=access&access=devices`)
    await approval.locator(`[data-model-device="${connection.session.identity.device_id}"]`).getByText('已撤销', { exact: true }).waitFor()
    await approval.locator(`[data-model-device="${connection.session.identity.device_id}"]`).scrollIntoViewIfNeeded()
    await approval.screenshot({ path: path.join(artifacts, 'account-device-revoked-390.png') })
    for (const target of [origin, server.origin]) for (const asset of ['app.js', 'app.css']) {
      const served = Buffer.from(await (await fetch(`${target}/assets/${asset}`)).arrayBuffer())
      const built = await readFile(path.join(repository, 'web/dist/assets', asset))
      assert.equal(createHash('sha256').update(served).digest('hex'), createHash('sha256').update(built).digest('hex'))
    }
    assert.deepEqual(errors, [])
  } catch (error) {
    await local?.screenshot({ path: path.join(artifacts, 'account-device-failure.png') }).catch(() => {})
    await approval?.screenshot({ path: path.join(artifacts, 'account-device-approval-failure.png') }).catch(() => {})
    error.message += `\n${processes.map(process => process.diagnostics()).join('\n')}`
    throw error
  } finally {
    release?.()
    await browser?.close()
    for (const process of processes.reverse()) await stopProcess(process)
    for (const source of sources) await source.close()
    await rm(directory, { recursive: true, force: true })
  }
})
