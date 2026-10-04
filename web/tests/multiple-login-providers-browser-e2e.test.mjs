import assert from 'node:assert/strict'
import { mkdtemp, mkdir, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { chromium } from 'playwright'
import { freePort, initializeServer, repository, serverRequest, startOidcServer, startProcess, stopProcess, waitForHttp } from './platform-e2e-fixture.mjs'

const binary = process.env.TERNILO_E2E_SERVER_BINARY ?? path.join(repository, 'target/debug/ternilo-server')

test('Named OIDC providers, OAuth-only open and invited registration, isolation and live disabling work in a real browser', { timeout: 240_000 }, async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-multiple-login-'))
  const artifacts = process.env.TERNILO_E2E_ARTIFACT_DIR ?? directory
  await mkdir(artifacts, { recursive: true })
  const first = await startOidcServer({ subject: 'shared-subject', email: 'shared@example.test', opaqueAccessTokens: true, validateClient: form => form.get('client_id') === 'first-client' && form.get('client_secret') === 'first-secret' })
  const second = await startOidcServer({ subject: 'shared-subject', email: 'shared@example.test', opaqueAccessTokens: true, validateClient: form => form.get('client_id') === 'second-client' && form.get('client_secret') === 'second-secret' })
  const origin = `http://127.0.0.1:${await freePort()}`
  let application, browser, adminPage
  const errors = [], failures = []
  try {
    application = await initializeServer({ directory, origin, oidc: { issuer: first.issuer, audience: '', client_id: 'first-client' }, databaseUrl: process.env.TERNILO_E2E_DATABASE_URL, migrationDatabaseUrl: process.env.TERNILO_E2E_MIGRATION_DATABASE_URL })
    const adminToken = application.owner.session.access_token
    const admin = (resource, options = {}) => serverRequest(origin, resource, { token: adminToken, ...options })
    await admin('/admin/instance/authentication', { method: 'PUT', body: { revision: 0, public_url: origin, turnstile: null, smtp: null, oidc_providers: [
      { id: 'first', name: '团队账号', enabled: true, issuer: first.issuer, audience: '', client_id: 'first-client', scopes: 'openid profile email', token_auth_method: 'client_secret_post', client_secret: 'first-secret' },
      { id: 'second', name: '社区账号', enabled: true, issuer: second.issuer, audience: '', client_id: 'second-client', scopes: 'openid profile email', token_auth_method: 'client_secret_post', client_secret: 'second-secret' },
    ] } })
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
    const observe = page => {
      page.on('pageerror', error => errors.push(error.message))
      page.on('response', response => { if (response.url().startsWith(origin) && response.status() >= 500) failures.push(`${response.status()} ${response.url()}`) })
    }
    const signIn = async (page, name) => {
      const exchange = page.waitForResponse(response => response.url() === `${origin}/auth/token`)
      await page.getByRole('button', { name: `使用 ${name} 登录`, exact: true }).click()
      const response = await exchange
      assert.equal(response.status(), 200, await response.text())
      return response.json()
    }
    adminPage = await browser.newPage({ locale: 'zh-CN', viewport: { width: 1280, height: 1200 }, serviceWorkers: 'block' })
    observe(adminPage)
    await adminPage.goto(`${origin}/admin/accounts`)
    await adminPage.getByLabel('用户名', { exact: true }).fill(application.owner.username)
    await adminPage.getByLabel('密码', { exact: true }).fill(application.owner.password)
    await adminPage.getByRole('button', { name: '登录', exact: true }).click()
    await adminPage.getByLabel('仅限 OAuth2 方式注册').check()
    await adminPage.getByRole('combobox', { name: '注册方式' }).click()
    await adminPage.getByRole('option', { name: '开放注册', exact: true }).click()
    const policySaved = adminPage.waitForResponse(response => response.url().endsWith('/admin/registration') && response.request().method() === 'PATCH')
    await adminPage.getByRole('button', { name: '保存注册设置', exact: true }).click()
    assert.equal((await policySaved).status(), 200)
    assert.equal((await admin('/admin/registration')).oidc_only, true)
    const passwordRegistration = await fetch(`${origin}/api/v1/auth/register`, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ username: 'password-bypass', email: 'password-bypass@example.test', password: 'fixture-password' }) })
    assert.equal(passwordRegistration.status, 403)
    const alpha = await browser.newPage({ locale: 'zh-CN', viewport: { width: 390, height: 900 }, isMobile: true, hasTouch: true, serviceWorkers: 'block' })
    observe(alpha)
    await alpha.goto(origin)
    await alpha.locator('[data-oidc-provider]').first().waitFor()
    assert.equal(await alpha.locator('[data-oidc-provider]').count(), 2)
    await alpha.getByRole('button', { name: '注册新账号', exact: true }).click()
    assert.equal(await alpha.locator('#server-password').count(), 0)
    const firstTokens = await signIn(alpha, '团队账号')
    await alpha.locator('[data-oidc-registration]').waitFor()
    await alpha.getByLabel('用户名', { exact: true }).fill('alpha-user')
    await alpha.getByLabel('邮箱', { exact: true }).fill('shared@example.test')
    await alpha.getByRole('button', { name: '完成注册并继续', exact: true }).click()
    await alpha.getByRole('button', { name: '退出登录', exact: true }).waitFor()
    const firstIdentity = await serverRequest(origin, '/auth/session', { token: firstTokens.access_token })
    const beta = await browser.newPage({ locale: 'zh-CN', viewport: { width: 390, height: 900 }, serviceWorkers: 'block' })
    observe(beta)
    await beta.goto(origin)
    const secondTokens = await signIn(beta, '社区账号')
    await beta.locator('[data-oidc-registration]').waitFor()
    assert.notEqual(await beta.locator('#server-username').count(), 0, 'Equal upstream subjects and emails do not silently merge accounts from different issuers')
    await beta.getByLabel('用户名', { exact: true }).fill('beta-user')
    await beta.getByLabel('邮箱', { exact: true }).fill('beta-contact@example.test')
    await beta.getByRole('button', { name: '完成注册并继续', exact: true }).click()
    await beta.getByRole('button', { name: '退出登录', exact: true }).waitFor()
    const secondIdentity = await serverRequest(origin, '/auth/session', { token: secondTokens.access_token })
    assert.notEqual(firstIdentity.user.user_id, secondIdentity.user.user_id)
    await adminPage.goto(`${origin}/admin/accounts`)
    const statuses = adminPage.locator('[data-account-status="active"]')
    await statuses.first().waitFor()
    assert.ok(await statuses.count() >= 3)
    for (const theme of ['light', 'dark']) {
      await adminPage.evaluate(theme => { localStorage.setItem('ternilo.theme', theme) }, theme)
      await adminPage.reload()
      await statuses.first().waitFor()
      await adminPage.screenshot({ path: path.join(artifacts, `account-status-${theme}.png`) })
    }
    await adminPage.goto(`${origin}/admin/instance`)
    const providerCard = adminPage.locator('[data-login-provider="first"]')
    await providerCard.getByLabel('登录方式名称').fill('研发账号')
    const saved = adminPage.waitForResponse(response => response.url().endsWith('/admin/instance/authentication') && response.request().method() === 'PUT')
    await adminPage.getByRole('button', { name: '保存登录设置', exact: true }).click()
    assert.equal((await saved).status(), 200)
    assert.equal((await serverRequest(origin, '/auth/session', { token: firstTokens.access_token })).user.user_id, firstIdentity.user.user_id, 'Renaming keeps existing sessions')
    await adminPage.setViewportSize({ width: 320, height: 1100 })
    await providerCard.scrollIntoViewIfNeeded()
    assert.ok(await adminPage.evaluate(() => document.documentElement.scrollWidth <= innerWidth))
    await adminPage.screenshot({ path: path.join(artifacts, 'multiple-login-settings-mobile.png') })
    await providerCard.getByRole('switch').click()
    const disabled = adminPage.waitForResponse(response => response.url().endsWith('/admin/instance/authentication') && response.request().method() === 'PUT')
    await adminPage.getByRole('button', { name: '保存登录设置', exact: true }).click()
    assert.equal((await disabled).status(), 200)
    assert.equal((await fetch(`${origin}/api/v1/auth/session`, { headers: { authorization: `Bearer ${firstTokens.access_token}` } })).status, 401)
    assert.equal((await fetch(`${origin}/auth/refresh`, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ provider_id: 'first', refresh_token: secondTokens.refresh_token }) })).status, 403, 'Disabled providers cannot refresh another provider’s session')
    assert.equal((await serverRequest(origin, '/auth/session', { token: secondTokens.access_token })).user.user_id, secondIdentity.user.user_id)
    const settings = await admin('/admin/instance/authentication')
    assert.ok(settings.oidc_providers.every(provider => provider.has_client_secret && provider.client_secret === undefined))
    assert.ok(!JSON.stringify(settings).includes('second-secret'))
    const registration = await admin('/admin/registration')
    await admin('/admin/registration', { method: 'PATCH', body: { ...registration, mode: 'invite', require_approval: false } })
    const invitation = await admin('/admin/invitations', { body: { tenant_id: null, role: 'member', expires_in_seconds: 300 } })
    const nativeInvite = await fetch(`${origin}/api/v1/auth/invitations/accept`, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ token: invitation.token, username: 'native-invited', email: 'native-invited@example.test', password: 'fixture-password' }) })
    assert.equal(nativeInvite.status, 403)
    await beta.close()
    await alpha.close()
    const third = await startOidcServer({ subject: 'invited-subject', email: 'invited@example.test', opaqueAccessTokens: true })
    try {
      const input = settings.oidc_providers.map(({ has_client_secret: _, available: _available, ...provider }) => provider)
      await admin('/admin/instance/authentication', { method: 'PUT', body: { revision: settings.revision, public_url: origin, turnstile: null, smtp: null, oidc_providers: [...input, { id: 'invited', name: '邀请账号', enabled: true, issuer: third.issuer, audience: '', client_id: 'invited-client', scopes: 'openid profile email', token_auth_method: 'none', client_secret: null }] } })
      const invitedPage = await browser.newPage({ locale: 'zh-CN', viewport: { width: 390, height: 900 }, serviceWorkers: 'block' })
      observe(invitedPage)
      await invitedPage.goto(`${origin}/#invite=${invitation.token}`)
      assert.equal(await invitedPage.getByLabel('邀请令牌', { exact: true }).inputValue(), invitation.token)
      assert.equal(await invitedPage.locator('#server-password').count(), 0)
      await signIn(invitedPage, '邀请账号')
      await invitedPage.locator('[data-oidc-registration]').waitFor()
      assert.equal(await invitedPage.locator('#oidc-invitation-token').inputValue(), invitation.token)
      await invitedPage.getByLabel('用户名', { exact: true }).fill('oidc-invited')
      await invitedPage.getByLabel('邮箱', { exact: true }).fill('invited@example.test')
      await invitedPage.getByRole('button', { name: '完成注册并继续', exact: true }).click()
      await invitedPage.getByRole('button', { name: '退出登录', exact: true }).waitFor()
      await invitedPage.close()
    } finally { await third.close() }
    await stopProcess(application)
    application = startProcess(binary, ['serve', '--config-dir', directory], Object.fromEntries(Object.keys(process.env).filter(key => key.startsWith('TERNILO_')).map(key => [key, undefined])))
    await waitForHttp(`${origin}/readyz`, application)
    const persisted = await admin('/admin/instance/authentication')
    assert.equal(persisted.oidc_providers[0].name, '研发账号')
    assert.equal(persisted.oidc_providers[0].enabled, false)
    assert.equal((await admin('/admin/registration')).oidc_only, true)
    assert.deepEqual(errors, [])
    assert.deepEqual(failures, [])
  } catch (error) {
    await adminPage?.screenshot({ path: path.join(artifacts, 'multiple-login-failure.png') }).catch(() => {})
    throw new Error(`${error.stack}\n${application?.diagnostics() ?? ''}`)
  } finally {
    await browser?.close()
    await stopProcess(application)
    await first.close(); await second.close()
    await rm(directory, { recursive: true, force: true })
  }
})
