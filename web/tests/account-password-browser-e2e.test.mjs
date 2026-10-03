import assert from 'node:assert/strict'
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { chromium } from 'playwright'
import { freePort, initializeServer, serverRequest, stopProcess } from './platform-e2e-fixture.mjs'

async function login(page, username, password) {
  await page.getByLabel('用户名', { exact: true }).fill(username)
  await page.getByLabel('密码', { exact: true }).fill(password)
  const pending = page.waitForResponse(response => new URL(response.url()).pathname === '/api/v1/auth/login')
  await page.getByRole('button', { name: '登录', exact: true }).click()
  const response = await pending
  assert.equal(response.status(), 200)
  await page.getByLabel('用户名', { exact: true }).waitFor({ state: 'hidden' })
  return response.json()
}

test('self-service password changes revoke old browser access, retain resources and require the current password', { timeout: 120_000 }, async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-account-password-'))
  const artifacts = process.env.TERNILO_E2E_ARTIFACT_DIR ? path.join(process.env.TERNILO_E2E_ARTIFACT_DIR, 'account-password') : undefined
  if (artifacts) await mkdir(artifacts, { recursive: true })
  const errors = [], network = []
  let application, browser, page, changing = false, incorrect = false
  const replacement = ' changed password with spaces '
  try {
    application = await initializeServer({ directory, origin: `http://127.0.0.1:${await freePort()}`, mode: 'single_user',
      databaseUrl: process.env.TERNILO_E2E_DATABASE_URL, migrationDatabaseUrl: process.env.TERNILO_E2E_MIGRATION_DATABASE_URL })
    const { origin, owner } = application
    const { project } = await serverRequest(origin, `/tenants/${owner.session.personal_tenant_id}/projects`, {
      token: owner.session.access_token, body: { name: 'Retained password project' },
    })
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
    page = await browser.newPage({ viewport: { width: 1440, height: 1000 }, serviceWorkers: 'block' })
    page.on('pageerror', error => errors.push(error.message))
    page.on('console', message => {
      if (message.type() !== 'error') return
      const pathname = new URL(message.location().url || origin).pathname
      if (incorrect && pathname === '/api/v1/auth/password' && message.text().includes('403')) return
      if (changing && pathname === '/api/v1/auth/session' && message.text().includes('401')) return
      errors.push(message.text())
    })
    page.on('response', response => {
      const pathname = new URL(response.url()).pathname, status = response.status()
      network.push({ path: pathname, status })
      if (status < 400 || (incorrect && pathname === '/api/v1/auth/password' && status === 403)
        || (changing && pathname === '/api/v1/auth/session' && status === 401)) return
      errors.push(`HTTP ${status}: ${pathname}`)
    })
    await page.goto(origin)
    const before = await login(page, owner.username, owner.password)
    const initialSessions = await serverRequest(origin, '/auth/sessions', { token: before.access_token })
    await page.getByRole('button', { name: '用户设置', exact: true }).click()
    const settings = page.locator('[data-user-settings]')
    await settings.getByRole('button', { name: '通用', exact: true }).click()
    const form = settings.locator('[data-account-password]')
    await form.locator('summary').click()
    await form.getByLabel('当前密码', { exact: true }).fill('incorrect-password')
    await form.getByLabel('新密码', { exact: true }).fill(replacement)
    await form.getByLabel('确认新密码', { exact: true }).fill('different-password')
    const submit = () => form.getByRole('button', { name: '修改密码并退出登录', exact: true }).click()
    await submit()
    await form.getByRole('alert').waitFor()
    assert.equal(network.filter(request => request.path === '/api/v1/auth/password').length, 0, 'mismatched confirmation does not submit')
    await form.getByLabel('确认新密码', { exact: true }).fill(replacement)
    incorrect = true
    const denied = page.waitForResponse(response => new URL(response.url()).pathname === '/api/v1/auth/password')
    await submit()
    assert.equal((await denied).status(), 403)
    await form.getByRole('alert').waitFor()
    assert.equal((await serverRequest(origin, '/auth/session', { token: before.access_token })).user.user_id, before.user.user_id)
    await form.getByLabel('当前密码', { exact: true }).fill(owner.password)
    if (artifacts) await page.screenshot({ path: path.join(artifacts, 'desktop.png') })
    await page.setViewportSize({ width: 390, height: 844 })
    await form.scrollIntoViewIfNeeded()
    assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth))
    assert.ok(await form.evaluate(element => element.scrollWidth <= element.clientWidth))
    if (artifacts) await page.screenshot({ path: path.join(artifacts, 'mobile.png') })
    changing = true
    const saved = page.waitForResponse(response => new URL(response.url()).pathname === '/api/v1/auth/password')
    await submit()
    const response = await saved
    assert.equal(response.status(), 200)
    assert.equal(response.headers()['cache-control'], 'no-store')
    const result = await response.json()
    assert.equal(result.native_sessions_revoked, initialSessions.sessions.length)
    assert.ok(!JSON.stringify(result).includes(replacement))
    await page.getByLabel('用户名', { exact: true }).waitFor()
    for (const token of [owner.session.access_token, before.access_token]) {
      const revoked = await fetch(`${origin}/api/v1/auth/session`, { headers: { authorization: `Bearer ${token}` } })
      assert.equal(revoked.status, 401); await revoked.arrayBuffer()
    }
    const oldLogin = await fetch(`${origin}/api/v1/auth/login`, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ username: owner.username, password: owner.password }) })
    assert.equal(oldLogin.status, 401); await oldLogin.arrayBuffer()
    const after = await login(page, owner.username, replacement)
    assert.equal(after.user.user_id, before.user.user_id)
    assert.equal(after.personal_project_id, before.personal_project_id)
    const projects = await serverRequest(origin, '/projects', { token: after.access_token, tenantId: after.personal_tenant_id })
    assert.ok(projects.projects.some(row => row.project_id === project.project_id))
    assert.deepEqual(errors, [])
    if (artifacts) await writeFile(path.join(artifacts, 'result.json'), JSON.stringify({ status: 'passed', errors, network }))
  } catch (error) {
    if (artifacts && page) await page.screenshot({ path: path.join(artifacts, 'failure.png') }).catch(() => {})
    throw error
  } finally {
    await browser?.close(); if (application) await stopProcess(application)
    await rm(directory, { recursive: true, force: true })
  }
})
