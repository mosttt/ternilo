import assert from 'node:assert/strict'
import { mkdir, mkdtemp, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { chromium } from 'playwright'
import { execute, freePort, initializeServer, repository, serverRequest, startOidcServer, startProcess, stopProcess, waitForHttp } from './platform-e2e-fixture.mjs'

const binary = process.env.TERNILO_E2E_SERVER_BINARY ?? path.join(repository, 'target/debug/ternilo-server')
const cleanEnvironment = Object.fromEntries(Object.keys(process.env).filter(key => key.startsWith('TERNILO_')).map(key => [key, undefined]))
const resource = '/admin/instance/authentication'

test('Owner configures live OAuth and Turnstile, with responsive UI, persisted secrets and operator recovery', { timeout: 180000 }, async () => {
  const temporary = await mkdtemp(path.join(tmpdir(), 'ternilo-server-security-'))
  const artifacts = process.env.TERNILO_E2E_ARTIFACT_DIR ?? temporary
  await mkdir(artifacts, { recursive: true })
  const origin = `http://127.0.0.1:${await freePort()}`
  const secret = 'client:secret + with spaces'
  const encoded = value => new URLSearchParams({ value }).toString().slice('value='.length)
  let expectSecret = false
  const tokenRequests = []
  const provider = await startOidcServer({ audience: 'security-browser', subject: 'security-owner', email: 'owner@example.test', name: 'Owner',
    validateClient(form, authorization) {
      tokenRequests.push({ basic: Boolean(authorization), grant: form.get('grant_type') })
      return !expectSecret || authorization === `Basic ${Buffer.from(`security-browser:${encoded(secret)}`).toString('base64')}`
    },
  })
  let application, browser, ownerPage
  const errors = []
  try {
    application = await initializeServer({ directory: temporary, origin, binary,
      oidc: { issuer: provider.issuer, audience: 'security-browser', client_id: 'security-browser' },
    })
    const ownerToken = application.owner.session.access_token
    const ownerUserId = application.owner.session.user.user_id
    const ownerCredentials = { username: application.owner.username, password: application.owner.password }
    await serverRequest(origin, '/auth/oidc-link', { token: ownerToken, body: { access_token: provider.accessToken() } })
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
    ownerPage = await browser.newPage({ locale: 'zh-CN', viewport: { width: 1440, height: 1200 }, serviceWorkers: 'block' })
    ownerPage.on('pageerror', error => errors.push(error.message))
    ownerPage.on('console', message => { if (message.type() === 'error') errors.push(message.text()) })
    await ownerPage.goto(`${origin}/admin/instance`)
    await ownerPage.getByLabel('用户名', { exact: true }).fill(application.owner.username)
    await ownerPage.getByLabel('密码', { exact: true }).fill(application.owner.password)
    await ownerPage.getByRole('button', { name: '登录', exact: true }).click()
    await ownerPage.getByLabel('用户名', { exact: true }).waitFor({ state: 'hidden' })
    await ownerPage.goto(`${origin}/admin/instance`)
    let panel = ownerPage.getByRole('region', { name: '登录与人机验证' })
    await panel.getByLabel('Client ID', { exact: true }).waitFor()
    await panel.getByRole('combobox', { name: 'Token 端点认证' }).click()
    await ownerPage.getByRole('option', { name: 'Client Secret · HTTP Basic', exact: true }).click()
    await panel.getByLabel('Client Secret', { exact: true }).fill(secret)
    const save = async () => {
      const response = ownerPage.waitForResponse(response => response.url().endsWith(`/api/v1${resource}`) && response.request().method() === 'PUT')
      await panel.getByRole('button', { name: '保存登录设置', exact: true }).click()
      const saved = await response
      assert.equal(saved.status(), 200, await saved.text())
      const body = await saved.json()
      assert.equal(body.oidc_providers[0].has_client_secret, true)
      assert.equal(body.oidc_providers[0].client_secret, undefined)
      assert.equal(JSON.stringify(body).includes(secret), false)
      return body
    }
    await save()
    expectSecret = true
    await ownerPage.reload()
    await panel.getByLabel('Client Secret', { exact: true }).waitFor()
    assert.equal(await panel.getByLabel('Client Secret', { exact: true }).inputValue(), '')
    await panel.getByRole('switch', { name: 'Cloudflare Turnstile' }).click()
    await panel.getByLabel('Site Key', { exact: true }).fill('1x00000000000000000000AA')
    await panel.getByLabel('Secret Key', { exact: true }).fill('private-browser-test-secret')
    const saved = await save()
    assert.equal(saved.turnstile.secret_key, undefined)
    assert.equal(saved.turnstile.has_secret_key, true)
    for (const width of [1440, 390, 320]) {
      await ownerPage.setViewportSize({ width, height: 1100 })
      await panel.scrollIntoViewIfNeeded()
      assert.ok(await ownerPage.evaluate(() => document.documentElement.scrollWidth <= innerWidth), `no horizontal page overflow at ${width}`)
      await ownerPage.screenshot({ path: path.join(artifacts, `server-security-${width}.png`) })
    }
    const anonymous = await browser.newPage({ locale: 'zh-CN', viewport: { width: 320, height: 800 }, isMobile: true, hasTouch: true, serviceWorkers: 'block' })
    anonymous.on('pageerror', error => errors.push(error.message))
    await anonymous.route('https://challenges.cloudflare.com/turnstile/v0/api.js*', route => route.fulfill({ contentType: 'application/javascript', body: `window.turnstile = {
      render(container, options) {
        const button = document.createElement('button'); button.type = 'button'; button.textContent = 'Test verification';
        button.style.minWidth = options.size === 'flexible' ? '300px' : '150px';
        button.style.width = options.size === 'flexible' ? '100%' : '150px';
        button.onclick = () => options.callback('browser-widget-token');
        container.append(button); window.testTurnstileOptions = options; (window.testTurnstileHistory ??= []).push(options); return 'test-widget-' + window.testTurnstileHistory.length;
      }, remove() { document.querySelectorAll('button').forEach(button => { if (button.textContent === 'Test verification') button.remove(); }); }
    };` }))
    await anonymous.goto(origin)
    await anonymous.getByLabel('用户名', { exact: true }).fill(application.owner.username)
    await anonymous.getByLabel('密码', { exact: true }).fill(application.owner.password)
    await anonymous.getByRole('button', { name: 'Test verification', exact: true }).waitFor()
    const login = anonymous.getByRole('button', { name: '登录', exact: true })
    assert.equal(await login.isDisabled(), true)
    assert.equal(await anonymous.evaluate(() => window.testTurnstileOptions.size), 'compact')
    await anonymous.getByRole('button', { name: 'Test verification', exact: true }).click()
    assert.equal(await login.isEnabled(), true)
    await anonymous.evaluate(() => window.testTurnstileOptions['expired-callback']())
    await anonymous.locator('button:disabled').filter({ hasText: /^登录$/ }).waitFor()
    assert.equal(await login.isDisabled(), true)
    await anonymous.getByRole('button', { name: '重新验证', exact: true }).click()
    await anonymous.getByRole('button', { name: 'Test verification', exact: true }).waitFor()
    await anonymous.evaluate(() => document.documentElement.classList.add('dark'))
    await anonymous.waitForFunction(() => window.testTurnstileOptions.theme === 'dark')
    await anonymous.setViewportSize({ width: 430, height: 900 })
    await anonymous.waitForFunction(() => window.testTurnstileOptions.size === 'flexible')
    await anonymous.setViewportSize({ width: 320, height: 800 })
    await anonymous.waitForFunction(() => window.testTurnstileOptions.size === 'compact')
    assert.ok((await anonymous.getByRole('button', { name: 'Test verification', exact: true }).boundingBox()).width < 300)
    await anonymous.setViewportSize({ width: 430, height: 900 })
    await anonymous.waitForFunction(() => window.testTurnstileOptions.size === 'flexible')
    assert.equal(await anonymous.getByRole('button', { name: 'Test verification', exact: true }).count(), 1)
    await anonymous.evaluate(() => {
      const current = window.testTurnstileOptions
      const retired = window.testTurnstileHistory[0]
      current.callback('fresh-widget-token')
      retired['error-callback']('600010')
      retired['expired-callback']()
      retired.callback('retired-token')
    })
    await anonymous.waitForFunction(() => [...document.querySelectorAll('button')].some(button => button.textContent === '登录' && !button.disabled))
    assert.equal(await login.isEnabled(), true, 'Retired compact widget callbacks cannot invalidate the current flexible widget')
    await anonymous.evaluate(() => window.testTurnstileOptions['error-callback']('600010'))
    await anonymous.getByRole('alert').filter({ hasText: '600010' }).waitFor()
    assert.equal(await login.isDisabled(), true)
    await anonymous.getByRole('button', { name: '重新验证', exact: true }).click()
    await anonymous.getByRole('button', { name: 'Test verification', exact: true }).waitFor()
    const bypass = await anonymous.request.post(`${origin}/api/v1/auth/login`, { data: { username: application.owner.username, password: application.owner.password } })
    assert.equal(bypass.status(), 403)
    await anonymous.screenshot({ path: path.join(artifacts, 'turnstile-login-mobile.png') })
    await panel.getByRole('switch', { name: 'Cloudflare Turnstile' }).click()
    await save()
    const configPath = application.configPath
    await ownerPage.close()
    await stopProcess(application)
    application = startProcess(binary, ['serve', '--config-dir', path.dirname(configPath)], cleanEnvironment)
    await waitForHttp(`${origin}/readyz`, application)
    await anonymous.reload()
    const [exchange] = await Promise.all([
      anonymous.waitForResponse(response => response.url() === `${origin}/auth/token`),
      anonymous.getByRole('button', { name: '使用 Organization 登录', exact: true }).click(),
    ])
    assert.equal(exchange.status(), 200, await exchange.text())
    const identity = await serverRequest(origin, '/auth/session', { token: (await exchange.json()).access_token })
    assert.equal(identity.user.user_id, ownerUserId)
    await anonymous.getByLabel('用户名', { exact: true }).waitFor({ state: 'hidden' })
    assert.ok(tokenRequests.some(request => request.basic && request.grant === 'authorization_code'), 'persisted client secret is used for the real PKCE exchange')
    ownerPage = await browser.newPage({ locale: 'zh-CN', viewport: { width: 1440, height: 1100 }, serviceWorkers: 'block' })
    ownerPage.on('pageerror', error => errors.push(error.message))
    ownerPage.on('console', message => { if (message.type() === 'error') errors.push(message.text()) })
    await ownerPage.goto(origin)
    await ownerPage.evaluate(token => sessionStorage.setItem('ternilo.native.session', JSON.stringify({ access_token: token, expires_at_ms: Date.now() + 3600000 })), ownerToken)
    await ownerPage.goto(`${origin}/admin/instance`)
    panel = ownerPage.getByRole('region', { name: '登录与人机验证' })
    const oldDocument = await browser.newPage({ locale: 'zh-CN', viewport: { width: 1280, height: 900 }, serviceWorkers: 'block' })
    oldDocument.on('pageerror', error => errors.push(error.message))
    await oldDocument.route('https://challenges.cloudflare.com/**', route => route.abort())
    const additionalLogin = await serverRequest(origin, '/auth/login', { body: ownerCredentials })
    await oldDocument.goto(origin)
    await oldDocument.evaluate(session => sessionStorage.setItem('ternilo.native.session', JSON.stringify(session)), additionalLogin)
    await oldDocument.reload()
    await oldDocument.getByRole('button', { name: '退出登录', exact: true }).waitFor()
    await panel.getByRole('switch', { name: 'Cloudflare Turnstile' }).click()
    await panel.getByLabel('Site Key', { exact: true }).fill('1x00000000000000000000AA')
    await panel.getByLabel('Secret Key', { exact: true }).fill('private-browser-test-secret')
    await save()
    assert.ok((await (await fetch(`${origin}/auth/config`)).json()).turnstile)
    const [refreshedDocument] = await Promise.all([
      oldDocument.waitForResponse(response => response.request().resourceType() === 'document'),
      oldDocument.getByRole('button', { name: '退出登录', exact: true }).click(),
    ])
    assert.match(refreshedDocument.headers()['content-security-policy'], /frame-src 'self' blob: https:\/\/challenges.cloudflare.com/)
    await oldDocument.getByLabel('用户名', { exact: true }).waitFor()
    await oldDocument.close()
    await execute(binary, ['admin', 'reset-authentication', '--config-dir', path.dirname(configPath)], { cwd: repository })
    const config = await (await fetch(`${origin}/auth/config`)).json()
    assert.equal(config.turnstile, undefined)
    const recovered = await serverRequest(origin, resource, { token: ownerToken })
    assert.equal(recovered.revision, 0)
    assert.equal(recovered.oidc_providers[0].has_client_secret, false)
    assert.equal((await serverRequest(origin, '/auth/login', { body: ownerCredentials })).user.user_id, ownerUserId)
    assert.deepEqual(errors, [])
  } catch (error) {
    if (ownerPage) await ownerPage.screenshot({ path: path.join(artifacts, 'server-security-failure.png') }).catch(() => {})
    throw new Error(`${error.stack}\n${application?.diagnostics() ?? ''}`)
  } finally {
    await browser?.close()
    await stopProcess(application)
    await provider.close()
    await rm(temporary, { recursive: true, force: true })
  }
})
