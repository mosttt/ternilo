import assert from 'node:assert/strict'
import { mkdir, mkdtemp, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { chromium } from 'playwright'
import { freePort, initializeServer, repository, serverRequest, startProcess, stopProcess, waitForHttp } from './platform-e2e-fixture.mjs'
import { localApi, until } from './model-device-fixture.mjs'

const binary = process.env.TERNILO_E2E_LOCAL_BINARY ?? path.join(repository, 'target/debug/ternilo')
const launch = () => chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE })
function diagnostics(page, errors) {
  page.on('pageerror', error => errors.push(error.message))
  page.on('console', message => { if (message.type() === 'error') errors.push(message.text()) })
  page.on('response', response => { if (response.status() >= 400) errors.push(`${response.status()} ${new URL(response.url()).pathname}`) })
}

test('cancelling batch deletion keeps the in-flight deletion and does not send remaining requests', { timeout: 90_000 }, async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-cancel-delete-'))
  const artifacts = process.env.TERNILO_E2E_ARTIFACT_DIR ?? directory
  await mkdir(artifacts, { recursive: true })
  const origin = `http://127.0.0.1:${await freePort()}`
  const app = startProcess(binary, ['serve', '--listen', new URL(origin).host, '--data-dir', path.join(directory, 'data')])
  let browser, release
  const errors = [], deletes = []
  try {
    await waitForHttp(origin, app)
    const request = await localApi(origin)
    const folder = path.join(directory, 'workspace'); await mkdir(folder)
    const workspace = await request('/workspaces', { body: { path: folder } })
    for (const sessionId of ['delete-first', 'delete-second', 'delete-third']) {
      await request('/sessions', { body: { workspace_id: workspace.workspace_id, session_id: sessionId } })
      await request(`/sessions/${sessionId}/queue`, { body: { content: { kind: 'prompt', input: '/glob *' } } })
      await until(() => request(`/sessions/${sessionId}/queue`), queue => !queue.active_run_id && !queue.items.some(item => item.placement === 'queued'), 'session is established')
    }
    await request(`/workspaces/${workspace.workspace_id}`, { method: 'DELETE' })
    const order = (await request('/state')).sessions.map(session => session.identity.session_id)
    browser = await launch()
    const page = await browser.newPage({ viewport: { width: 1280, height: 900 }, serviceWorkers: 'block' })
    diagnostics(page, errors)
    const held = new Promise(resolve => { release = resolve })
    let started
    const waiting = new Promise(resolve => { started = resolve })
    await page.route('**/api/v1/sessions/*', async route => {
      if (route.request().method() !== 'DELETE') return route.continue()
      const sessionId = new URL(route.request().url()).pathname.split('/').at(-1)
      deletes.push(sessionId)
      if (sessionId !== order[1]) return route.continue()
      const response = await route.fetch()
      started()
      await held
      await route.fulfill({ response })
    })
    await page.goto(origin)
    await page.getByRole('button', { name: '未分组', exact: true }).hover()
    await page.getByRole('button', { name: '未分组的操作', exact: true }).click()
    await page.getByRole('menuitem', { name: '删除全部会话（3）', exact: true }).click()
    const dialog = page.getByRole('dialog', { name: '删除未分组会话？', exact: true })
    await dialog.getByRole('button', { name: '全部删除', exact: true }).click()
    await Promise.race([waiting, new Promise((_, reject) => setTimeout(() => reject(new Error('second deletion did not start')), 15000))])
    await dialog.getByRole('button', { name: '取消剩余删除', exact: true }).click()
    assert.equal(await dialog.getByRole('button', { name: '等待当前删除完成…', exact: true }).isDisabled(), true)
    await page.screenshot({ path: path.join(artifacts, 'batch-deletion-cancelling.png') })
    release()
    await dialog.waitFor({ state: 'hidden' })
    assert.deepEqual(deletes, order.slice(0, 2))
    assert.deepEqual((await request('/state')).sessions.map(session => session.identity.session_id), [order[2]])
    await page.getByText('已停止删除：已删除 2 个，保留 1 个会话', { exact: true }).waitFor()
    await page.screenshot({ path: path.join(artifacts, 'batch-deletion-cancelled.png') })
    assert.deepEqual(errors, [])
  } finally {
    release?.(); await browser?.close(); await stopProcess(app); await rm(directory, { recursive: true, force: true })
  }
})

