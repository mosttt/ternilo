import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { chromium } from 'playwright'
import { localApi, until } from './model-device-fixture.mjs'
import { freePort, initializeServer, repository, serverRequest, startProcess, stopProcess, waitForHttp } from './platform-e2e-fixture.mjs'

async function thumb(surface) {
  return surface.evaluate(element => ({
    color: getComputedStyle(element, '::-webkit-scrollbar-thumb').backgroundColor,
    width: getComputedStyle(element, '::-webkit-scrollbar').width,
    top: element.scrollTop,
    overflow: element.scrollHeight > element.clientHeight,
    active: element.hasAttribute('data-scrollbar-active'),
  }))
}

async function checkScrolling(page, surface, touch) {
  await surface.waitFor()
  await until(() => thumb(surface), state => state.overflow && !state.active, 'idle overflowing surface')
  assert.equal((await thumb(surface)).color, 'rgba(0, 0, 0, 0)')
  assert.equal((await thumb(surface)).width, '8px')
  await surface.evaluate(element => { element.scrollTop = 20 })
  await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))))
  assert.equal((await thumb(surface)).active, false, 'restoring position does not reveal the scrollbar')
  const bounds = await surface.boundingBox()
  const horizontal = bounds.x + bounds.width / 2
  const vertical = bounds.y + Math.min(bounds.height - 30, 220)
  if (touch) {
    const client = await page.context().newCDPSession(page)
    await client.send('Input.dispatchTouchEvent', { type: 'touchStart', touchPoints: [{ x: horizontal, y: vertical }] })
    await client.send('Input.dispatchTouchEvent', { type: 'touchMove', touchPoints: [{ x: horizontal, y: vertical - 90 }] })
    await client.send('Input.dispatchTouchEvent', { type: 'touchEnd', touchPoints: [] })
    await client.detach()
  } else {
    await page.mouse.move(horizontal, vertical)
    await page.mouse.wheel(0, 130)
  }
  await until(() => thumb(surface), state => state.active && state.top > 20, 'user scrolling')
  assert.notEqual((await thumb(surface)).color, 'rgba(0, 0, 0, 0)')
  await until(() => thumb(surface), state => !state.active, 'scrollbar hides after inactivity')
  assert.equal((await thumb(surface)).color, 'rgba(0, 0, 0, 0)')
}

