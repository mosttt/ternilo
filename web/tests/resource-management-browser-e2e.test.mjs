import assert from 'node:assert/strict'
import { mkdir, mkdtemp, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { chromium } from 'playwright'
import { freePort, initializeServer, repository, selectSpace, serverRequest, startProcess, stopProcess, waitForHttp } from './platform-e2e-fixture.mjs'

test('workspace management transfers through the browser while execution ownership and retained access stay separate', { timeout: 120_000 }, async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-resource-management-'))
  let server, node, browser
  try {
    const origin = `http://127.0.0.1:${await freePort()}`
    server = await initializeServer({ directory: path.join(directory, 'server'), origin })
    const token = server.owner.session.access_token
    const registration = await serverRequest(origin, '/admin/registration', { token })
    await serverRequest(origin, '/admin/registration', { token, method: 'PATCH', body: { mode: 'open', require_approval: false, revision: registration.revision } })
    await serverRequest(origin, '/auth/register', { body: { username: 'management-recipient', email: 'recipient@example.test', password: 'management-recipient-password' } })
    const recipient = await serverRequest(origin, '/auth/login', { body: { username: 'management-recipient', password: 'management-recipient-password' } })
    const { tenant } = await serverRequest(origin, '/tenants', { token, body: { slug: 'management-team', display_name: 'Management team' } })
    const scope = { token, tenantId: tenant.tenant_id }
    await serverRequest(origin, `/tenants/${tenant.tenant_id}/members/${recipient.user.user_id}`, { token, method: 'PUT', body: { role: 'member' } })
    const { projects } = await serverRequest(origin, '/projects', scope)
    const { enrollment } = await serverRequest(origin, `/tenants/${tenant.tenant_id}/my-computer-enrollments`, { token, body: { name: 'owner-computer', project_id: null, ttl_seconds: 600 } })
    const enrolledComputerId = enrollment.executor_id
    const { credential } = await serverRequest(origin, '/enrollments/consume', { body: { token: enrollment.token } })
    const local = `http://127.0.0.1:${await freePort()}`
    node = startProcess(process.env.TERNILO_E2E_NODE_BINARY ?? path.join(repository, 'target/debug/ternilo'), [
      'serve', '--listen', new URL(local).host, '--data-dir', path.join(directory, 'node'),
      '--node-id', enrolledComputerId, '--gateway-url', `${origin.replace('http:', 'ws:')}/api/v1/executors/connect`,
      '--token', credential.token, '--allow-insecure-gateway',
    ])
    await waitForHttp(local, node)
    for (let attempt = 0; attempt < 100; attempt += 1) {
      const targets = await serverRequest(origin, '/execution-targets', scope)
      if (targets.executors.some(executor => executor.connected)) break
      await new Promise(resolve => setTimeout(resolve, 100))
    }
    const folder = path.join(directory, 'workspace')
    await mkdir(folder)
    const { workspace } = await serverRequest(origin, '/workspaces', { ...scope, body: { project_id: projects[0].project_id, name: 'Shared workspace', placement: 'local_node', executor_id: enrolledComputerId, path: folder } })
    const session = await serverRequest(origin, '/sessions', { ...scope, body: { workspace_id: workspace.workspace_id } })
    const endpoint = `/workspaces/${workspace.workspace_id}/sharing`
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
    const page = await browser.newPage({ locale: 'zh-CN', viewport: { width: 1365, height: 900 }, serviceWorkers: 'block' })
    const errors = []
    page.on('pageerror', error => errors.push(error.message))
    page.on('console', message => { if (message.type() === 'error') errors.push(message.text()) })
    page.on('response', response => { if (response.status() >= 400) errors.push(`HTTP ${response.status()}: ${new URL(response.url()).pathname}`) })
    await page.goto(origin)
    await page.getByLabel('用户名', { exact: true }).fill(server.owner.username)
    await page.getByLabel('密码', { exact: true }).fill(server.owner.password)
    await page.getByRole('button', { name: '登录', exact: true }).click()
    await selectSpace(page, tenant.tenant_id)
    const row = page.locator('[data-sidebar-workspace-row]').filter({ hasText: 'Shared workspace' })
    await row.hover()
    await row.getByRole('button', { name: /的操作$/ }).click()
    await page.getByRole('menuitem', { name: '共享…', exact: true }).click()
    const dialog = page.getByRole('dialog', { name: '共享工作区', exact: true })
    await dialog.getByRole('button', { name: '转交管理权…', exact: true }).click()
    const transfer = dialog.locator('[data-ownership-transfer]')
    assert.equal(await transfer.getByLabel('保留我为可编辑协作者', { exact: true }).isChecked(), true)
    await transfer.getByLabel('搜索接收者', { exact: true }).fill('management-recipient')
    await transfer.getByRole('button', { name: '搜索', exact: true }).click()
    await transfer.locator(`[data-sharing-candidate="user:${recipient.user.user_id}"]`).click()
    assert.equal(await transfer.getByRole('button', { name: '确认转交', exact: true }).isEnabled(), false)
    await transfer.locator('[data-ownership-confirm]').check()
    const changed = page.waitForResponse(response => new URL(response.url()).pathname === `/api/v1${endpoint}/ownership` && response.request().method() === 'PUT')
    await transfer.getByRole('button', { name: '确认转交', exact: true }).click()
    assert.equal((await changed).status(), 200)
    await dialog.waitFor({ state: 'detached' })
    const retained = await serverRequest(origin, endpoint, scope)
    assert.equal(retained.access.owner_user_id, recipient.user.user_id)
    assert.equal(retained.access.is_owner, false)
    assert.equal(retained.access.can_manage_sharing, false)
    assert.equal(retained.access.is_execution_owner, true)
    assert.equal(retained.access.permissions.configure, true)
    const receiverScope = { token: recipient.access_token, tenantId: tenant.tenant_id }
    const receiver = await serverRequest(origin, endpoint, receiverScope)
    assert.equal(receiver.access.is_owner, true)
    assert.equal(receiver.access.is_execution_owner, false)
    assert.equal(receiver.access.can_manage_sharing, true)
    const inherited = await serverRequest(origin, `/sessions/${session.identity.session_id}/sharing`, receiverScope)
    assert.equal(inherited.access.owner_user_id, recipient.user.user_id)
    const targets = await serverRequest(origin, '/execution-targets', scope)
    assert.ok(targets.executors.some(executor => executor.executor_id === enrolledComputerId && executor.connected))
    const artifacts = process.env.TERNILO_E2E_ARTIFACT_DIR
    if (artifacts) {
      await mkdir(artifacts, { recursive: true })
      await page.screenshot({ path: path.join(artifacts, 'workspace-management-retained-access.png'), fullPage: true })
    }
    assert.deepEqual(errors, [])
  } catch (error) {
    throw new Error(`${error.stack}\n${server?.diagnostics() ?? ''}\n${node?.diagnostics() ?? ''}`)
  } finally {
    await browser?.close()
    await stopProcess(node)
    await stopProcess(server)
    await rm(directory, { recursive: true, force: true })
  }
})
