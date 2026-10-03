import assert from 'node:assert/strict'
import { createHmac } from 'node:crypto'
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { chromium } from 'playwright'
import { smtpFixture, emailLink } from './account-email-fixture.mjs'
import { execute, freePort, initializeServer, repository, serverRequest, startOidcServer, stopProcess } from './platform-e2e-fixture.mjs'
import { until } from './model-device-fixture.mjs'

function code(secret, time = Date.now()) {
  let bits = 0, value = 0; const bytes = []
  for (const character of secret) {
    const digit = 'ABCDEFGHIJKLMNOPQRSTUVWXYZ234567'.indexOf(character); assert.ok(digit >= 0)
    value = (value << 5) | digit; bits += 5
    if (bits >= 8) { bits -= 8; bytes.push((value >>> bits) & 255) }
  }
  const counter = Buffer.alloc(8); counter.writeBigUInt64BE(BigInt(Math.floor(time / 30_000)))
  const digest = createHmac('sha1', Buffer.from(bytes)).update(counter).digest(), offset = digest.at(-1) & 15
  return String((digest.readUInt32BE(offset) & 0x7fffffff) % 1_000_000).padStart(6, '0')
}
async function submitLogin(page, owner, password, mfa = null) {
  await page.getByLabel('用户名', { exact: true }).fill(owner.username)
  await page.getByLabel('密码', { exact: true }).fill(password)
  let pending = page.waitForResponse(response => new URL(response.url()).pathname === '/api/v1/auth/login')
  await page.getByRole('button', { name: '登录', exact: true }).click()
  let response = await pending
  if (mfa) {
    assert.equal(response.status(), 403)
    assert.equal((await response.json()).error.message, 'multi-factor verification is required')
    await page.getByLabel('验证码或恢复码', { exact: true }).fill(mfa)
    pending = page.waitForResponse(response => new URL(response.url()).pathname === '/api/v1/auth/login')
    await page.getByRole('button', { name: '登录', exact: true }).click(); response = await pending
  }
  assert.equal(response.status(), 200)
  await page.getByLabel('用户名', { exact: true }).waitFor({ state: 'hidden' })
  return response.json()
}