test('projects have a discoverable management page and a themed keyboard-accessible picker', { timeout: 120_000 }, async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-project-ui-'))
  const artifacts = process.env.TERNILO_E2E_ARTIFACT_DIR ?? directory
  await mkdir(artifacts, { recursive: true })
  let server, browser, page
  const errors = []
  try {
    server = await initializeServer({ directory, origin: `http://127.0.0.1:${await freePort()}`, mode: 'single_user' })
    const request = (resource, options = {}) => serverRequest(server.origin, resource, { token: server.owner.session.access_token, tenantId: server.owner.session.personal_tenant_id, ...options })
    browser = await launch()
    page = await browser.newPage({ viewport: { width: 1280, height: 900 }, hasTouch: true, serviceWorkers: 'block' })
    diagnostics(page, errors)
    await page.goto(server.origin)
    await page.getByLabel('用户名', { exact: true }).fill(server.owner.username)
    await page.getByLabel('密码', { exact: true }).fill(server.owner.password)
    await page.getByRole('button', { name: '登录', exact: true }).click()
    await page.getByRole('button', { name: '添加工作区', exact: true }).click()
    let dialog = page.getByRole('dialog', { name: '打开工作区', exact: true })
    const project = dialog.getByRole('combobox', { name: '项目', exact: true })
    await until(() => project.getAttribute('data-choice-value'), value => Boolean(value), 'initial projects loaded')
    await project.press('ArrowDown')
    await page.getByRole('listbox').waitFor()
    await page.getByRole('option', { name: '新建项目', exact: true }).press('End')
    await page.getByRole('option', { name: '新建项目', exact: true }).press('Enter')
    await dialog.getByLabel('项目名称', { exact: true }).fill('Browser project')
    await dialog.getByRole('button', { name: '仅创建项目', exact: true }).click()
    await until(() => request('/projects'), result => result.projects.some(project => project.name === 'Browser project'), 'project created from picker')
    await dialog.getByRole('button', { name: '管理项目', exact: true }).click()
    await page.locator('[data-project-management]').waitFor()
    assert.equal(await page.getByRole('dialog').count(), 0)
    await page.getByRole('heading', { name: 'Browser project', exact: true }).waitFor()
    await page.getByLabel('项目名称', { exact: true }).fill('Second project')
    await page.getByRole('button', { name: '新建项目', exact: true }).click()
    await page.getByRole('heading', { name: 'Second project', exact: true }).waitFor()
    await page.screenshot({ path: path.join(artifacts, 'project-management-desktop.png') })
    await page.getByRole('link', { name: '返回工作台', exact: true }).click()
    await page.getByRole('button', { name: '添加工作区', exact: true }).click()
    dialog = page.getByRole('dialog', { name: '打开工作区', exact: true })
    for (const width of [1280, 390]) {
      await page.setViewportSize({ width, height: 900 })
      await dialog.getByRole('combobox', { name: '项目', exact: true }).click()
      const list = page.getByRole('listbox')
      await list.getByRole('option', { name: 'Second project', exact: true }).waitFor()
      const bounds = await list.boundingBox()
      assert.ok(bounds.x >= 0 && bounds.x + bounds.width <= width)
      await page.screenshot({ path: path.join(artifacts, `project-picker-${width}.png`) })
      await list.getByRole('option', { name: 'Second project', exact: true }).click()
      assert.match(await dialog.getByRole('combobox', { name: '项目', exact: true }).innerText(), /Second project/)
    }
    assert.deepEqual(errors, [])
  } catch (error) {
    await page?.screenshot({ path: path.join(artifacts, 'project-navigation-failure.png') }).catch(() => {})
    throw error
  } finally {
    await browser?.close(); await stopProcess(server); await rm(directory, { recursive: true, force: true })
  }
})