test('Local and Server share initially hidden, themed scrollbars on desktop and touch screens', { timeout: 180000 }, async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-scrollbars-'))
  const artifacts = process.env.TERNILO_E2E_ARTIFACT_DIR || path.join(directory, 'evidence')
  await mkdir(artifacts, { recursive: true })
  const processes = []
  const errors = []
  const layouts = []
  let browser
  try {
    const server = await initializeServer({ directory: path.join(directory, 'server'), origin: `http://127.0.0.1:${await freePort()}` })
    processes.push(server)
    const tenant = server.owner.session.personal_tenant_id
    const request = (resource, options = {}) => serverRequest(server.origin, resource, { token: server.owner.session.access_token, tenantId: tenant, ...options })
    const enrollment = await request(`/tenants/${tenant}/my-computer-enrollments`, { body: { name: 'scroll-node', project_id: null, ttl_seconds: 600 } })
    const enrolledComputerId = enrollment.enrollment.executor_id
    const credential = await request('/enrollments/consume', { body: { token: enrollment.enrollment.token } })
    const origin = `http://127.0.0.1:${await freePort()}`
    const environment = Object.fromEntries(Object.keys(process.env).filter(key => key.startsWith('TERNILO_')).map(key => [key, undefined]))
    const node = startProcess(process.env.TERNILO_E2E_NODE_BINARY ?? path.join(repository, 'target/debug/ternilo'), ['serve', '--listen', new URL(origin).host, '--data-dir', path.join(directory, 'node'), '--node-id', enrolledComputerId, '--gateway-url', `${server.origin.replace('http:', 'ws:')}/api/v1/executors/connect`, '--allow-insecure-gateway'], { ...environment, TERNILO_LOCAL_TOKEN: credential.credential.token, XDG_STATE_HOME: path.join(directory, 'state') })
    processes.push(node)
    await waitForHttp(origin, node)
    const local = await localApi(origin)
    const folder = path.join(directory, 'workspace')
    await mkdir(folder)
    const workspace = await local('/workspaces', { body: { path: folder } })
    for (let index = 0; index < 24; index++) await local('/sessions', { body: { workspace_id: workspace.workspace_id } })
    await until(() => request('/state'), state => state.sessions.length === 24, 'Node sessions synchronized')
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE || undefined })
    for (const [surfaceName, address] of [['local', origin], ['server', server.origin]]) {
      for (const asset of ['app.js', 'app.css']) {
        const response = await fetch(`${address}/assets/${asset}`)
        assert.equal(response.status, 200)
        assert.equal(createHash('sha256').update(Buffer.from(await response.arrayBuffer())).digest('hex'), createHash('sha256').update(await readFile(path.join(repository, 'web/dist/assets', asset))).digest('hex'))
      }
      const page = await browser.newPage({ viewport: { width: 1366, height: 700 }, hasTouch: true, serviceWorkers: 'block' })
      page.on('pageerror', error => errors.push(error.message))
      page.on('console', message => { if (message.type() === 'error') errors.push(message.text()) })
      page.on('response', response => { if (response.status() >= 400) errors.push(`${response.status()} ${response.url()}`) })
      page.on('requestfailed', failed => { if (failed.failure()?.errorText !== 'net::ERR_ABORTED') errors.push(`${failed.url()}: ${failed.failure()?.errorText}`) })
      await page.goto(`${address}/files`)
      if (surfaceName === 'server') {
        await page.getByLabel('用户名', { exact: true }).fill(server.owner.username)
        await page.getByLabel('密码', { exact: true }).fill(server.owner.password)
        await page.getByRole('button', { name: '登录', exact: true }).click()
        await page.getByLabel('用户名', { exact: true }).waitFor({ state: 'hidden' })
        await page.goto(`${address}/files`)
      }
      for (const theme of ['dark', 'light']) {
        await page.evaluate(value => localStorage.setItem('ternilo.theme', value), theme)
        await page.reload()
        await until(() => page.locator('[data-file-session-filter]').count(), count => count === 24, 'file filters ready')
        await checkScrolling(page, page.locator('[data-files-navigation]'), false)
        await page.screenshot({ path: path.join(artifacts, `${surfaceName}-files-${theme}.png`) })
        layouts.push({ surfaceName, theme, width: 1366, page: 'files' })
      }
      assert.equal(await page.evaluate(() => !document.body.dispatchEvent(new MouseEvent('contextmenu', { bubbles: true, cancelable: true }))), false, 'ordinary browser keeps its default menu')
      await page.goto(address)
      const workspaceButton = page.locator('[data-sidebar-workspace-button]').first()
      await workspaceButton.waitFor()
      if (await workspaceButton.getAttribute('aria-expanded') === 'false') await workspaceButton.click()
      await page.locator('[data-sidebar-session-button]').first().click()
      for (const width of [1366, 390, 320]) {
        await page.setViewportSize({ width, height: 700 })
        await page.getByText('让想法，动起来', { exact: true }).waitFor()
        assert.equal(await page.locator('[data-new-session-hero]').getByText('预览版', { exact: true }).count(), 0)
        assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth))
        await page.screenshot({ path: path.join(artifacts, `${surfaceName}-welcome-${width}.png`) })
      }
      for (const width of [390, 320]) {
        await page.setViewportSize({ width: 1366, height: 600 })
        await page.goto(surfaceName === 'server' ? `${address}/settings/general` : address)
        if (surfaceName === 'local') await page.getByRole('button', { name: '设置', exact: true }).click()
        await page.locator('[data-settings-content]').waitFor()
        await page.setViewportSize({ width, height: 600 })
        await checkScrolling(page, page.locator('[data-settings-content]'), true)
        assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth))
        await page.screenshot({ path: path.join(artifacts, `${surfaceName}-settings-${width}.png`) })
        layouts.push({ surfaceName, width, page: 'settings' })
      }
      await page.close()
    }
    assert.deepEqual(errors, [])
  } finally {
    await writeFile(path.join(artifacts, 'scrollbar-results.json'), JSON.stringify({ errors, layouts }, null, 2))
    await browser?.close()
    for (const process of processes.reverse()) await stopProcess(process)
    await rm(directory, { recursive: true, force: true })
  }
})
