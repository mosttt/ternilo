import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { mkdir, mkdtemp, readFile, rm } from 'node:fs/promises'
import path from 'node:path'
import { tmpdir } from 'node:os'
import test from 'node:test'
import { chromium } from 'playwright'
import { freePort, repository, serverRequest, startProcess, stopProcess, waitForHttp } from './platform-e2e-fixture.mjs'

async function until(read, ready, label) {
  const deadline = Date.now() + 40_000
  while (Date.now() < deadline) {
    const value = await read()
    if (ready(value)) return value
    await new Promise(resolve => setTimeout(resolve, 100))
  }
  throw new Error(`Timed out: ${label}`)
}

test('a local service restart renews the boot token without remote login, page reload or draft loss', { timeout: 120_000 }, async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-local-reconnect-'))
  const artifacts = process.env.TERNILO_E2E_ARTIFACT_DIR
  const processes = [], errors = [], bootstrapRequests = [], unauthorized = []
  let browser, page
  let restarting = false
  try {
    const origin = `http://127.0.0.1:${await freePort()}`
    const binary = process.env.TERNILO_E2E_LOCAL_BINARY ?? path.join(repository, 'target/debug/ternilo')
    const args = ['serve', '--listen', new URL(origin).host, '--data-dir', path.join(directory, 'data')]
    const environment = { XDG_STATE_HOME: path.join(directory, 'state') }
    const first = startProcess(binary, args, environment)
    processes.push(first)
    await waitForHttp(origin, first)
    const bootToken = async () => JSON.parse((await (await fetch(origin, { cache: 'no-store' })).text()).match(/window\.__TERNILO_BOOT__\s*=\s*(\{[^<]+\});/)[1]).apiToken
    const firstToken = await bootToken()
    const local = (resource, options = {}) => serverRequest(origin, resource, { token: firstToken, ...options })
    const folder = path.join(directory, 'workspace')
    await mkdir(folder)
    const workspace = await local('/workspaces', { body: { path: folder } })
    const session = await local('/sessions', { body: { workspace_id: workspace.workspace_id } })
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE })
    page = await browser.newPage({ locale: 'zh-CN', viewport: { width: 1400, height: 900 } })
    page.on('pageerror', error => errors.push(error.message))
    page.on('console', message => {
      if (message.type() !== 'error') return
      const text = message.text()
      if (restarting && (/Failed to load resource:.*(?:401|ERR_CONNECTION_(?:REFUSED|RESET|CLOSED))/.test(text)
        || text.startsWith(`WebSocket connection to '${origin.replace('http:', 'ws:')}/api/v1/live' failed`))) return
      errors.push(text)
    })
    page.on('request', request => { if (new URL(request.url()).pathname === '/' && request.resourceType() === 'fetch') bootstrapRequests.push(request.url()) })
    page.on('response', response => { if (response.status() === 401) unauthorized.push(response.url()); else if (response.status() >= 400) errors.push(`${response.status()} ${new URL(response.url()).pathname}`) })
    await page.goto(origin)
    await page.evaluate(async () => { await navigator.serviceWorker.ready; window.__reconnectMarker = 'preserved' })
    for (const asset of ['app.js', 'app.css']) {
      assert.equal(createHash('sha256').update(Buffer.from(await (await fetch(`${origin}/assets/${asset}`)).arrayBuffer())).digest('hex'), createHash('sha256').update(await readFile(path.join(repository, 'web/dist/assets', asset))).digest('hex'))
    }
    await page.locator(`[data-sidebar-session-row][data-session-id="${session.identity.session_id}"] [data-sidebar-session-button]`).click()
    const input = page.locator('[data-composer-input]')
    await input.fill('重连后应保留这份未发送草稿')
    await until(() => page.locator('[data-sidebar-connection]').textContent(), text => text.includes('已连接'), 'initial live connection')
    restarting = true
    await stopProcess(first)
    await until(() => page.locator('[data-sidebar-connection]').textContent(), text => !text.includes('已连接'), 'disconnected state')
    const restarted = startProcess(binary, args, environment)
    processes.push(restarted)
    await waitForHttp(origin, restarted)
    const newToken = await bootToken()
    assert.notEqual(newToken, firstToken)
    await until(() => page.locator('[data-sidebar-connection]').textContent(), text => text.includes('已连接'), 'automatic authenticated reconnection')
    restarting = false
    assert.ok(unauthorized.length >= 1)
    assert.ok(bootstrapRequests.length >= 1)
    assert.equal(await page.getByRole('dialog').filter({ hasText: '连接 Ternilo 节点' }).count(), 0)
    assert.equal(await page.evaluate(() => window.__reconnectMarker), 'preserved')
    const storage = await page.evaluate(() => JSON.stringify([Object.entries(localStorage), Object.entries(sessionStorage)]))
    assert.equal(storage.includes(newToken) || storage.includes(firstToken), false)
    assert.equal(await input.inputValue(), '重连后应保留这份未发送草稿')
    assert.equal(await page.locator(`[data-sidebar-session-row][data-session-id="${session.identity.session_id}"]`).count(), 1)
    if (artifacts) { await mkdir(artifacts, { recursive: true }); await page.screenshot({ path: path.join(artifacts, 'local-reconnected.png'), animations: 'disabled' }) }
    assert.deepEqual(errors, [])
  } catch (error) {
    if (page && artifacts) { await mkdir(artifacts, { recursive: true }); await page.screenshot({ path: path.join(artifacts, 'local-reconnect-failure.png') }).catch(() => {}) }
    throw new Error(`${error.stack}\n${processes.map(process => process.diagnostics()).join('\n')}`)
  } finally {
    await browser?.close()
    for (const process of processes.reverse()) await stopProcess(process)
    await rm(directory, { recursive: true, force: true })
  }
})
