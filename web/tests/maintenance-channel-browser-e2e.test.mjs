import assert from 'node:assert/strict'
import { mkdir, mkdtemp, readFile, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { DatabaseSync } from 'node:sqlite'
import { chromium } from 'playwright'
import { freePort, initializeServer, serverRequest, startProcess, stopProcess, waitForHttp, repository } from './platform-e2e-fixture.mjs'

// A controlled lifecycle peer deliberately never opens the execution WebSocket.
test('incompatible execution stays maintainable across Server restart and offers an update in three languages', { timeout: 90_000 }, async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-maintenance-browser-'))
  let server, browser, polling, running = true
  const errors = [], commands = []
  try {
    const origin = `http://127.0.0.1:${await freePort()}`
    server = await initializeServer({ directory: path.join(directory, 'server'), origin })
    const identity = server.owner.session
    const scope = { token: identity.access_token, tenantId: identity.personal_tenant_id }
    const { enrollment } = await serverRequest(origin, `/tenants/${scope.tenantId}/my-computer-enrollments`, { token: scope.token, body: { name: 'Maintenance computer', ttl_seconds: 600 } })
    const { credential } = await serverRequest(origin, '/enrollments/consume', { body: { token: enrollment.token } })
    const info = { protocol_version: 1, version: '0.1.0', target: 'x86_64-unknown-linux-gnu', enabled: true, data_epoch: 1, restart_method: 'supervisor', progress: null }
    const hello = { protocol_version: 1, executor_id: enrollment.executor_id, instance_nonce: 'maintenance-browser-fixture', execution_protocol_version: 4294967295, info }
    const exchange = body => serverRequest(origin, '/executors/maintenance/v1', { token: credential.token, body })
    await serverRequest(origin, '/executors/cleanup/sync', { token: credential.token, body: { storage_instance_id: 'maintenance-browser-storage' } })
    const greeting = await exchange({ type: 'hello', hello })
    assert.equal(greeting.protocol_version, 1)
    assert.notEqual(greeting.execution_protocol_version, hello.execution_protocol_version)
    let reply = null, paused = false
    polling = (async () => {
      while (running) {
        if (paused) { await new Promise(resolve => setTimeout(resolve, 50)); continue }
        try {
          const response = await exchange({ type: 'poll', instance_nonce: hello.instance_nonce, info, reply })
          reply = null
          if (response.command) {
            commands.push(response.command)
            assert.equal(response.command.command.operation, 'status')
            reply = { id: response.command.id, info, error: null }
          }
        } catch (error) {
          // Only connection loss is expected while this test deliberately restarts Server.
          if (!String(error).includes('fetch failed')) throw error
        }
        await new Promise(resolve => setTimeout(resolve, 100))
      }
    })().catch(error => { errors.push(error.stack); running = false })
    const endpoint = `/tenants/${scope.tenantId}/my-computers/${enrollment.executor_id}/updates`
    const status = await serverRequest(origin, `${endpoint}?refresh=true`, scope)
    assert.equal(status.connected, true)
    assert.equal(status.execution_connected, false)
    assert.equal(status.availability, 'available')
    assert.equal(status.execution_protocol_version, hello.execution_protocol_version)
    assert.equal(commands.length, 1)
    paused = true
    await new Promise(resolve => setTimeout(resolve, 150))
    const interrupted = serverRequest(origin, `${endpoint}?refresh=true`, scope).catch(() => null)
    const config = JSON.parse(await readFile(server.configPath, 'utf8'))
    const database = new DatabaseSync(config.database_url.replace(/^sqlite:\/\//, '').split('?')[0], { readOnly: true })
    let queued
    try {
      for (let attempt = 0; attempt < 100; attempt += 1) {
        queued = database.prepare('SELECT command_id FROM control_maintenance_commands WHERE executor_id=? AND reply_json IS NULL').get(enrollment.executor_id)
        if (queued) break
        await new Promise(resolve => setTimeout(resolve, 50))
      }
      assert.ok(queued, 'status request must be persisted before Server restart')
    } finally { database.close() }
    await stopProcess(server)
    await interrupted
    server = { ...server, ...startProcess(process.env.TERNILO_E2E_SERVER_BINARY ?? path.join(repository, 'target/debug/ternilo-server'), ['serve', '--config-dir', path.dirname(server.configPath)]) }
    await waitForHttp(`${origin}/readyz`, server)
    paused = false
    assert.equal((await serverRequest(origin, `${endpoint}?refresh=true`, scope)).connected, true)
    assert.ok(commands.some(command => command.id === queued.command_id), 'new Server delivers the command queued by the old process')

    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
    const page = await browser.newPage({ locale: 'zh-CN', viewport: { width: 1365, height: 900 }, serviceWorkers: 'block' })
    page.on('pageerror', error => errors.push(error.message))
    page.on('console', message => { if (message.type() === 'error') errors.push(message.text()) })
    page.on('response', response => { if (response.status() >= 400) errors.push(`HTTP ${response.status()} ${new URL(response.url()).pathname}`) })
    await page.goto(`${origin}/settings/computers`)
    await page.getByLabel('用户名', { exact: true }).fill(server.owner.username)
    await page.getByLabel('密码', { exact: true }).fill(server.owner.password)
    await page.getByRole('button', { name: '登录', exact: true }).click()
    await page.locator(`[data-platform-computer="${enrollment.executor_id}"]`).waitFor()
    for (const [locale, badge, open, update, close] of [
      ['zh', '维护在线', '更新客户端', '更新所选电脑（1）', '关闭'],
      ['en', 'Maintenance online', 'Update client', 'Update selected (1)', 'Close'],
      ['ko', '유지보수 연결됨', '클라이언트 업데이트', '선택한 컴퓨터 업데이트 (1)', '닫기'],
    ]) {
      await page.evaluate(locale => localStorage.setItem('ternilo.locale', locale), locale)
      await page.goto(`${origin}/settings/computers`)
      const card = page.locator(`[data-platform-computer="${enrollment.executor_id}"]`)
      await card.getByText(badge, { exact: true }).waitFor()
      await card.locator('[data-computer-version-state="upgrade_required"]').waitFor()
      if (locale === 'zh' && process.env.TERNILO_E2E_ARTIFACT_DIR) {
        await mkdir(process.env.TERNILO_E2E_ARTIFACT_DIR, { recursive: true })
        await page.screenshot({ path: path.join(process.env.TERNILO_E2E_ARTIFACT_DIR, 'maintenance-desktop.png'), fullPage: true, animations: 'disabled' })
      }
      await card.getByRole('button', { name: open, exact: true }).click()
      const dialog = page.locator('[data-computer-updates]')
      await dialog.locator('[data-computer-version-state="upgrade_required"]').waitFor()
      assert.equal(await dialog.getByRole('button', { name: update, exact: true }).isEnabled(), true)
      await page.setViewportSize({ width: 390, height: 700 })
      assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth))
      if (process.env.TERNILO_E2E_ARTIFACT_DIR) {
        await mkdir(process.env.TERNILO_E2E_ARTIFACT_DIR, { recursive: true })
        await page.screenshot({ path: path.join(process.env.TERNILO_E2E_ARTIFACT_DIR, `maintenance-mobile-${locale}.png`), fullPage: true, animations: 'disabled' })
      }
      await dialog.locator('[data-slot="dialog-footer"]').getByRole('button', { name: close, exact: true }).click()
    }
    assert.deepEqual(errors, [])
  } finally {
    running = false
    await polling
    await browser?.close()
    await stopProcess(server)
    await rm(directory, { recursive: true, force: true })
  }
})