test('native and OIDC MFA require a second factor, email recovery retains it, and private operator recovery restores access', { timeout: 180_000 }, async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-account-mfa-'))
  const artifacts = process.env.TERNILO_E2E_ARTIFACT_DIR ? path.join(process.env.TERNILO_E2E_ARTIFACT_DIR, 'account-mfa') : undefined
  if (artifacts) await mkdir(artifacts, { recursive: true })
  const smtp = await smtpFixture(), provider = await startOidcServer({ audience: 'mfa-browser', subject: 'mfa-owner', email: 'mfa-owner@example.test', name: 'MFA owner' })
  const errors = [], network = []
  let application, browser, page, revoking = false
  const observe = page => {
    page.on('pageerror', error => errors.push(error.message))
    page.on('console', message => {
      if (message.type() !== 'error') return
      const pathname = new URL(message.location().url || 'http://fixture').pathname
      if (pathname === '/api/v1/auth/login' && message.text().includes('403')) return
      if (revoking && pathname === '/api/v1/auth/session' && message.text().includes('401')) return
      errors.push(message.text())
    })
    page.on('response', response => {
      const pathname = new URL(response.url()).pathname, status = response.status(); network.push({ path: pathname, status })
      if (status >= 400 && !(pathname === '/api/v1/auth/login' && status === 403) && !(revoking && pathname === '/api/v1/auth/session' && status === 401)) errors.push(`HTTP ${status} ${pathname}`)
    })
  }
  try {
    application = await initializeServer({ directory, origin: `http://127.0.0.1:${await freePort()}`, mode: 'single_user',
      oidc: { issuer: provider.issuer, audience: 'mfa-browser', client_id: 'mfa-browser' }, ownerOidcToken: provider.accessToken(),
      owner: { username: 'mfa-owner', email: 'mfa-owner@example.test' }, databaseUrl: process.env.TERNILO_E2E_DATABASE_URL, migrationDatabaseUrl: process.env.TERNILO_E2E_MIGRATION_DATABASE_URL })
    const { origin, owner } = application; let access = owner.session.access_token
    const api = (resource, options = {}) => serverRequest(origin, resource, { token: access, ...options })
    const settings = await api('/admin/instance/authentication')
    const { has_client_secret: _secret, ...oidc } = settings.oidc
    await api('/admin/instance/authentication', { method: 'PUT', body: { revision: settings.revision, public_url: origin, oidc, turnstile: null,
      smtp: { host: '127.0.0.1', port: smtp.port, security: 'local', from: 'ternilo@example.test', username: 'fixture-user', password: 'fixture-smtp-password' } } })
    await api('/auth/email/send', { method: 'POST' })
    await until(() => Promise.resolve(smtp.messages), values => values.length === 1, 'email verification arrives')
    await api('/auth/email/verify', { body: { token: new URL(emailLink(smtp.messages[0], 'verify-email')).hash.slice(1) } })
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
    page = await browser.newPage({ viewport: { width: 1440, height: 1100 }, serviceWorkers: 'block' }); observe(page)
    await page.addInitScript(() => localStorage.setItem('ternilo.theme', 'dark'))
    await page.goto(`${origin}/settings/general`)
    const first = await submitLogin(page, owner, owner.password); access = first.access_token
    const mfa = page.locator('[data-account-mfa]')
    await mfa.locator('summary').first().click()
    await mfa.getByLabel('当前密码', { exact: true }).fill(owner.password)
    await mfa.getByRole('button', { name: '设置验证器', exact: true }).click()
    const enrollment = mfa.locator('[data-mfa-enrollment]'); await enrollment.waitFor()
    await enrollment.locator('summary').click()
    const secret = await enrollment.locator('[data-mfa-secret]').textContent()
    const recovery = await enrollment.locator('[data-mfa-recovery-code]').allTextContents(); assert.equal(recovery.length, 8)
    assert.ok(await enrollment.locator('img').evaluate(async image => { await image.decode(); return image.naturalWidth > 0 }))
    await mfa.getByLabel('验证器验证码', { exact: true }).fill(code(secret))
    assert.equal(await mfa.getByRole('button', { name: '确认启用并退出', exact: true }).isDisabled(), true)
    await mfa.getByLabel('我已保存恢复码', { exact: true }).check()
    await page.setViewportSize({ width: 390, height: 844 }); await enrollment.scrollIntoViewIfNeeded()
    assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth))
    assert.ok(await mfa.evaluate(element => element.scrollWidth <= element.clientWidth))
    if (artifacts) await page.screenshot({ path: path.join(artifacts, 'setup-mobile.png') })
    revoking = true
    await mfa.getByRole('button', { name: '确认启用并退出', exact: true }).click()
    await page.getByLabel('用户名', { exact: true }).waitFor()
    const nativeCode = code(secret, Date.now() + 30_000)
    const native = await submitLogin(page, owner, owner.password, nativeCode); access = native.access_token
    assert.equal(native.user.user_id, first.user.user_id)
    const replay = await fetch(`${origin}/api/v1/auth/login`, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ username: owner.username, password: owner.password, mfa_code: nativeCode }) })
    assert.equal(replay.status, 403); await replay.arrayBuffer()
    const rawOidc = await fetch(`${origin}/api/v1/auth/session`, { headers: { authorization: `Bearer ${provider.accessToken()}` } })
    assert.equal(rawOidc.status, 401); await rawOidc.arrayBuffer()
    const oidcPage = await browser.newPage({ viewport: { width: 390, height: 844 }, serviceWorkers: 'block' }); observe(oidcPage)
    await oidcPage.goto(origin); await oidcPage.getByRole('button', { name: '使用组织账号登录', exact: true }).click()
    const challengeForm = oidcPage.locator('[data-oidc-mfa]'); await challengeForm.waitFor()
    assert.equal(await oidcPage.evaluate(() => sessionStorage.getItem('ternilo.oidc.access')), null)
    const challenge = await oidcPage.evaluate(() => JSON.parse(sessionStorage.getItem('ternilo.oidc.mfa')).mfa_challenge)
    const pendingAccess = await fetch(`${origin}/api/v1/auth/session`, { headers: { authorization: `Bearer ${challenge}` } })
    assert.equal(pendingAccess.status, 401); await pendingAccess.arrayBuffer()
    if (artifacts) await oidcPage.screenshot({ path: path.join(artifacts, 'oidc-code-mobile.png') })
    await challengeForm.getByLabel('验证码或恢复码', { exact: true }).fill(recovery[0])
    await challengeForm.getByRole('button', { name: '验证并登录', exact: true }).click()
    await oidcPage.locator('[data-app-frame]').waitFor(); await challengeForm.waitFor({ state: 'hidden' })
    const oldOidc = await oidcPage.evaluate(() => sessionStorage.getItem('ternilo.oidc.access'))
    await oidcPage.evaluate(() => sessionStorage.setItem('ternilo.oidc.expires', String(Date.now() - 1)))
    const refreshing = oidcPage.waitForResponse(response => new URL(response.url()).pathname === '/auth/refresh')
    await oidcPage.reload(); assert.equal((await refreshing).status(), 200); await oidcPage.locator('[data-app-frame]').waitFor()
    assert.notEqual(await oidcPage.evaluate(() => sessionStorage.getItem('ternilo.oidc.access')), oldOidc)
    await oidcPage.close()
    const recovering = await browser.newPage({ viewport: { width: 390, height: 844 }, serviceWorkers: 'block' }); observe(recovering)
    await recovering.goto(`${origin}/auth/recover`)
    await recovering.getByLabel('邮箱', { exact: true }).fill(owner.email)
    await recovering.getByRole('button', { name: '发送找回邮件', exact: true }).click()
    await until(() => Promise.resolve(smtp.messages), values => values.length === 2, 'MFA account recovery email arrives')
    await recovering.goto(emailLink(smtp.messages[1], 'reset-password'))
    const password = 'new-mfa-email-password'
    await recovering.getByLabel('新密码', { exact: true }).fill(password)
    await recovering.getByLabel('确认新密码', { exact: true }).fill(password)
    await recovering.getByRole('button', { name: '保存新密码', exact: true }).click()
    await recovering.getByRole('status').filter({ hasText: '密码已更新' }).waitFor()
    await recovering.getByRole('button', { name: '返回登录', exact: true }).click()
    const afterRecovery = await submitLogin(recovering, owner, password, recovery[1]); access = afterRecovery.access_token
    assert.equal(afterRecovery.personal_project_id, first.personal_project_id)
    assert.equal((await api('/auth/mfa')).recovery_codes_remaining, 6)
    await recovering.goto(`${origin}/settings/general`)
    const current = recovering.locator('[data-account-mfa]'); await current.locator('summary').first().click()
    await current.getByLabel('当前密码', { exact: true }).fill(password)
    await current.getByLabel('验证码或恢复码', { exact: true }).fill(recovery[2])
    await current.getByRole('button', { name: '停用两步验证并退出', exact: true }).click()
    const disabled = await submitLogin(recovering, owner, password); access = disabled.access_token
    assert.equal((await api('/auth/mfa')).enabled, false)
    const second = await api('/auth/mfa/setup', { body: { current_password: password } })
    await api('/auth/mfa/enable', { body: { current_password: password, generation: second.generation, code: code(second.secret) } })
    const result = await execute(process.env.TERNILO_E2E_SERVER_BINARY ?? path.join(repository, 'target/debug/ternilo-server'), ['admin', 'reset-mfa', '--config-dir', directory, '--username', owner.username], { cwd: repository, env: Object.fromEntries(Object.entries(process.env).filter(([key]) => !key.startsWith('TERNILO_'))) })
    assert.ok(result.stdout.includes(first.user.user_id)); assert.ok(!result.stdout.includes(second.secret)); assert.match(result.stdout, /MFA reset/)
    await recovering.goto(origin)
    const restored = await submitLogin(recovering, owner, password); access = restored.access_token
    assert.equal(restored.user.user_id, first.user.user_id); assert.equal((await api('/auth/mfa')).enabled, false)
    assert.deepEqual(smtp.failures, []); assert.deepEqual(errors, [])
    if (artifacts) await writeFile(path.join(artifacts, 'result.json'), JSON.stringify({ status: 'passed', errors, network, emailDeliveries: smtp.messages.length }))
  } catch (error) {
    if (page && artifacts) await page.screenshot({ path: path.join(artifacts, 'failure.png') }).catch(() => {})
    throw error
  } finally {
    await browser?.close(); if (application) await stopProcess(application)
    await smtp.close(); await provider.close(); await rm(directory, { recursive: true, force: true })
  }
})
