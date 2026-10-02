import assert from 'node:assert/strict'
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { chromium } from 'playwright'
import { freePort, initializeServer, repository, serverRequest, startProcess, stopProcess, waitForHttp } from './platform-e2e-fixture.mjs'
import { localApi, until } from './model-device-fixture.mjs'
import { selectChoice } from './browser-select-fixture.mjs'

async function settle(page) {
  await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))))
}

async function sidebar(page) {
  if (page.viewportSize().width <= 760) await page.getByRole('button', { name: /^(打开|展开)侧边栏$/ }).click()
}

for (const platform of [false, true]) test(`settings, retained navigation and mobile input (${platform ? 'Server' : 'Local'})`, { timeout: 180_000 }, async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-settings-navigation-'))
  const artifacts = path.join(process.env.TERNILO_E2E_ARTIFACT_DIR ?? directory, platform ? 'server' : 'local')
  await mkdir(artifacts, { recursive: true })
  const processes = [], errors = [], requests = [], subscriptions = []
  let browser, page
  try {
    const environment = { XDG_STATE_HOME: path.join(directory, 'state') }
    const localOrigin = `http://127.0.0.1:${await freePort()}`
    const args = ['serve', '--listen', new URL(localOrigin).host, '--data-dir', path.join(directory, 'node')]
    let server, request
    if (platform) {
      server = await initializeServer({ directory: path.join(directory, 'server'), origin: `http://127.0.0.1:${await freePort()}`, mode: 'single_user', environment })
      processes.push(server)
      const tenantId = server.owner.session.personal_tenant_id
      request = (resource, options = {}) => serverRequest(server.origin, resource, { token: server.owner.session.access_token, tenantId, ...options })
      const enrolled = await request(`/tenants/${tenantId}/my-computer-enrollments`, { body: { name: 'settings-computer', project_id: null, ttl_seconds: 600 } })
      const enrolledComputerId = enrolled.enrollment.executor_id
      environment.TERNILO_LOCAL_TOKEN = (await request('/enrollments/consume', { body: { token: enrolled.enrollment.token } })).credential.token
      args.push('--node-id', enrolledComputerId, '--gateway-url', `${server.origin.replace('http:', 'ws:')}/api/v1/executors/connect`, '--allow-insecure-gateway')
    }
    const node = startProcess(process.env.TERNILO_E2E_NODE_BINARY ?? path.join(repository, 'target/debug/ternilo'), args, environment)
    processes.push(node)
    await waitForHttp(localOrigin, node)
    const local = await localApi(localOrigin)
    request ??= local
    const folder = path.join(directory, 'a-long-workspace-path-for-mobile-horizontal-scrolling-and-settings')
    await mkdir(folder)
    const workspace = await local('/workspaces', { body: { path: folder } })
    const session = await local('/sessions', { body: { workspace_id: workspace.workspace_id } })
    const nodeId = session.identity.session_id
    const write = async name => {
      await local(`/sessions/${nodeId}/queue`, { body: { content: { kind: 'prompt', input: `/write ${name} navigation-proof` } } })
      await until(() => local(`/sessions/${nodeId}/queue`), queue => !queue.active_run_id && !queue.items.some(item => item.placement === 'queued'), 'write finished')
    }
    await write('first.txt')
    const state = await until(() => request('/state'), value => value.sessions.length === 1, 'session visible')
    const sessionId = state.sessions[0].identity.session_id
    await until(() => request('/files'), value => value.items.length === 1, 'file visible')
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
    page = await browser.newPage({ viewport: { width: 1366, height: 900 }, hasTouch: true, serviceWorkers: 'block' })
    page.on('pageerror', error => errors.push(error.message))
    page.on('console', message => { if (message.type() === 'error') errors.push(message.text()) })
    page.on('request', request => { if (request.url().includes('/api/v1/')) requests.push(new URL(request.url()).pathname) })
    page.on('response', response => { if (response.status() >= 400) errors.push(`${response.status()} ${new URL(response.url()).pathname}`) })
    page.on('websocket', socket => socket.on('framesent', frame => { try { const value = JSON.parse(String(frame.payload)); if (value.type === 'subscribe') subscriptions.push(value) } catch {} }))
    await page.goto(server?.origin ?? localOrigin)
    if (server) {
      await page.getByLabel('用户名', { exact: true }).fill(server.owner.username)
      await page.getByLabel('密码', { exact: true }).fill(server.owner.password)
      await page.getByRole('button', { name: '登录', exact: true }).click()
    }
    await page.locator(`[data-sidebar-session-row][data-session-id="${sessionId}"]`).click()
    await page.locator('[data-session-workspace-context]').waitFor()
    await page.getByRole('textbox', { name: '输入任务', exact: true }).waitFor()
    await page.getByRole('button', { name: '文件', exact: true }).click()
    await page.locator('[data-file-id]').first().waitFor()
    const fileReads = requests.filter(route => route === '/api/v1/files').length
    const historySubscriptions = subscriptions.filter(frame => frame.after_seq === undefined).length
    for (let iteration = 0; iteration < 3; iteration += 1) {
      await page.getByRole('link', { name: '返回工作台', exact: true }).click()
      await page.locator('[data-session-workspace-context]').waitFor()
      await page.getByRole('button', { name: '文件', exact: true }).click()
      await page.locator('[data-file-id]').first().waitFor()
      await settle(page)
    }
    assert.equal(requests.filter(route => route === '/api/v1/files').length, fileReads, 'returning reuses file metadata instead of rereading the directory')
    assert.equal(subscriptions.filter(frame => frame.after_seq === undefined).length, historySubscriptions, 'navigation never requests full conversation history again')
    await write('second.txt')
    await until(() => page.locator('[data-file-id]').count(), count => count === 2, 'live file changes invalidate cached metadata')
    await page.getByRole('button', { name: '刷新', exact: true }).click()
    await page.locator('[data-file-id]').first().waitFor()
    await page.getByRole('link', { name: '返回工作台', exact: true }).click()
    await page.setViewportSize({ width: 390, height: 844 })
    await settle(page)
    const position = page.locator('[data-session-workspace-context]')
    await position.waitFor()
    assert.equal(await position.evaluate(element => getComputedStyle(element).scrollbarWidth), 'none')
    assert.ok(await position.evaluate(element => element.scrollWidth > element.clientWidth))
    const touch = await page.context().newCDPSession(page)
    const bounds = await position.boundingBox()
    const start = bounds.x + bounds.width - 8, height = bounds.y + bounds.height / 2
    await touch.send('Input.dispatchTouchEvent', { type: 'touchStart', touchPoints: [{ x: start, y: height }] })
    for (let step = 1; step <= 5; step += 1) {
      await touch.send('Input.dispatchTouchEvent', { type: 'touchMove', touchPoints: [{ x: start - step * (bounds.width - 16) / 5, y: height }] })
      await settle(page)
    }
    await touch.send('Input.dispatchTouchEvent', { type: 'touchEnd', touchPoints: [] })
    await until(() => position.evaluate(element => element.scrollLeft), value => value > 0, 'native touch scroll without scrollbar')
    await touch.detach()
    const input = page.getByRole('textbox', { name: '输入任务', exact: true })
    await input.fill('/glob *')
    await page.getByRole('button', { name: '发送', exact: true }).tap()
    await until(() => request(`/sessions/${sessionId}/queue`), queue => !queue.active_run_id && !queue.items.some(item => item.placement === 'queued'), 'mobile input finished')
    await settle(page)
    assert.equal(await input.evaluate(element => document.activeElement === element), false, 'submission does not restore mobile input focus')
    await page.getByRole('button', { name: '会话指令（/）', exact: true }).tap()
    await page.getByRole('listbox').waitFor()
    assert.equal(await input.evaluate(element => document.activeElement === element), false, 'opening commands does not summon the keyboard')
    await page.screenshot({ path: path.join(artifacts, 'mobile-commands-without-keyboard.png') })
    await page.getByRole('button', { name: '会话指令（/）', exact: true }).tap()
    await sidebar(page)
    await page.getByRole('button', { name: platform ? '用户设置' : '设置', exact: true }).click()
    for (const width of [390, 320, 1366]) {
      await page.setViewportSize({ width, height: width > 760 ? 900 : 844 })
      const controls = page.getByRole('combobox')
      const count = await controls.count()
      assert.ok(count >= 6)
      for (let index = 0; index < count; index += 1) {
        const control = controls.nth(index)
        assert.equal(await control.evaluate(element => element.tagName), 'BUTTON')
        if (await control.isDisabled()) continue
        await control.click()
        const menu = page.locator('[data-choice-menu]')
        await menu.waitFor()
        const box = await menu.boundingBox()
        assert.ok(box.x >= 0 && box.x + box.width <= width + 1, `dropdown exceeds ${width}px viewport`)
        await page.keyboard.press('Escape')
      }
      await selectChoice(page.getByRole('combobox', { name: '界面主题', exact: true }), 'light')
      await page.getByRole('combobox', { name: '语言', exact: true }).click()
      await page.screenshot({ path: path.join(artifacts, `settings-light-${width}.png`) })
      await page.keyboard.press('Escape')
      await selectChoice(page.getByRole('combobox', { name: '界面主题', exact: true }), 'dark')
      await page.getByRole('combobox', { name: '语言', exact: true }).click()
      await page.screenshot({ path: path.join(artifacts, `settings-dark-${width}.png`) })
      await page.keyboard.press('Escape')
      assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth))
    }
    await selectChoice(page.getByRole('combobox', { name: '语言', exact: true }), 'en')
    await selectChoice(page.getByRole('combobox', { name: 'Language', exact: true }), 'zh')
    assert.deepEqual(errors, [])
    await writeFile(path.join(artifacts, 'network.json'), JSON.stringify({ requests, subscriptions }, null, 2))
  } catch (error) {
    await page?.screenshot({ path: path.join(artifacts, 'failure.png'), fullPage: true }).catch(() => {})
    error.message += `\n${processes.map(process => process.diagnostics()).join('\n')}`
    throw error
  } finally {
    await browser?.close()
    for (const process of processes.reverse()) await stopProcess(process)
    await rm(directory, { recursive: true, force: true })
  }
})
