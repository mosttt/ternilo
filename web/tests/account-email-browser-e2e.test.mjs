import assert from 'node:assert/strict'
import { createServer } from 'node:net'
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { chromium } from 'playwright'
import { freePort, initializeServer, serverRequest, startOidcServer, stopProcess } from './platform-e2e-fixture.mjs'
import { until } from './model-device-fixture.mjs'

async function smtpFixture() {
  const messages = [], sockets = new Set(), failures = []
  const server = createServer(socket => {
    sockets.add(socket); socket.on('close', () => sockets.delete(socket)); socket.on('error', () => {})
    socket.write('220 fixture ESMTP\r\n')
    let buffer = '', data = false, lines = []
    socket.on('data', chunk => {
      buffer += chunk.toString()
      let index
      while ((index = buffer.indexOf('\r\n')) >= 0) {
        const line = buffer.slice(0, index); buffer = buffer.slice(index + 2)
        if (data) {
          if (line === '.') { messages.push(lines.join('\r\n')); lines = []; data = false; socket.write('250 accepted\r\n') }
          else lines.push(line.replace(/^\.\./, '.'))
        } else if (line.startsWith('EHLO')) socket.write('250-fixture\r\n250 AUTH PLAIN\r\n')
        else if (line.startsWith('AUTH PLAIN ')) {
          if (Buffer.from(line.slice(11), 'base64').toString() !== '\0fixture-user\0fixture-smtp-password') failures.push('wrong SMTP authentication')
          socket.write('235 authenticated\r\n')
        } else if (line.startsWith('MAIL FROM:') || line.startsWith('RCPT TO:') || line === 'RSET' || line === 'NOOP') socket.write('250 ok\r\n')
        else if (line === 'DATA') { data = true; socket.write('354 send message\r\n') }
        else if (line === 'QUIT') { socket.end('221 bye\r\n') }
        else { failures.push(`unexpected SMTP command ${line.split(' ')[0]}`); socket.write('500 unsupported\r\n') }
      }
    })
  })
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve))
  return { port: server.address().port, messages, failures, close: async () => { for (const socket of sockets) socket.destroy(); await new Promise(resolve => server.close(resolve)) } }
}
function link(message, action) {
  const [headers, ...parts] = message.split('\r\n\r\n')
  const body = parts.join('\r\n\r\n')
  const decoded = /Content-Transfer-Encoding: base64/i.test(headers) ? Buffer.from(body.replace(/\s/g, ''), 'base64').toString() : body.replace(/=\r\n/g, '').replace(/=([A-F\d]{2})/g, (_, hex) => String.fromCharCode(parseInt(hex, 16)))
  const url = decoded.match(new RegExp(`http://127\\.0\\.0\\.1:\\d+/auth/${action}#[A-Za-z0-9_-]+`))?.[0]
  assert.ok(url, 'message contains the requested one-time link')
  return url
}
async function login(page, credentials) {
  await page.getByLabel('用户名', { exact: true }).fill(credentials.username)
  await page.getByLabel('密码', { exact: true }).fill(credentials.password)
  const pending = page.waitForResponse(response => new URL(response.url()).pathname === '/api/v1/auth/login')
  await page.getByRole('button', { name: '登录', exact: true }).click()
  const response = await pending; assert.equal(response.status(), 200)
  await page.getByLabel('用户名', { exact: true }).waitFor({ state: 'hidden' })
  return response.json()
}

