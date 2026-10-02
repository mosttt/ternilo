import assert from 'node:assert/strict'
import { mkdir, mkdtemp, rm } from 'node:fs/promises'
import path from 'node:path'
import { tmpdir } from 'node:os'
import test from 'node:test'
import { chromium } from 'playwright'
import { freePort, initializeServer, repository, selectSpace, serverRequest, startProcess, stopProcess, waitForHttp } from './platform-e2e-fixture.mjs'
import { localApi, until } from './model-device-fixture.mjs'

async function settle(page) {
  await page.evaluate(async () => {
    await new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve)))
    await Promise.all(document.getAnimations().filter(animation => animation.effect?.getTiming().iterations !== Infinity).map(animation => animation.finished.catch(() => {})))
  })
}

test('workspace identity, controls, navigation and two-way deletion remain consistent', { timeout: 180_000 }, async testContext => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-header-theme-'))
  const artifacts = process.env.TERNILO_E2E_ARTIFACT_DIR ?? directory
  await mkdir(artifacts, { recursive: true })
  const errors = []
  const deletedSessions = new Map(), offlineSessions = new Set(), expectedReadRejections = []
  let server, node, browser, page
  try {
    server = await initializeServer({ directory: path.join(directory, 'server'), origin: `http://127.0.0.1:${await freePort()}`, mode: 'multi_user' })
    const tenantId = (await serverRequest(server.origin, '/tenants', { token: server.owner.session.access_token, body: { slug: 'design-tests', display_name: '设计团队' } })).tenant.tenant_id
    const request = (resource, options = {}) => serverRequest(server.origin, resource, { token: server.owner.session.access_token, tenantId, ...options })
    const { project } = await request('/projects', { body: { name: '设计项目' } })
    const enrolled = await request(`/tenants/${tenantId}/my-computer-enrollments`, { body: { name: 'design-desktop', project_id: project.project_id, ttl_seconds: 600 } })
    const enrolledComputerId = enrolled.enrollment.executor_id
    const credential = await request('/enrollments/consume', { body: { token: enrolled.enrollment.token } })
    const origin = `http://127.0.0.1:${await freePort()}`
    const binary = process.env.TERNILO_E2E_NODE_BINARY ?? path.join(repository, 'target/debug/ternilo')
    const localArgs = ['serve', '--listen', new URL(origin).host, '--data-dir', path.join(directory, 'node')]
    const nodeArgs = [...localArgs, '--node-id', enrolledComputerId, '--gateway-url', `${server.origin.replace('http:', 'ws:')}/api/v1/executors/connect`, '--allow-insecure-gateway']
    const environment = { TERNILO_LOCAL_TOKEN: credential.credential.token, XDG_STATE_HOME: path.join(directory, 'state') }
    node = startProcess(binary, nodeArgs, environment)
    await waitForHttp(origin, node)
    let local = await localApi(origin)
    const folder = path.join(directory, ...Array.from({ length: 5 }, (_, index) => `long-workspace-location-${index}-design-documents`))
    await mkdir(folder, { recursive: true })
    const workspace = await local('/workspaces', { body: { path: folder } })
    await local(`/workspaces/${workspace.workspace_id}`, { method: 'PATCH', body: { title: '设计工作区' } })
    const session = await local('/sessions', { body: { workspace_id: workspace.workspace_id } })
    await local(`/sessions/${session.identity.session_id}/queue`, { body: { content: { kind: 'prompt', input: '/glob *' } } })
    await until(() => local(`/sessions/${session.identity.session_id}/queue`), value => !value.active_run_id && !value.items.some(item => item.placement === 'queued'), 'local command completes')
    const profile = {
      id: 'design-provider', display_name: '设计模型', protocol: 'openai-chat-completions', base_url: 'http://127.0.0.1:9/v1', api_key_ref: null,
      defaults: { context_window: 32000, max_output_tokens: 2048, reasoning: { default_effort: 'medium', efforts: { low: 'low', medium: 'medium', high: 'high' } } },
      models: [{ id: 'design-model', display_name: 'Design Model', settings: { mode: 'inherit' } }], timeout_ms: 1000, max_attempts: 1, retry_base_delay_ms: 25,
    }
    await local('/providers', { body: profile })
    await request('/providers', { body: profile, tenantId: server.owner.session.personal_tenant_id })
    await request('/admin/models/providers', { body: { profile, enabled: true } })
    await request('/admin/models/publications', { body: { model_id: 'platform-design', display_name: '平台设计模型', provider_id: profile.id, upstream_model: 'design-model', enabled: true } })
    await request('/admin/models/grants', { body: { name: '设计预算', subject: { kind: 'user', id: server.owner.session.user.user_id }, model_ids: ['platform-design'], monthly_tokens: 100000, max_concurrent_requests: 2, allow_resource_sharing: false } })
    await local(`/sessions/${session.identity.session_id}`, { method: 'PATCH', body: { model: { provider: 'named_provider', provider_id: profile.id, model: 'design-model' } } })
    const state = await until(() => request('/state'), value => value.sessions.length === 1 && value.workspaces.some(item => item.status === 'online'), 'workspace synchronizes')
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE })
    page = await browser.newPage({ viewport: { width: 1440, height: 900 }, hasTouch: true, serviceWorkers: 'block' })
    const requestSession = address => {
      if (!address.startsWith(server.origin)) return false
      const url = new URL(address)
      return url.searchParams.get('session_id') ?? url.pathname.match(/\/sessions\/([^/]+)/)?.[1]
    }
    const expectedRead = (address, status) => {
      const target = requestSession(address)
      return status === 400 && deletedSessions.has(target) || status === 503 && offlineSessions.has(target)
    }
    page.on('pageerror', error => errors.push(error.message))
    page.on('console', message => {
      if (message.type() !== 'error') return
      const status = message.text().match(/^Failed to load resource: the server responded with a status of (400|503) /)?.[1]
      if (!status || !expectedRead(message.location().url, Number(status))) errors.push(message.text())
    })
    page.on('response', async response => {
      if (response.status() < 400) return
      const body = await response.text()
      let failure
      try { failure = JSON.parse(body).error } catch {}
      const nodeSession = deletedSessions.get(requestSession(response.url()))
      const expectedBody = response.status() === 400
        ? failure?.code === 'invalid_input' && (failure.message === 'session does not exist' || nodeSession && failure.message === `unknown session ${JSON.stringify(nodeSession)}`)
        : failure?.code === 'unavailable' && failure.message.includes('node is offline')
      if (response.request().method() === 'GET' && expectedRead(response.url(), response.status()) && expectedBody) expectedReadRejections.push({ status: response.status(), url: response.url(), body })
      else errors.push(`${response.status()} ${response.url()}: ${body}`)
    })
    await page.goto(server.origin)
    await page.getByLabel('用户名', { exact: true }).fill(server.owner.username)
    await page.getByLabel('密码', { exact: true }).fill(server.owner.password)
    await page.getByRole('button', { name: '登录', exact: true }).click()
    await page.getByRole('button', { name: '创建团队', exact: true }).waitFor()
    await selectSpace(page, tenantId)
    await page.locator(`[data-sidebar-session-row][data-session-id="${state.sessions[0].identity.session_id}"]`).click()
    const context = page.locator('[data-session-workspace-context]')
    await context.locator('[data-session-workspace-path]').filter({ hasText: folder }).waitFor()
    await context.locator('[data-session-project]').filter({ hasText: '设计项目' }).waitFor()
    assert.equal(await context.innerText(), `电脑 design-desktop · 设计工作区 · 设计项目 · ${folder}`)
    assert.equal(await context.locator('button, a').count(), 0, 'workspace information is not a navigation control')
    for (const width of [1440, 390, 320]) {
      await page.mouse.move(1438, 880)
      await page.setViewportSize({ width, height: 900 })
      await settle(page)
      await context.evaluate(element => { element.scrollLeft = 0 })
      const layout = await context.evaluate(element => ({
        width: element.clientWidth, content: element.scrollWidth,
        tops: [...element.querySelectorAll('[data-session-workspace], [data-session-project], [data-session-workspace-path]')].map(item => item.getBoundingClientRect().top),
        pageWidth: document.documentElement.scrollWidth, viewport: innerWidth,
      }))
      assert.ok(layout.content > layout.width && layout.width > 40, JSON.stringify(layout))
      assert.ok(Math.max(...layout.tops) - Math.min(...layout.tops) < 2, 'all identity fields stay on the same line')
      assert.ok(layout.pageWidth <= layout.viewport, 'only the identity strip overflows')
      await page.screenshot({ path: path.join(artifacts, `workspace-header-${width}.png`) })
      if (width === 1440) {
        await context.hover(); await page.mouse.wheel(0, 300)
      } else {
        const bounds = await context.boundingBox()
        const touch = await page.context().newCDPSession(page)
        const start = bounds.x + bounds.width - 8
        const height = bounds.y + 7
        await touch.send('Input.dispatchTouchEvent', { type: 'touchStart', touchPoints: [{ x: start, y: height }] })
        for (let step = 1; step <= 5; step++) {
          await touch.send('Input.dispatchTouchEvent', { type: 'touchMove', touchPoints: [{ x: start - step * (bounds.width - 16) / 5, y: height }] })
          await new Promise(resolve => setTimeout(resolve, 20))
        }
        await touch.send('Input.dispatchTouchEvent', { type: 'touchEnd', touchPoints: [] })
        await touch.detach()
      }
      await until(() => context.evaluate(element => element.scrollLeft), value => value > 10, `scroll input at ${width}px`)
      assert.equal(await page.getByRole('dialog').count(), 0, 'scrolling keeps the workspace information read-only')
      await context.evaluate(element => { element.scrollLeft = element.scrollWidth })
      await page.screenshot({ path: path.join(artifacts, `workspace-header-end-${width}.png`) })
    }
    await page.setViewportSize({ width: 1440, height: 900 })
    await settle(page)
    await context.evaluate(element => { element.scrollLeft = 0 })
    await context.focus(); await page.keyboard.press('ArrowRight')
    await until(() => context.evaluate(element => element.scrollLeft), value => value > 0, 'keyboard scroll')
    for (const theme of ['dark', 'light']) {
      await page.evaluate(value => localStorage.setItem('ternilo.theme', value), theme)
      await page.reload()
      await context.waitFor()
      await page.getByRole('button', { name: '添加工作区', exact: true }).click()
      const dialog = page.getByRole('dialog', { name: '打开工作区', exact: true })
      const select = dialog.getByRole('combobox', { name: '项目', exact: true })
      await until(() => select.getAttribute('data-choice-value'), value => Boolean(value), 'project loaded')
      const controlStyle = locator => locator.evaluate(element => ({ background: getComputedStyle(element).backgroundColor, border: getComputedStyle(element).borderTopWidth }))
      const inputStyle = await controlStyle(dialog.getByLabel('工作区名称', { exact: true }))
      for (const control of [select, dialog.getByRole('button', { name: '取消', exact: true }), dialog.getByRole('button', { name: '管理项目', exact: true })]) assert.deepEqual(await controlStyle(control), inputStyle)
      await page.screenshot({ path: path.join(artifacts, `workspace-dialog-${theme}.png`) })
      await select.click(); await page.getByRole('option', { name: '设计项目', exact: true }).click()
      await dialog.getByRole('button', { name: '取消', exact: true }).click()
      await dialog.waitFor({ state: 'hidden' })
      const row = page.locator('[data-sidebar-workspace-row]').filter({ hasText: '设计工作区' })
      await row.hover(); await row.getByRole('button', { name: '工作区“设计工作区”的操作', exact: true }).click()
      const menu = page.getByRole('menu')
      await menu.getByRole('menuitem', { name: '查看位置', exact: true }).waitFor()
      await settle(page)
      for (const icon of await menu.getByRole('menuitem').locator('svg').all()) {
        const bounds = await icon.boundingBox()
        assert.equal(bounds.width, 16); assert.equal(bounds.height, 16)
      }
      const remove = menu.getByRole('menuitem', { name: '移除工作区', exact: true })
      const destructiveColor = await remove.evaluate(element => getComputedStyle(element).color)
      await remove.hover()
      assert.equal(await remove.evaluate(element => getComputedStyle(element).color), destructiveColor, 'destructive intent stays visible on hover')
      await page.screenshot({ path: path.join(artifacts, `workspace-menu-${theme}.png`) })
      await page.keyboard.press('Escape')
      await menu.waitFor({ state: 'hidden' })
    }
    await page.locator('[data-input-bar] [data-model-picker]').click()
    assert.equal(await page.getByRole('button', { name: '配置账号自有模型', exact: true }).count(), 0)
    await page.getByRole('menuitem', { name: /^模型/ }).press('ArrowRight')
    for (const source of ['account', 'node', 'platform']) await page.locator(`[data-model-source="${source}"] [role="menuitem"]`).first().waitFor()
    assert.equal(await page.locator('[data-model-source="node"] [data-selected]').count(), 1)
    await page.screenshot({ path: path.join(artifacts, 'model-source-menu-light.png') })
    await page.keyboard.press('Escape')
    await page.evaluate(() => { history.pushState({}, '', '/models'); dispatchEvent(new PopStateEvent('popstate')) })
    await page.locator('[data-model-access-shell]').waitFor()
    await page.getByRole('menu').waitFor({ state: 'hidden' })
    await page.goto(`${server.origin}/files`)
    for (const width of [1440, 390]) {
      await page.setViewportSize({ width, height: 900 })
      await settle(page)
      await page.getByRole('combobox', { name: '切换空间', exact: true }).waitFor()
      assert.equal(await page.getByRole('button', { name: '创建团队', exact: true }).count(), 0)
      await page.screenshot({ path: path.join(artifacts, `files-space-switcher-${width}.png`) })
    }
    await page.setViewportSize({ width: 1440, height: 900 })
    await page.goto(server.origin)
    const originalId = state.sessions[0].identity.session_id
    const originalRow = page.locator(`[data-sidebar-session-row][data-session-id="${originalId}"]`)
    await originalRow.waitFor()
    const registration = await request('/admin/registration')
    await request('/admin/registration', { method: 'PATCH', body: { mode: 'open', require_approval: false, revision: registration.revision } })
    const member = (await serverRequest(server.origin, '/auth/register', { body: { username: 'delete-viewer', email: 'delete-viewer@example.test', password: 'delete-viewer-password' } })).session
    await request(`/tenants/${tenantId}/members/${member.user.user_id}`, { method: 'PUT', body: { role: 'member' } })
    const memberRequest = (resource, options = {}) => serverRequest(server.origin, resource, { token: member.access_token, tenantId, ...options })
    await assert.rejects(() => memberRequest(`/sessions/${originalId}`, { method: 'DELETE' }), /400|403/)
    await request(`/sessions/${originalId}/sharing/user/${member.user.user_id}`, { method: 'PUT', body: { view: true, submit: true, stop: true, configure: true } })
    await assert.rejects(() => memberRequest(`/sessions/${originalId}`, { method: 'DELETE' }), /403/)
    assert.ok((await local('/state')).sessions.some(item => item.identity.session_id === session.identity.session_id), 'sharing or a guessed ID never grants deletion')
    deletedSessions.set(originalId, session.identity.session_id)
    await local(`/sessions/${session.identity.session_id}`, { method: 'DELETE' })
    await until(() => request('/state'), value => !value.sessions.some(item => item.identity.session_id === originalId), 'local deletion removes the Server mapping')
    await originalRow.waitFor({ state: 'hidden' })
    assert.equal((await memberRequest('/state')).sessions.some(item => item.identity.session_id === originalId), false)
    await request(`/sessions/${originalId}`, { method: 'DELETE' })
    await local(`/sessions/${session.identity.session_id}`, { method: 'DELETE' })
    const createSession = async id => {
      await local('/sessions', { body: { workspace_id: workspace.workspace_id, session_id: id } })
      await local(`/sessions/${id}/queue`, { body: { content: { kind: 'prompt', input: '/glob *' } } })
      await until(() => local(`/sessions/${id}/queue`), value => !value.active_run_id && !value.items.some(item => item.placement === 'queued'), 'new local session ready')
      const snapshot = await until(() => request('/state'), value => value.sessions.some(item => !known.has(item.identity.session_id)), 'new session mapped')
      const created = snapshot.sessions.find(item => !known.has(item.identity.session_id)).identity.session_id
      known.add(created)
      return created
    }
    const known = new Set((await request('/state')).sessions.map(item => item.identity.session_id))
    const serverDeleted = await createSession('delete-from-server')
    deletedSessions.set(serverDeleted, 'delete-from-server')
    await request(`/sessions/${serverDeleted}`, { method: 'DELETE' })
    assert.equal((await local('/state')).sessions.some(item => item.identity.session_id === 'delete-from-server'), false)
    await request(`/sessions/${serverDeleted}`, { method: 'DELETE' })
    const offlineDeleted = await createSession('delete-while-offline')
    const retained = await createSession('keep-this-session')
    offlineSessions.add(offlineDeleted); offlineSessions.add(retained)
    await stopProcess(node)
    await until(() => request('/state'), value => value.workspaces.every(item => item.status === 'offline'), 'computer disconnected')
    assert.ok((await request('/state')).sessions.some(item => item.identity.session_id === offlineDeleted), 'offline is not evidence of deletion')
    node = startProcess(binary, localArgs, { XDG_STATE_HOME: environment.XDG_STATE_HOME })
    await waitForHttp(origin, node); local = await localApi(origin)
    deletedSessions.set(offlineDeleted, 'delete-while-offline')
    await local('/sessions/delete-while-offline', { method: 'DELETE' })
    await stopProcess(node)
    node = startProcess(binary, nodeArgs, environment)
    await waitForHttp(origin, node); local = await localApi(origin)
    await until(() => request('/state'), value => value.workspaces.some(item => item.status === 'online') && !value.sessions.some(item => item.identity.session_id === offlineDeleted), 'offline deletion catches up after reconnect')
    assert.ok((await request('/state')).sessions.some(item => item.identity.session_id === retained))
    assert.ok((await local('/state')).sessions.some(item => item.identity.session_id === 'keep-this-session'))
    assert.ok((await request(`/sessions/${retained}/events`)).length > 0, 'the retained session history still works after reconnect')
    await page.waitForLoadState('networkidle')
    assert.doesNotMatch(await page.locator('body').innerText(), /unknown session|session does not exist/)
    testContext.diagnostic(JSON.stringify({ expectedReadRejections }))
    assert.deepEqual(errors, [])
  } catch (error) {
    await page?.screenshot({ path: path.join(artifacts, 'workspace-header-failure.png') }).catch(() => {})
    throw error
  } finally {
    await browser?.close(); await stopProcess(node); await stopProcess(server); await rm(directory, { recursive: true, force: true })
  }
})
