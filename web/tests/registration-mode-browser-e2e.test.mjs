import assert from 'node:assert/strict'
import { mkdir, mkdtemp, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { chromium } from 'playwright'
import { selectChoice } from './browser-select-fixture.mjs'
import { freePort, initializeServer, serverRequest, stopProcess } from './platform-e2e-fixture.mjs'

test('single-user accounts hide registration controls while mode changes preserve the stored multi-user policy', { timeout: 90_000 }, async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-registration-mode-'))
  let server, browser
  try {
    const origin = `http://127.0.0.1:${await freePort()}`
    server = await initializeServer({ directory: path.join(directory, 'server'), origin, mode: 'single_user' })
    const token = server.owner.session.access_token
    const original = await serverRequest(origin, '/admin/registration', { token })
    await serverRequest(origin, '/admin/registration', { token, method: 'PATCH', body: { mode: 'open', require_approval: true, revision: original.revision } })
    const denied = await fetch(`${origin}/api/v1/auth/register`, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ username: 'single-user-candidate', email: 'single-user-candidate@example.test', password: 'single-user-candidate-password' }) })
    assert.equal(denied.status, 403, 'single-user mode must reject signup even with a saved open policy')
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
    const page = await browser.newPage({ locale: 'zh-CN', viewport: { width: 1365, height: 900 }, serviceWorkers: 'block' })
    if (process.env.TERNILO_E2E_ASSET_DIR) {
      for (const [asset, contentType] of [['app.js', 'text/javascript'], ['app.css', 'text/css']]) {
        await page.route(`${origin}/assets/${asset}`, route => route.fulfill({ path: path.join(process.env.TERNILO_E2E_ASSET_DIR, asset), contentType }))
      }
    }
    const errors = [], policyRequests = []
    page.on('pageerror', error => errors.push(error.message))
    page.on('console', message => { if (message.type() === 'error') errors.push(message.text()) })
    page.on('response', response => { if (response.status() >= 400) errors.push(`HTTP ${response.status()}: ${new URL(response.url()).pathname}`) })
    page.on('request', request => {
      const url = new URL(request.url())
      if (url.pathname === '/api/v1/admin/registration' || url.pathname === '/api/v1/admin/invitations') policyRequests.push(url.pathname)
    })
    await page.goto(`${origin}/admin/accounts`)
    await page.getByLabel('用户名', { exact: true }).fill(server.owner.username)
    await page.getByLabel('密码', { exact: true }).fill(server.owner.password)
    await page.getByRole('button', { name: '登录', exact: true }).click()
    await page.locator('[data-registration-disabled]').waitFor()
    assert.equal(await page.locator('[data-registration-settings]').count(), 0)
    assert.equal(await page.locator('[data-admin-invitations]').count(), 0)
    assert.deepEqual(policyRequests, [], 'single-user mode should not read inactive registration settings')
    let instance = await serverRequest(origin, '/admin/instance', { token })
    instance = await serverRequest(origin, '/admin/instance', { token, method: 'PATCH', body: { mode: 'multi_user', revision: instance.revision } })
    await page.reload()
    const settings = page.locator('[data-registration-settings]')
    await settings.waitFor()
    assert.equal(await settings.getByRole('combobox', { name: '注册方式', exact: true }).getAttribute('data-choice-value'), 'open')
    assert.equal(await settings.getByRole('checkbox', { name: /新注册账号需要审核/ }).isChecked(), true)
    await selectChoice(settings.getByRole('combobox', { name: '注册方式', exact: true }), 'invite')
    await settings.getByRole('button', { name: '保存注册设置', exact: true }).click()
    await page.locator('[data-admin-invitations]').waitFor()
    const stored = await serverRequest(origin, '/admin/registration', { token })
    instance = await serverRequest(origin, '/admin/instance', { token, method: 'PATCH', body: { mode: 'single_user', revision: instance.revision } })
    const beforeReload = policyRequests.length
    await page.reload()
    await page.locator('[data-registration-disabled]').waitFor()
    assert.equal(await page.locator('[data-registration-settings]').count(), 0)
    assert.equal(await page.locator('[data-admin-invitations]').count(), 0)
    assert.equal(policyRequests.length, beforeReload)
    assert.deepEqual(await serverRequest(origin, '/admin/registration', { token }), stored, 'switching mode must not rewrite the saved registration policy')
    const artifacts = process.env.TERNILO_E2E_ARTIFACT_DIR
    if (artifacts) {
      await mkdir(artifacts, { recursive: true })
      await page.screenshot({ path: path.join(artifacts, 'single-user-accounts-registration-hidden.png'), fullPage: true })
    }
    assert.deepEqual(errors, [])
  } finally {
    await browser?.close()
    await stopProcess(server)
    await rm(directory, { recursive: true, force: true })
  }
})