test('SMTP settings, verified email and self-service password recovery work in a real browser', { timeout: 180_000 }, async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-account-email-'))
  const artifacts = process.env.TERNILO_E2E_ARTIFACT_DIR ? path.join(process.env.TERNILO_E2E_ARTIFACT_DIR, 'account-email') : undefined
  if (artifacts) await mkdir(artifacts, { recursive: true })
  const smtp = await smtpFixture(), errors = [], network = []
  const provider = await startOidcServer({ audience: 'email-browser', subject: 'email-owner', email: 'browser-owner@example.test', name: 'Email owner' })
  let application, browser, page, revoking = false
  const observe = page => {
    page.on('pageerror', error => errors.push(error.message))
    page.on('console', message => { if (message.type() === 'error' && !(revoking && message.location().url.endsWith('/api/v1/auth/session') && message.text().includes('401'))) errors.push(message.text()) })
    page.on('response', response => { const pathname = new URL(response.url()).pathname; network.push({ path: pathname, status: response.status() }); if (response.status() >= 400 && !(revoking && pathname === '/api/v1/auth/session' && response.status() === 401)) errors.push(`HTTP ${response.status()} ${pathname}`) })
  }
  try {
    application = await initializeServer({ directory, origin: `http://127.0.0.1:${await freePort()}`, mode: 'single_user', oidc: { issuer: provider.issuer, audience: 'email-browser', client_id: 'email-browser' }, ownerOidcToken: provider.accessToken(), databaseUrl: process.env.TERNILO_E2E_DATABASE_URL, migrationDatabaseUrl: process.env.TERNILO_E2E_MIGRATION_DATABASE_URL })
    const { origin, owner } = application
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
    page = await browser.newPage({ viewport: { width: 1440, height: 1000 }, serviceWorkers: 'block' }); observe(page)
    await page.goto(`${origin}/admin/instance`)
    const signedIn = await login(page, owner)
    const settings = page.getByRole('region', { name: '登录与人机验证', exact: true })
    await settings.getByRole('switch', { name: '账号邮件服务', exact: true }).click()
    await settings.getByLabel('SMTP 主机', { exact: true }).fill('127.0.0.1')
    await settings.getByLabel('邮件传输加密', { exact: true }).click()
    await page.getByRole('option', { name: '本机邮件代理（仅回环地址）', exact: true }).click()
    await settings.getByLabel('SMTP 端口', { exact: true }).fill(String(smtp.port))
    await settings.getByLabel('发件邮箱', { exact: true }).fill('ternilo@example.test')
    await settings.getByLabel('SMTP 用户名（可选）', { exact: true }).fill('fixture-user')
    await settings.getByLabel('SMTP 密码', { exact: true }).fill('fixture-smtp-password')
    const saving = page.waitForResponse(response => new URL(response.url()).pathname === '/api/v1/admin/instance/authentication' && response.request().method() === 'PUT')
    await settings.getByRole('button', { name: '保存登录设置', exact: true }).click()
    const saved = await saving; assert.equal(saved.status(), 200)
    const config = await saved.json(); assert.equal(config.smtp.has_password, true); assert.equal(config.smtp.password, undefined)
    assert.ok(!JSON.stringify(config).includes('fixture-smtp-password'))
    if (artifacts) await page.screenshot({ path: path.join(artifacts, 'smtp.png') })
    await page.goto(`${origin}/settings/general`)
    const verification = page.locator('[data-account-email-verification]')
    await verification.getByRole('button', { name: '发送验证邮件', exact: true }).click()
    await until(() => Promise.resolve(smtp.messages), messages => messages.length === 1, 'verification email arrives')
    const verificationUrl = link(smtp.messages[0], 'verify-email')
    const verifyPage = await browser.newPage({ viewport: { width: 390, height: 844 }, serviceWorkers: 'block' }); observe(verifyPage)
    await verifyPage.goto(verificationUrl)
    await verifyPage.getByRole('button', { name: '使用组织账号登录', exact: true }).click()
    await verifyPage.waitForURL(url => url.pathname === '/auth/verify-email')
    await verifyPage.getByRole('button', { name: '验证邮箱', exact: true }).waitFor()
    assert.equal(new URL(verifyPage.url()).hash, '', 'one-time credential is removed from the visible URL')
    await verifyPage.getByRole('button', { name: '验证邮箱', exact: true }).click()
    await verifyPage.getByRole('status').filter({ hasText: '邮箱已验证' }).waitFor()
    assert.ok(await verifyPage.evaluate(() => document.documentElement.scrollWidth <= innerWidth))
    if (artifacts) await verifyPage.screenshot({ path: path.join(artifacts, 'verified-mobile.png') })
    await verifyPage.close()
    await page.reload(); await verification.getByText('邮箱已验证。', { exact: true }).waitFor()
    const unknown = await fetch(`${origin}/api/v1/auth/password-recovery`, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ email: 'absent@example.test' }) })
    assert.equal(unknown.status, 200); const unknownBody = await unknown.json()
    const recoverPage = await browser.newPage({ viewport: { width: 390, height: 844 }, serviceWorkers: 'block' }); observe(recoverPage)
    await recoverPage.goto(origin)
    await recoverPage.getByRole('button', { name: '忘记密码', exact: true }).click()
    await recoverPage.getByLabel('邮箱', { exact: true }).fill(owner.email)
    const request = recoverPage.waitForResponse(response => new URL(response.url()).pathname === '/api/v1/auth/password-recovery')
    await recoverPage.getByRole('button', { name: '发送找回邮件', exact: true }).click()
    const requested = await request; assert.equal(requested.status(), 200); assert.deepEqual(await requested.json(), unknownBody)
    await until(() => Promise.resolve(smtp.messages), messages => messages.length === 2, 'recovery email arrives')
    const resetUrl = link(smtp.messages[1], 'reset-password'), replacement = 'recovered-email-password'
    await recoverPage.goto(resetUrl.split('#')[0] + '#invalid-link')
    await recoverPage.locator('[data-account-email-page=reset]').waitFor()
    await recoverPage.evaluate(fragment => { location.hash = fragment }, new URL(resetUrl).hash)
    await recoverPage.waitForURL(url => url.hash === '')
    await recoverPage.getByLabel('新密码', { exact: true }).fill(replacement)
    await recoverPage.getByLabel('确认新密码', { exact: true }).fill(replacement)
    if (artifacts) await recoverPage.screenshot({ path: path.join(artifacts, 'reset-mobile.png') })
    revoking = true
    await recoverPage.getByRole('button', { name: '保存新密码', exact: true }).click()
    await recoverPage.getByRole('status').filter({ hasText: '密码已更新' }).waitFor()
    const reused = await fetch(`${origin}/api/v1/auth/password-reset`, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ token: new URL(resetUrl).hash.slice(1), password: 'another-password' }) })
    assert.equal(reused.status, 403); await reused.arrayBuffer()
    const limited = await fetch(`${origin}/api/v1/auth/password-recovery`, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ email: 'absent@example.test' }) })
    assert.equal(limited.status, 429); await limited.arrayBuffer()
    for (const token of [signedIn.access_token, owner.session.access_token]) {
      const invalid = await fetch(`${origin}/api/v1/auth/session`, { headers: { authorization: `Bearer ${token}` } }); assert.equal(invalid.status, 401); await invalid.arrayBuffer()
    }
    await recoverPage.getByRole('button', { name: '返回登录', exact: true }).click()
    const after = await login(recoverPage, { ...owner, password: replacement })
    assert.equal(after.user.user_id, signedIn.user.user_id); assert.equal(after.personal_project_id, signedIn.personal_project_id)
    let mailSettings = await serverRequest(origin, '/admin/instance/authentication', { token: after.access_token })
    const { has_password: _secret, ...smtpInput } = mailSettings.smtp
    const saveMail = async (smtp, publicUrl = origin) => {
      const response = await fetch(`${origin}/api/v1/admin/instance/authentication`, { method: 'PUT', headers: { authorization: `Bearer ${after.access_token}`, 'content-type': 'application/json' }, body: JSON.stringify({ revision: mailSettings.revision, public_url: publicUrl, oidc: null, turnstile: null, smtp }) })
      const payload = await response.json(); return { status: response.status, payload }
    }
    const invalidRelay = await saveMail({ ...smtpInput, host: 'smtp.example.test', password: 'fixture-smtp-password' })
    assert.equal(invalidRelay.status, 400, 'unencrypted remote mail cannot be configured')
    const invalidLink = await saveMail({ ...smtpInput, password: 'fixture-smtp-password' }, 'http://example.test')
    assert.equal(invalidLink.status, 403, 'remote recovery links require HTTPS')
    for (const security of ['tls', 'starttls']) {
      const tls = await saveMail({ ...smtpInput, security, password: 'fixture-smtp-password' })
      assert.equal(tls.status, 200, JSON.stringify(tls.payload)); mailSettings = tls.payload
      assert.equal(mailSettings.smtp.password, undefined)
    }
    assert.equal(smtp.messages.length, 2); assert.deepEqual(smtp.failures, []); assert.deepEqual(errors, [])
    if (artifacts) await writeFile(path.join(artifacts, 'result.json'), JSON.stringify({ status: 'passed', deliveries: smtp.messages.length, errors, network }))
  } catch (error) {
    if (page && artifacts) await page.screenshot({ path: path.join(artifacts, 'failure.png') }).catch(() => {})
    throw error
  } finally {
    await browser?.close(); if (application) await stopProcess(application)
    await smtp.close(); await provider.close(); await rm(directory, { recursive: true, force: true })
  }
})
