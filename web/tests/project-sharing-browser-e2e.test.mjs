import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { mkdir, mkdtemp, readFile, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { chromium } from 'playwright'
import { selectChoice } from './browser-select-fixture.mjs'
import { freePort, initializeServer, repository, selectSpace, serverRequest, startProcess, stopProcess, waitForHttp } from './platform-e2e-fixture.mjs'
import { localApi, until } from './model-device-fixture.mjs'

async function login(page, origin, account, tenantId, destination = '/') {
  await page.goto(`${origin}${destination}`)
  await page.getByLabel('用户名', { exact: true }).fill(account.username)
  await page.getByLabel('密码', { exact: true }).fill(account.password)
  await page.getByRole('button', { name: '登录', exact: true }).click()
  await selectSpace(page, tenantId)
}

async function workspaceSharing(page, title) {
  const row = page.locator('[data-sidebar-workspace-row]').filter({ hasText: title })
  await row.hover()
  await row.getByRole('button', { name: /的操作$/ }).click()
  await page.getByRole('menuitem', { name: '共享…', exact: true }).click()
  const dialog = page.getByRole('dialog', { name: '共享工作区', exact: true })
  await dialog.locator('[data-project-inheritance]').waitFor()
  return dialog
}

async function layout(page, dialog, artifacts, name) {
  await page.waitForFunction(() => Number.parseFloat(document.documentElement.style.getPropertyValue('--ternilo-visual-viewport-height')) === Math.round(window.visualViewport?.height ?? innerHeight))
  await dialog.evaluate(async element => {
    await Promise.all(element.getAnimations({ subtree: true }).filter(animation => animation.effect?.getTiming().iterations !== Infinity).map(animation => animation.finished.catch(() => {})))
  })
  assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth))
  assert.ok(await dialog.evaluate(element => element.scrollWidth <= element.clientWidth))
  const bounds = await dialog.boundingBox()
  assert.ok(bounds && bounds.x >= 0 && bounds.y >= 0 && bounds.x + bounds.width <= page.viewportSize().width + 1 && bounds.y + bounds.height <= page.viewportSize().height + 1)
  await page.screenshot({ path: path.join(artifacts, `${name}.png`) })
}

