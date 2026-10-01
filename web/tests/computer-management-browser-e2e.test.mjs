import assert from 'node:assert/strict'
import { mkdir, mkdtemp, readFile, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { chromium } from 'playwright'
import { freePort, initializeServer, repository, serverRequest, startProcess, stopProcess, waitForHttp } from './platform-e2e-fixture.mjs'

async function until(read, predicate, message, timeout = 30_000) {
  const deadline = Date.now() + timeout
  while (Date.now() < deadline) {
    const value = await read()
    if (predicate(value)) return value
    await new Promise(resolve => setTimeout(resolve, 100))
  }
  throw new Error(message)
}

test('computer management edits metadata, suspends and resumes the same Node, and removes registration while retaining history', { timeout: 150_000 }, async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-computer-management-'))
  let server, node, browser, page
  try {
    const origin = `http://127.0.0.1:${await freePort()}`
    server = await initializeServer({ directory: path.join(directory, 'server'), origin })
    assert.ok(Buffer.from(await (await fetch(`${origin}/assets/app.js`)).arrayBuffer()).equals(await readFile(path.join(repository, 'web/dist/assets/app.js'))), 'Server must embed the current web build')
    const identity = server.owner.session
    const scope = { token: identity.access_token, tenantId: identity.personal_tenant_id }
    const endpoint = `/tenants/${scope.tenantId}/my-computers/home`
    const { enrollment } = await serverRequest(origin, `/tenants/${scope.tenantId}/my-computer-enrollments`, { token: scope.token, body: { executor_id: 'home', ttl_seconds: 600 } })
    const { credential } = await serverRequest(origin, '/enrollments/consume', { body: { token: enrollment.token } })
    const localOrigin = `http://127.0.0.1:${await freePort()}`
    const folder = path.join(directory, 'Project')
    await mkdir(folder)
    node = startProcess(process.env.TERNILO_E2E_NODE_BINARY ?? path.join(repository, 'target/debug/ternilo'), [
      'serve', '--listen', new URL(localOrigin).host, '--data-dir', path.join(directory, 'node'), '--node-id', 'home',
      '--gateway-url', `${origin.replace('http:', 'ws:')}/api/v1/executors/connect`, '--token', credential.token, '--allow-insecure-gateway',
    ])
    await waitForHttp(localOrigin, node)
    await until(() => serverRequest(origin, endpoint, scope), value => value.connected, 'Node must connect')
    const { workspace } = await serverRequest(origin, '/workspaces', { ...scope, body: { project_id: identity.personal_project_id, name: 'Project', placement: 'local_node', executor_id: 'home', path: folder } })
    const session = await serverRequest(origin, '/sessions', { ...scope, body: { workspace_id: workspace.workspace_id } })
    const sessionId = session.identity.session_id
    await serverRequest(origin, `/sessions/${sessionId}/queue`, { ...scope, body: { content: { kind: 'prompt', input: '/write retained.txt management-history-retained' } } })
    await until(() => readFile(path.join(folder, 'retained.txt'), 'utf8').catch(() => ''), value => value === 'management-history-retained', 'seed a completed file command')
    await until(() => serverRequest(origin, `/sessions/${sessionId}/history?limit=1000`, scope), value => value.events.some(event => event.type === 'command_finished'), 'history must be replicated before management actions')

    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
    page = await browser.newPage({ viewport: { width: 1365, height: 900 }, serviceWorkers: 'block' })
    const errors = [], detailRequests = []
    page.on('pageerror', error => errors.push(error.message))
    page.on('console', message => { if (message.type() === 'error') errors.push(message.text()) })
    page.on('response', response => { if (response.status() >= 400) errors.push(`HTTP ${response.status()} ${new URL(response.url()).pathname}`) })
    page.on('request', request => { if (new URL(request.url()).pathname === `/api/v1${endpoint}` && request.method() === 'GET') detailRequests.push(request.url()) })
    await page.goto(`${origin}/settings/computers`)
    await page.getByLabel('用户名', { exact: true }).fill(server.owner.username)
    await page.getByLabel('密码', { exact: true }).fill(server.owner.password)
    await page.getByRole('button', { name: '登录', exact: true }).click()
    const card = page.locator('[data-platform-computer="home"]')
    await card.waitFor()
    assert.deepEqual(detailRequests, [], 'details must not load with the list')
    await card.getByRole('button', { name: '详情与编辑', exact: true }).click()
    const dialog = page.locator('[data-computer-details]')
    await dialog.getByLabel('显示名称', { exact: true }).fill('开发电脑')
    await dialog.getByLabel('备注', { exact: true }).fill('本轮测试电脑')
    assert.equal(detailRequests.length, 1)
    await dialog.getByRole('button', { name: '保存电脑信息', exact: true }).click()
    await dialog.waitFor({ state: 'detached' })
    await card.getByText('开发电脑', { exact: true }).waitFor()
    assert.equal(await card.getByText('home', { exact: true }).count(), 0, 'registration ID belongs in the detail view')
    const updated = await serverRequest(origin, endpoint, scope)
    assert.equal(updated.details.management.display_name, '开发电脑')
    assert.equal(updated.details.executor.executor_id, 'home')
    assert.equal(updated.details.workspace_count, 1)
    assert.equal(updated.details.session_count, 1)
    assert.ok(updated.details.hello)

    await card.getByRole('button', { name: '暂停接入', exact: true }).click()
    await page.getByRole('dialog', { name: '暂停这台电脑接入？', exact: true }).getByRole('button', { name: '暂停接入', exact: true }).click()
    await card.getByText('已暂停接入', { exact: true }).waitFor()
    await until(() => serverRequest(origin, endpoint, scope), value => !value.connected, 'suspended Node must be disconnected')
    assert.equal(node.child.exitCode, null, 'suspending Server access must retain the local service')
    assert.equal((await fetch(localOrigin)).status, 200)
    await card.getByRole('button', { name: '恢复接入', exact: true }).click()
    await page.getByRole('dialog', { name: '恢复这台电脑接入？', exact: true }).getByRole('button', { name: '恢复接入', exact: true }).click()
    await until(() => serverRequest(origin, endpoint, scope), value => value.connected, 'same running Node must reconnect without a new credential')
    await page.getByRole('button', { name: '刷新', exact: true }).click()
    await card.getByText('在线', { exact: true }).waitFor()
    await page.getByRole('button', { name: '返回工作台', exact: true }).click()
    await page.locator('[data-sidebar-computer-group="node:home"] [data-sidebar-computer-title]').getByText('电脑 开发电脑', { exact: true }).waitFor()
    await page.locator(`[data-sidebar-session-row][data-session-id="${sessionId}"] [data-sidebar-session-button]`).click()
    await page.locator('[data-session-workspace]').filter({ hasText: '电脑 开发电脑 · Project' }).waitFor()
    await page.getByRole('button', { name: '用户设置', exact: true }).click()
    await page.getByRole('button', { name: '我的机器', exact: true }).click()
    await card.waitFor()
    await page.setViewportSize({ width: 390, height: 844 })
    assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), 'mobile computer actions must fit')
    const artifacts = process.env.TERNILO_E2E_ARTIFACT_DIR
    if (artifacts) { await mkdir(artifacts, { recursive: true }); await page.screenshot({ path: path.join(artifacts, 'computer-management-mobile.png'), fullPage: true }) }
    const before = await serverRequest(origin, `/sessions/${sessionId}/history?limit=1000`, scope)
    await card.getByRole('button', { name: '移除登记', exact: true }).click()
    const remove = page.getByRole('dialog', { name: '移除这台电脑登记？', exact: true })
    assert.match(await remove.textContent(), /会话历史.*全部保留/)
    await remove.getByRole('button', { name: '移除登记', exact: true }).click()
    await card.waitFor({ state: 'detached' })
    assert.equal((await serverRequest(origin, `/tenants/${scope.tenantId}/my-computers`, scope)).executors.length, 0)
    assert.equal((await serverRequest(origin, '/state', scope)).workspaces.length, 1)
    const after = await serverRequest(origin, `/sessions/${sessionId}/history?limit=1000`, scope)
    assert.deepEqual(after.events, before.events)
    assert.equal(await readFile(path.join(folder, 'retained.txt'), 'utf8'), 'management-history-retained')
    assert.deepEqual(errors, [])
  } catch (error) {
    if (page && process.env.TERNILO_E2E_ARTIFACT_DIR) await page.screenshot({ path: path.join(process.env.TERNILO_E2E_ARTIFACT_DIR, 'computer-management-failure.png'), fullPage: true }).catch(() => {})
    throw new Error(`${error.stack}\n${server?.diagnostics() ?? ''}\n${node?.diagnostics() ?? ''}`)
  } finally {
    await browser?.close()
    await stopProcess(node)
    await stopProcess(server)
    await rm(directory, { recursive: true, force: true })
  }
})
