import assert from 'node:assert/strict'
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { chromium } from 'playwright'
import { freePort, initializeServer, repository, selectSpace, serverRequest, startProcess, stopProcess, waitForHttp } from './platform-e2e-fixture.mjs'
import { localApi, until } from './model-device-fixture.mjs'

test('sharing changes on one Server update and revoke another Server browser without reloading', { timeout: 180_000 }, async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-resource-notifications-'))
  const artifacts = process.env.TERNILO_E2E_ARTIFACT_DIR ?? directory
  await mkdir(artifacts, { recursive: true })
  const processes = [], errors = [], responses = [], consoleErrors = []
  let browser, page, revoking = false, sessionId
  try {
    const firstOrigin = `http://127.0.0.1:${await freePort()}`
    const secondOrigin = `http://127.0.0.1:${await freePort()}`
    const first = await initializeServer({ directory: path.join(directory, 'first'), origin: firstOrigin })
    processes.push(first)
    const config = JSON.parse(await readFile(first.configPath, 'utf8'))
    config.listen = new URL(secondOrigin).host
    config.public_url = secondOrigin
    const secondConfig = path.join(directory, 'second.json')
    await writeFile(secondConfig, JSON.stringify(config), { mode: 0o600 })
    const environment = Object.fromEntries(Object.keys(process.env).filter(key => key.startsWith('TERNILO_')).map(key => [key, undefined]))
    const second = startProcess(process.env.TERNILO_E2E_SERVER_BINARY ?? path.join(repository, 'target/debug/ternilo-server'), ['serve', '--config-dir', path.dirname(secondConfig)], environment)
    processes.push(second)
    await waitForHttp(`${secondOrigin}/readyz`, second)
    const ownerToken = first.owner.session.access_token
    const admin = (resource, options = {}) => serverRequest(firstOrigin, resource, { token: ownerToken, ...options })
    const tenantId = (await admin('/tenants', { body: { slug: 'resource-notifications', display_name: 'Resource notifications' } })).tenant.tenant_id
    const invitation = await admin('/admin/invitations', { body: { tenant_id: null, role: 'member', expires_in_seconds: 3600 } })
    const account = { username: 'remote-reader', email: 'remote-reader@example.test', password: 'remote-reader-password' }
    const reader = await serverRequest(firstOrigin, '/auth/invitations/accept', { body: { ...account, token: invitation.token } })
    await admin(`/tenants/${tenantId}/members/${reader.user.user_id}`, { method: 'PUT', body: { role: 'member' } })
    const owner = (resource, options = {}) => admin(resource, { tenantId, ...options })
    const remote = (resource, options = {}) => serverRequest(secondOrigin, resource, { token: reader.access_token, tenantId, ...options })
    const project = (await owner('/projects')).projects[0]
    const { enrollment } = await owner(`/tenants/${tenantId}/my-computer-enrollments`, { body: { name: 'second-server-node', project_id: project.project_id, ttl_seconds: 600 } })
    const enrolledComputerId = enrollment.executor_id
    const { credential } = await owner('/enrollments/consume', { body: { token: enrollment.token } })
    const nodeOrigin = `http://127.0.0.1:${await freePort()}`
    // Node and reader use Server B; all permission mutations use Server A.
    // This checks distributed notifications independently of cross-Server RPC routing.
    const node = startProcess(process.env.TERNILO_E2E_NODE_BINARY ?? path.join(repository, 'target/debug/ternilo'), [
      'serve', '--listen', new URL(nodeOrigin).host, '--data-dir', path.join(directory, 'node'),
      '--node-id', enrolledComputerId, '--gateway-url', `${secondOrigin.replace('http:', 'ws:')}/api/v1/executors/connect`, '--allow-insecure-gateway',
    ], { ...environment, TERNILO_LOCAL_TOKEN: credential.token })
    processes.push(node)
    await waitForHttp(nodeOrigin, node)
    const local = await localApi(nodeOrigin)
    const folder = path.join(directory, 'workspace')
    await mkdir(folder)
    const workspace = await local('/workspaces', { body: { path: folder } })
    const session = await local('/sessions', { body: { workspace_id: workspace.workspace_id } })
    const localId = session.identity.session_id
    await local(`/sessions/${localId}`, { method: 'PATCH', body: { title: 'Cross-server conversation' } })
    const run = async (runId, input) => {
      await local(`/sessions/${localId}/queue`, { body: { run_id: runId, content: { kind: 'prompt', input } } })
      await until(() => local(`/sessions/${localId}/events`), events => events.some(event => event.run_id === runId && event.type === 'turn_finished'), `${runId} finished`)
    }
    await run('before-share', '/write cross-server.txt cross-server-proof')
    const state = await until(() => serverRequest(secondOrigin, '/state', { token: ownerToken, tenantId }), value => value.sessions.length === 1, 'Node discovered by Server B')
    sessionId = state.sessions[0].identity.session_id
    const workspaceId = state.workspaces[0].workspace_id
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
    page = await browser.newPage({ viewport: { width: 1280, height: 900 }, serviceWorkers: 'block' })
    page.on('pageerror', error => errors.push(error.message))
    page.on('response', response => {
      if (response.status() >= 400) responses.push({ url: response.url(), status: response.status(), revoked: revoking, body: response.json() })
    })
    page.on('console', message => { if (message.type() === 'error') consoleErrors.push({ text: message.text(), url: message.location().url }) })
    let batches = 0, workbenches = 0
    page.on('websocket', socket => socket.on('framereceived', frame => {
      const value = JSON.parse(String(frame.payload))
      if (value.type === 'event_batch' && value.session_id === sessionId) batches++
      if (value.type === 'workbench') workbenches++
    }))
    await page.goto(secondOrigin)
    await page.getByLabel('用户名', { exact: true }).fill(account.username)
    await page.getByLabel('密码', { exact: true }).fill(account.password)
    await page.getByRole('button', { name: '登录', exact: true }).click()
    await selectSpace(page, tenantId)
    await until(() => workbenches, count => count > 0, 'reader receives initial workbench')
    const row = page.locator(`[data-sidebar-session-row][data-session-id="${sessionId}"]`)
    assert.equal(await row.count(), 0)
    const read = { view: true, submit: false, stop: false, configure: false }
    const share = `/workspaces/${workspaceId}/sharing/user/${reader.user.user_id}`
    await owner(share, { method: 'PUT', body: read })
    await row.waitFor()
    await row.locator('[data-sidebar-session-button]').click()
    await page.locator('article[data-role="user"]').first().waitFor()
    await until(() => batches, count => count > 0, 'authorized history delivered')
    revoking = true
    await owner(share, { method: 'DELETE' })
    await row.waitFor({ state: 'detached' })
    await until(() => page.locator('article[data-role="user"]').count(), count => count === 0, 'revoked history cleared')
    await assert.rejects(remote(`/sessions/${sessionId}/history`), /400|403/)
    const stoppedAt = batches
    await run('after-revoke', '/read cross-server.txt')
    assert.equal(batches, stoppedAt, 'revoked subscription receives no subsequent Node events')
    const group = await owner(`/tenants/${tenantId}/groups`, { body: { name: 'Distributed reviewers', description: null } })
    const memberPath = `/tenants/${tenantId}/groups/${group.group_id}/members/${reader.user.user_id}`
    await owner(memberPath, { method: 'PUT' })
    await owner(`/workspaces/${workspaceId}/sharing/group/${group.group_id}`, { method: 'PUT', body: read })
    await row.waitFor()
    await row.locator('[data-sidebar-session-button]').click()
    await page.locator('article[data-role="user"]').first().waitFor()
    await owner(memberPath, { method: 'DELETE' })
    await row.waitFor({ state: 'detached' })
    const projectShare = `/projects/${project.project_id}/sharing/user/${reader.user.user_id}`
    await owner(projectShare, { method: 'PUT', body: read })
    assert.equal(await row.count(), 0)
    await owner(`/workspaces/${workspaceId}/project-sharing`, { method: 'PUT', body: { enabled: true } })
    await row.waitFor()
    await row.locator('[data-sidebar-session-button]').click()
    await page.locator('article[data-role="user"]').first().waitFor()
    await page.screenshot({ path: path.join(artifacts, 'cross-server-shared-workspace.png'), animations: 'disabled' })
    await owner(projectShare, { method: 'DELETE' })
    await row.waitFor({ state: 'detached' })
    await until(() => page.locator('article[data-role="user"]').count(), count => count === 0, 'project revocation clears history')
    await page.screenshot({ path: path.join(artifacts, 'cross-server-revoked-workspace.png'), animations: 'disabled' })
    assert.deepEqual(errors, [])
    for (const response of responses) {
      const url = new URL(response.url)
      assert.ok(response.revoked && [400, 403].includes(response.status)
        && (url.pathname.startsWith(`/api/v1/sessions/${sessionId}/`) || url.searchParams.get('session_id') === sessionId
          || url.pathname === '/api/v1/catalog' && url.searchParams.get('workspace_id') === workspaceId), JSON.stringify(response))
      assert.ok(['policy_denied', 'invalid_input'].includes((await response.body).error?.code))
    }
    for (const error of consoleErrors) assert.ok(responses.some(response => response.url === error.url
      && error.text === `Failed to load resource: the server responded with a status of ${response.status} (${response.status === 400 ? 'Bad Request' : 'Forbidden'})`), JSON.stringify(error))
  } catch (error) {
    await page?.screenshot({ path: path.join(artifacts, 'resource-notifications-failure.png') }).catch(() => {})
    error.message += `\n${processes.map(process => process.diagnostics()).join('\n')}`
    throw error
  } finally {
    await browser?.close()
    for (const process of processes.reverse()) await stopProcess(process)
    await rm(directory, { recursive: true, force: true })
  }
})