test('project rules require workspace owner opt-in and revoke live, fork and archive access without becoming direct grants', { timeout: 240_000 }, async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-project-sharing-'))
  const artifacts = process.env.TERNILO_E2E_ARTIFACT_DIR ?? directory
  await mkdir(artifacts, { recursive: true })
  const environment = { XDG_STATE_HOME: path.join(directory, 'state') }
  const processes = [], errors = [], networkErrors = [], consoleErrors = [], denials = []
  let browser, adminPage, ownerPage, readerPage
  try {
    const origin = `http://127.0.0.1:${await freePort()}`
    const server = await initializeServer({ directory: path.join(directory, 'server'), origin, environment })
    processes.push(server)
    const admin = (resource, options = {}) => serverRequest(origin, resource, { token: server.owner.session.access_token, ...options })
    const tenantId = (await admin('/tenants', { body: { slug: 'project-sharing', display_name: 'Project sharing' } })).tenant.tenant_id
    const member = async username => {
      const invitation = await admin('/admin/invitations', { body: { tenant_id: null, role: 'member', expires_in_seconds: 3600 } })
      const account = { username, password: `${username}-password`, email: `${username}@example.test` }
      const identity = await serverRequest(origin, '/auth/invitations/accept', { body: { ...account, token: invitation.token } })
      await admin(`/tenants/${tenantId}/members/${identity.user.user_id}`, { method: 'PUT', body: { role: 'member' } })
      return { ...account, identity, request: (resource, options = {}) => serverRequest(origin, resource, { token: identity.access_token, tenantId, ...options }) }
    }
    const machineOwner = await member('machine-owner'), reader = await member('project-reader')
    const project = (await machineOwner.request('/projects')).projects[0]
    await admin(`/projects/${project.project_id}`, { tenantId, method: 'PATCH', body: { name: 'Shared project' } })
    const group = await admin(`/tenants/${tenantId}/groups`, { body: { name: 'Project contributors', description: null } })
    await admin(`/tenants/${tenantId}/groups/${group.group_id}/members/${reader.identity.user.user_id}`, { method: 'PUT' })
    const enrollment = (await machineOwner.request(`/tenants/${tenantId}/my-computer-enrollments`, { body: { name: 'project-node', project_id: project.project_id, ttl_seconds: 600 } })).enrollment
    const enrolledComputerId = enrollment.executor_id
    const credential = (await machineOwner.request('/enrollments/consume', { body: { token: enrollment.token } })).credential
    const nodeOrigin = `http://127.0.0.1:${await freePort()}`
    const node = startProcess(process.env.TERNILO_E2E_NODE_BINARY ?? path.join(repository, 'target/debug/ternilo'), ['serve', '--listen', new URL(nodeOrigin).host, '--data-dir', path.join(directory, 'node'), '--node-id', enrolledComputerId, '--gateway-url', `${origin.replace('http:', 'ws:')}/api/v1/executors/connect`, '--allow-insecure-gateway'], { ...environment, TERNILO_LOCAL_TOKEN: credential.token })
    processes.push(node); await waitForHttp(nodeOrigin, node)
    for (const target of [origin, nodeOrigin]) for (const asset of ['app.js', 'app.css']) {
      const served = Buffer.from(await (await fetch(`${target}/assets/${asset}`)).arrayBuffer())
      const built = await readFile(path.join(repository, 'web/dist/assets', asset))
      assert.equal(createHash('sha256').update(served).digest('hex'), createHash('sha256').update(built).digest('hex'))
    }
    const local = await localApi(nodeOrigin)
    const folder = path.join(directory, 'workspace'); await mkdir(folder)
    const workspace = await local('/workspaces', { body: { path: folder } })
    const session = await local('/sessions', { body: { workspace_id: workspace.workspace_id } })
    const localId = session.identity.session_id
    await local(`/sessions/${localId}`, { method: 'PATCH', body: { title: 'Project inherited conversation' } })
    const runLocal = async (runId, input) => {
      await local(`/sessions/${localId}/queue`, { body: { run_id: runId, content: { kind: 'prompt', input } } })
      await until(() => local(`/sessions/${localId}/events`), events => events.some(event => event.run_id === runId && event.type === 'turn_finished'), `${runId} finished`)
    }
    await runLocal('seed-project', '/write project.txt shared-project-proof')
    const state = await until(() => machineOwner.request('/state'), value => value.sessions.length === 1, 'owned Node discovered')
    const publicId = state.sessions[0].identity.session_id, workspaceId = state.workspaces[0].workspace_id
    assert.equal((await admin('/state', { tenantId })).sessions.length, 0)
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
    ;[adminPage, ownerPage, readerPage] = await Promise.all([0, 1, 2].map(async () => {
      const context = await browser.newContext({ locale: 'zh-CN', viewport: { width: 1280, height: 900 }, serviceWorkers: 'block' })
      const page = await context.newPage()
      page.on('pageerror', error => errors.push(error.message))
      page.on('console', message => { if (message.type() === 'error') consoleErrors.push({ url: message.location().url, text: message.text() }) })
      page.on('response', response => { if (response.status() >= 400) networkErrors.push({ url: response.url(), status: response.status() }) })
      return page
    }))
    let batches = 0, revoking = false
    readerPage.on('websocket', socket => socket.on('framereceived', frame => {
      try { const value = JSON.parse(String(frame.payload)); if (value.type === 'event_batch' && value.session_id === publicId) batches++ } catch {}
    }))
    await readerPage.route(url => url.origin === origin && (
      ['queue', 'commands', 'history', 'stats', 'projection', 'plugins'].some(resource => url.pathname === `/api/v1/sessions/${publicId}/${resource}`)
      || ['/api/v1/catalog', '/api/v1/model-options'].includes(url.pathname) && url.searchParams.get('session_id') === publicId), async route => {
      if (route.request().method() !== 'GET') { await route.continue(); return }
      const response = await route.fetch()
      if (revoking && [400, 403].includes(response.status())) {
        const expected = response.status() === 400 ? { code: 'invalid_input', message: 'session does not exist' }
          : { code: 'policy_denied', message: 'this resource has not been shared with the requested permission' }
        try { assert.deepEqual(await response.json(), { error: expected }); denials.push({ url: route.request().url(), status: response.status() }) }
        catch (error) { errors.push(error.message) }
      }
      await route.fulfill({ response })
    })
    await login(adminPage, origin, server.owner, tenantId, '/spaces/current?tab=projects')
    await adminPage.locator(`[data-project-id="${project.project_id}"]`).getByRole('button', { name: '项目共享规则', exact: true }).click()
    const rules = adminPage.getByRole('dialog', { name: '项目共享规则', exact: true })
    await selectChoice(rules.locator('#sharing-kind'), 'group')
    await rules.locator(`[data-sharing-candidate="group:${group.group_id}"]`).click()
    await rules.getByLabel('发送任务', { exact: true }).check()
    await rules.getByLabel('停止任务', { exact: true }).check()
    await rules.getByRole('button', { name: '保存共享权限', exact: true }).click()
    await rules.locator(`[data-sharing-grant="group:${group.group_id}"]`).waitFor()
    assert.equal((await reader.request('/state')).sessions.length, 0, 'project rules leave existing resources private until owner opt-in')
    await assert.rejects(admin(`/workspaces/${workspaceId}/project-sharing`, { tenantId, method: 'PUT', body: { enabled: true } }), /403/)
    await login(ownerPage, origin, machineOwner, tenantId)
    const ownerSharing = await workspaceSharing(ownerPage, state.workspaces[0].title)
    const toggle = ownerSharing.locator('[data-project-inheritance] input')
    const inherit = async enabled => {
      const saved = ownerPage.waitForResponse(response => response.request().method() === 'PUT' && new URL(response.url()).pathname === `/api/v1/workspaces/${workspaceId}/project-sharing`)
      await toggle.click()
      const response = await saved
      assert.equal(response.status(), 200)
      assert.equal((await response.json()).enabled, enabled)
      await until(() => toggle.isChecked(), value => value === enabled, 'confirmed inheritance state')
    }
    assert.equal(await toggle.isChecked(), false)
    await ownerSharing.getByRole('button', { name: '查看项目规则', exact: true }).click()
    const inspectedRules = ownerPage.getByRole('dialog', { name: '项目共享规则', exact: true })
    await inspectedRules.locator(`[data-sharing-grant="group:${group.group_id}"]`).waitFor()
    assert.equal(await inspectedRules.locator('[data-sharing-candidates]').count(), 0)
    assert.equal(await inspectedRules.locator('[data-sharing-grant] button').count(), 0)
    await inspectedRules.locator('[data-slot="dialog-close"]').click()
    await inspectedRules.waitFor({ state: 'hidden' })
    await inherit(true)
    await until(() => reader.request('/state'), value => value.sessions.some(value => value.identity.session_id === publicId), 'project sharing appears in member workbench')
    assert.equal((await admin('/state', { tenantId })).sessions.length, 0, 'project administrator has no implicit resource access')
    const inherited = (await reader.request(`/sessions/${publicId}/sharing`)).access
    assert.equal(inherited.owner_user_id, machineOwner.identity.user.user_id)
    assert.equal(inherited.can_manage_sharing, false)
    assert.ok(inherited.sources.some(source => source.resource_kind === 'project' && source.group_id === group.group_id))
    await assert.rejects(reader.request(`/workspaces/${workspaceId}/project-sharing`, { method: 'PUT', body: { enabled: false } }), /403/)
    await login(readerPage, origin, reader, tenantId)
    await readerPage.locator(`[data-sidebar-session-row][data-session-id="${publicId}"] [data-sidebar-session-button]`).click()
    await readerPage.getByRole('textbox', { name: '输入任务', exact: true }).fill('/read project.txt')
    await readerPage.getByRole('button', { name: '发送', exact: true }).click()
    await readerPage.locator('[data-tool-call-toggle]').filter({ hasText: '读取文件' }).click()
    await readerPage.locator('[data-tool-view="read"]').getByText('shared-project-proof', { exact: false }).waitFor()
    assert.ok((await reader.request('/files')).items.some(item => item.session_id === publicId && item.name === 'project.txt'))
    const fork = await reader.request(`/sessions/${publicId}/fork`, { body: {} })
    const forkId = fork.identity.session_id
    await machineOwner.request(`/sessions/${forkId}/archive`, { method: 'POST' })
    assert.ok((await reader.request(`/sessions/${forkId}/archive-history`)).events.length > 0)
    for (const width of [390, 320]) {
      await adminPage.setViewportSize({ width, height: 844 })
      await layout(adminPage, rules, artifacts, `project-rules-${width}`)
      await ownerPage.setViewportSize({ width, height: 844 })
      await layout(ownerPage, ownerSharing, artifacts, `project-inheritance-${width}`)
    }
    revoking = true
    await rules.getByRole('button', { name: '移除“Project contributors”的共享权限', exact: true }).click()
    await rules.locator('[data-sharing-grant]').waitFor({ state: 'detached' })
    await readerPage.locator(`[data-sidebar-session-row][data-session-id="${publicId}"]`).waitFor({ state: 'detached' })
    assert.equal(await readerPage.locator('article[data-role="user"]').count(), 0)
    assert.equal(await readerPage.locator('[data-tool-view="read"]').count(), 0)
    await assert.rejects(reader.request(`/sessions/${publicId}/history`), /400|403/)
    await assert.rejects(reader.request(`/sessions/${forkId}/archive-history`), /400|403/)
    assert.deepEqual(await reader.request('/sessions/archived'), [])
    assert.deepEqual((await reader.request('/files')).items, [])
    const stoppedAt = batches
    await runLocal('owner-after-revocation', '/read project.txt')
    assert.equal(batches, stoppedAt, 'revoked members receive no later events from owner tasks')
    assert.ok((await machineOwner.request(`/sessions/${publicId}/history`)).events.length > 0)
    await inherit(false)
    await selectChoice(rules.locator('#sharing-kind'), 'user')
    await rules.locator(`[data-sharing-candidate="user:${reader.identity.user.user_id}"]`).click()
    await rules.getByRole('button', { name: '保存共享权限', exact: true }).click()
    await rules.locator(`[data-sharing-grant="user:${reader.identity.user.user_id}"]`).waitFor()
    assert.equal((await reader.request('/state')).sessions.length, 0, 'disabled inheritance remains disabled when project rules change')
    await inherit(true)
    await until(() => reader.request('/state'), value => value.sessions.some(value => value.identity.session_id === publicId), 'explicit re-enrollment follows current project rules')
    assert.equal((await reader.request(`/sessions/${publicId}/sharing`)).access.permissions.submit, false)
    await readerPage.unrouteAll({ behavior: 'wait' })
    for (const error of networkErrors) assert.ok(denials.some(value => value.url === error.url && value.status === error.status), JSON.stringify(error))
    for (const error of consoleErrors) assert.ok(denials.some(value => value.url === error.url && error.text === `Failed to load resource: the server responded with a status of ${value.status} (${value.status === 400 ? 'Bad Request' : 'Forbidden'})`), JSON.stringify(error))
    assert.deepEqual(errors, [])
  } catch (error) {
    for (const [name, page] of [['admin', adminPage], ['owner', ownerPage], ['reader', readerPage]]) {
      await page?.screenshot({ path: path.join(artifacts, `project-sharing-${name}-failure.png`) }).catch(() => {})
    }
    error.message += `\n${processes.map(process => process.diagnostics()).join('\n')}`
    throw error
  } finally {
    await browser?.close()
    for (const process of processes.reverse()) await stopProcess(process)
    await rm(directory, { recursive: true, force: true })
  }
})
