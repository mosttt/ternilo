import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { mkdir, mkdtemp, readFile, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { chromium } from 'playwright'
import { freePort, initializeServer, repository, startProcess, stopProcess, waitForHttp } from './platform-e2e-fixture.mjs'
import { approveConnection, chooseModel, closeModels, localApi, modelFixture, openModels, seedModels, until } from './model-device-fixture.mjs'

const binary = process.env.TERNILO_E2E_NODE_BINARY ?? path.join(repository, 'target/debug/ternilo')

test('account device login discovers multiple grants and runs local tasks without Node or Worker enrollment', { timeout: 300_000 }, async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-model-device-'))
  const artifacts = process.env.TERNILO_E2E_ARTIFACT_DIR ?? directory
  await mkdir(artifacts, { recursive: true })
  const processes = []
  const errors = []
  let restarting = false
  const environment = { XDG_STATE_HOME: path.join(directory, 'state') }
  let browser, upstream, local, approval
  try {
    upstream = await modelFixture()
    const server = await initializeServer({ directory: path.join(directory, 'server'), origin: `http://127.0.0.1:${await freePort()}`, mode: 'single_user', environment })
    processes.push(server)
    const models = await seedModels(server, upstream)
    const origin = `http://127.0.0.1:${await freePort()}`
    const data = path.join(directory, 'local-data')
    const args = ['serve', '--listen', new URL(origin).host, '--data-dir', data]
    let app = startProcess(binary, args, environment)
    processes.push(app)
    await waitForHttp(origin, app)
    let request = await localApi(origin)
    const folder = path.join(directory, 'workspace')
    await mkdir(folder)
    const workspace = await request('/workspaces', { body: { path: folder } })
    const session = await request('/sessions', { body: { workspace_id: workspace.workspace_id } })
    const sessionId = session.identity.session_id
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
    local = await browser.newPage({ locale: 'zh-CN', viewport: { width: 1280, height: 900 }, hasTouch: true, serviceWorkers: 'block' })
    approval = await browser.newPage({ locale: 'zh-CN', viewport: { width: 320, height: 844 }, hasTouch: true, serviceWorkers: 'block' })
    for (const page of [local, approval]) {
      page.on('pageerror', error => errors.push(error.message))
      page.on('console', message => {
        if (message.type() !== 'error') return
        if (page === local && restarting && /^Failed to load resource:.*(?:ERR_CONNECTION_REFUSED|status of 401)/.test(message.text())) return
        errors.push(message.text())
      })
      page.on('response', response => { if (response.status() >= 400 && !(page === local && restarting && response.status() === 401)) errors.push(`HTTP ${response.status()} ${new URL(response.url()).pathname}`) })
    }
    await local.goto(origin)
    for (const target of [server.origin, origin]) for (const asset of ['app.js', 'app.css']) {
      const served = await (await fetch(`${target}/assets/${asset}`)).arrayBuffer()
      const built = await readFile(path.join(repository, 'web/dist/assets', asset))
      assert.equal(createHash('sha256').update(Buffer.from(served)).digest('hex'), createHash('sha256').update(built).digest('hex'))
    }
    await approveConnection(local, approval, server, 'My platform')
    let connections = await request('/model-connections')
    assert.equal(connections.length, 1)
    assert.equal(connections[0].session.grants.length, 2)
    const connectionId = connections[0].connection_id
    const deviceId = connections[0].session.identity.device_id
    let providers = await request('/providers')
    assert.equal(providers.length, 2)
    const alpha = providers.find(provider => provider.display_name.includes('Budget Alpha'))
    const beta = providers.find(provider => provider.display_name.includes('Budget Beta'))
    assert.ok(alpha && beta && alpha.id !== beta.id)
    assert.match(alpha.base_url, new RegExp(models.first.grant_id))
    await chooseModel(local, alpha.id)
    await local.getByRole('textbox', { name: '输入任务', exact: true }).fill('Create a file through the account model')
    await local.getByRole('button', { name: '发送', exact: true }).click()
    await local.locator('article[data-role="assistant"]').filter({ hasText: 'The account model completed this local task.' }).waitFor()
    assert.equal(await readFile(path.join(folder, 'model-device-proof.txt'), 'utf8'), 'account-device-task')
    const usage = await models.request('/model-access/requests?limit=50')
    assert.ok(usage.requests.some(item => item.origin === 'client_device' && item.grant_id === models.first.grant_id && item.key_id === deviceId && item.accounted_tokens > 0))
    assert.equal((await models.request('/model-access/keys?limit=50')).keys.length, 0)
    assert.equal((await models.request('/execution-targets', { tenantId: server.owner.session.personal_tenant_id })).executors.length, 0)
    const third = await models.grant('Budget Gamma')
    await openModels(local)
    await local.getByRole('button', { name: '刷新 My platform 的模型', exact: true }).click()
    await until(() => request('/providers'), result => result.length === 3, 'new grant discovered without login')
    connections = await request('/model-connections')
    assert.equal(connections.length, 1)
    assert.equal(connections[0].session.identity.device_id, deviceId)
    await local.screenshot({ path: path.join(artifacts, 'device-connected-desktop.png') })
    await closeModels(local)
    await models.request(`/admin/models/grants/${models.first.grant_id}`, { method: 'DELETE' })
    await openModels(local)
    await local.getByRole('button', { name: '刷新 My platform 的模型', exact: true }).click()
    await until(() => request('/providers'), result => result.length === 2, 'one grant removed without disconnecting the account')
    await closeModels(local)
    restarting = true
    await stopProcess(app)
    app = startProcess(binary, args, environment)
    processes.push(app)
    await waitForHttp(origin, app)
    request = await localApi(origin)
    await local.reload()
    restarting = false
    const state = await request('/state')
    assert.equal(state.sessions.find(item => item.identity.session_id === sessionId).model.provider_id, alpha.id, 'revocation and restart do not silently select another budget')
    await local.getByText('当前模型不可用', { exact: true }).waitFor()
    await chooseModel(local, beta.id)
    await local.getByRole('textbox', { name: '输入任务', exact: true }).fill('Continue with the explicitly selected second budget')
    await local.getByRole('button', { name: '发送', exact: true }).click()
    await until(async () => local.locator('article[data-role="assistant"]').filter({ hasText: 'The account model completed this local task.' }).count(), count => count >= 2, 'second allowance works')
    await approveConnection(local, approval, server, 'Restricted device', third.grant_id)
    connections = await request('/model-connections')
    assert.equal(connections.find(entry => entry.name === 'Restricted device').session.grants.length, 1)
    await approval.goto(`${server.origin}/models`)
    await approval.getByRole('link', { name: '授权与接入', exact: true }).click()
    await approval.getByRole('tab', { name: '模型授权设备', exact: true }).click()
    await approval.locator(`[data-model-device="${deviceId}"]`).getByRole('button', { name: '撤销', exact: true }).tap()
    await approval.getByRole('dialog').getByRole('button', { name: '撤销', exact: true }).tap()
    await approval.getByRole('dialog').waitFor({ state: 'hidden' })
    await approval.screenshot({ path: path.join(artifacts, 'device-directory-320.png') })
    const stored = await readFile(path.join(data, 'secrets/model-connections.json'), 'utf8')
    assert.ok(!stored.includes('ter_d_'), 'connection metadata contains no bearer credentials')
    assert.ok(upstream.calls.length >= 4)
    assert.deepEqual(errors, [])
  } catch (error) {
    await local?.screenshot({ path: path.join(artifacts, 'device-local-failure.png'), fullPage: true }).catch(() => {})
    await approval?.screenshot({ path: path.join(artifacts, 'device-server-failure.png'), fullPage: true }).catch(() => {})
    error.message += `\n${processes.map(item => item.diagnostics()).join('\n')}`
    throw error
  } finally {
    await browser?.close()
    for (const process of processes.reverse()) await stopProcess(process)
    await upstream?.close()
    await rm(directory, { recursive: true, force: true })
  }
})
