import assert from 'node:assert/strict'
import { execFile, spawn } from 'node:child_process'
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import { promisify } from 'node:util'
import test from 'node:test'
import { chromium } from 'playwright'
import { freePort, repository, startOidcServer, stopProcess, waitForHttp } from './platform-e2e-fixture.mjs'

const execute = promisify(execFile)
const binary = process.env.TERNILO_E2E_SERVER_BINARY ?? path.join(repository, 'target/debug/ternilo-server')
const cleanEnvironment = Object.fromEntries(Object.entries(process.env).filter(([key]) => !key.startsWith('TERNILO_')))
const nativeKey = 'ternilo.native.session'

async function requestIdentity(page, origin, token) {
  const response = await page.request.get(`${origin}/api/v1/auth/session`, {
    headers: { authorization: `Bearer ${token}` },
  })
  assert.equal(response.status(), 200)
  assert.equal(response.headers()['cache-control'], 'no-store')
  return response.json()
}

async function openAccountSettings(page) {
  await page.getByRole('button', { name: '用户设置', exact: true }).click()
  const dialog = page.locator('[data-user-settings]')
  await dialog.getByRole('button', { name: '通用', exact: true }).click()
  return dialog
}

test('A native owner explicitly links OIDC through PKCE and keeps the same account', { timeout: 120_000 }, async () => {
  const temporary = await mkdtemp(path.join(tmpdir(), 'ternilo-server-oidc-link-browser-'))
  const artifacts = process.env.TERNILO_E2E_ARTIFACT_DIR ?? temporary
  await mkdir(artifacts, { recursive: true })
  const origin = `http://127.0.0.1:${await freePort()}`
  const configPath = path.join(temporary, 'server', 'server.json')
  const provider = await startOidcServer({
    audience: 'ternilo-link-browser',
    subject: 'explicit-native-owner',
    email: 'owner@example.test',
    name: 'Linked owner',
  })
  let application, browser, page
  const pageErrors = [], consoleErrors = [], failedApiRequests = []
  try {
    await execute(binary, [
      'init', '--non-interactive', '--config', configPath,
      '--listen', new URL(origin).host, '--public-url', origin,
    ], {
      cwd: repository,
      env: { ...cleanEnvironment, TERNILO_SERVER_OWNER_USERNAME: 'owner', TERNILO_SERVER_OWNER_EMAIL: 'owner@example.test', TERNILO_SERVER_OWNER_PASSWORD: 'browser-owner-password' },
    })
    const config = JSON.parse(await readFile(configPath, 'utf8'))
    assert.match(config.database_url, /^sqlite:/)
    config.oidc = {
      issuer: provider.issuer,
      audience: 'ternilo-link-browser',
      client_id: 'ternilo-link-browser',
      scopes: 'openid profile email',
      allow_insecure: true,
    }
    await writeFile(configPath, `${JSON.stringify(config, null, 2)}\n`, { mode: 0o600 })
    const child = spawn(binary, ['serve', '--config', configPath], {
      cwd: repository, env: cleanEnvironment, stdio: ['ignore', 'pipe', 'pipe'],
    })
    let output = ''
    child.stdout.on('data', chunk => { output += chunk })
    child.stderr.on('data', chunk => { output += chunk })
    application = { child, diagnostics: () => output }
    await waitForHttp(`${origin}/auth/config`, application)
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
    page = await browser.newPage({ viewport: { width: 1280, height: 900 }, serviceWorkers: 'block' })
    page.on('pageerror', error => pageErrors.push(error.message))
    page.on('console', message => { if (message.type() === 'error') consoleErrors.push(message.text()) })
    page.on('response', response => {
      const url = new URL(response.url())
      if (url.origin === origin && url.pathname.startsWith('/api/v1/') && response.status() >= 400) {
        failedApiRequests.push({ path: url.pathname, status: response.status() })
      }
    })
    await page.goto(origin)
    await page.getByLabel('用户名', { exact: true }).fill('owner')
    await page.getByLabel('密码', { exact: true }).fill('browser-owner-password')
    const [loggedIn] = await Promise.all([
      page.waitForResponse(response => new URL(response.url()).pathname === '/api/v1/auth/login'),
      page.getByRole('button', { name: '登录', exact: true }).click(),
    ])
    assert.equal(loggedIn.status(), 200)
    const original = await loggedIn.json()
    await page.getByRole('dialog').waitFor({ state: 'hidden' })
    const originalNative = await page.evaluate(key => JSON.parse(sessionStorage.getItem(key)), nativeKey)
    assert.equal(originalNative.access_token, original.access_token)
    assert.equal(original.instance.mode, 'single_user')
    assert.equal(original.is_instance_owner, true)

    const prematureOidcSessions = []
    let linking = true
    page.on('request', request => {
      if (linking && new URL(request.url()).pathname === '/api/v1/auth/session') {
        const bearer = request.headers().authorization ?? ''
        if (bearer && !bearer.startsWith('Bearer kns_')) prematureOidcSessions.push('OIDC login preceded explicit account binding')
      }
    })
    const settings = await openAccountSettings(page)
    const [authorization, linked] = await Promise.all([
      page.waitForRequest(request => request.url().startsWith(`${provider.issuer}/authorize?`)),
      page.waitForResponse(response => new URL(response.url()).pathname === '/api/v1/auth/oidc-link' && response.request().method() === 'POST'),
      settings.getByRole('button', { name: '绑定 OIDC 登录', exact: true }).click(),
    ])
    const authorizationUrl = new URL(authorization.url())
    assert.equal(authorizationUrl.searchParams.get('code_challenge_method'), 'S256')
    assert.ok(authorizationUrl.searchParams.get('code_challenge'))
    assert.equal(authorizationUrl.searchParams.get('redirect_uri'), `${origin}/auth/callback`)
    assert.equal(linked.status(), 200)
    assert.equal(linked.request().headers().authorization, `Bearer ${originalNative.access_token}`)
    const link = await linked.json()
    assert.deepEqual(link, { native: true, oidc: { issuer: provider.issuer, subject: 'explicit-native-owner' } })
    await page.waitForURL(url => url.origin === origin && url.pathname === '/' && !url.search && !url.hash)
    await page.getByRole('button', { name: '用户设置', exact: true }).waitFor()
    linking = false
    assert.deepEqual(prematureOidcSessions, [])
    assert.deepEqual(await page.evaluate(key => JSON.parse(sessionStorage.getItem(key)), nativeKey), originalNative)
    assert.equal(await page.evaluate(() => sessionStorage.getItem('ternilo.oidc.access')), null, 'binding does not replace the active native session with OIDC')
    const linkedNative = await requestIdentity(page, origin, originalNative.access_token)
    assert.equal(linkedNative.user.user_id, original.user.user_id)
    assert.equal(linkedNative.personal_tenant_id, original.personal_tenant_id)
    assert.equal(linkedNative.personal_project_id, original.personal_project_id)

    const linkedSettings = await openAccountSettings(page)
    await linkedSettings.getByText('OIDC 登录已绑定', { exact: true }).waitFor()
    await linkedSettings.getByText(provider.issuer, { exact: true }).waitFor()
    assert.equal(await linkedSettings.getByRole('button', { name: '绑定 OIDC 登录', exact: true }).count(), 0)
    await linkedSettings.locator('[data-account-settings]').scrollIntoViewIfNeeded()
    await page.screenshot({ path: path.join(artifacts, 'server-oidc-linked-owner.png'), fullPage: true })
    await page.getByRole('button', { name: '返回工作台', exact: true }).click()
    await page.locator('[data-user-settings]').waitFor({ state: 'hidden' })
    await page.getByRole('button', { name: '退出登录', exact: true }).click()
    await page.getByLabel('用户名', { exact: true }).waitFor()
    assert.equal(await page.evaluate(key => sessionStorage.getItem(key), nativeKey), null)
    await page.getByRole('button', { name: '使用组织账号登录', exact: true }).click()
    await page.waitForURL(url => url.origin === origin && url.pathname === '/' && !url.search && !url.hash)
    await page.getByRole('button', { name: '用户设置', exact: true }).waitFor()
    const oidcToken = await page.evaluate(() => sessionStorage.getItem('ternilo.oidc.access'))
    assert.ok(oidcToken)
    assert.equal(await page.evaluate(key => sessionStorage.getItem(key), nativeKey), null)
    const oidcIdentity = await requestIdentity(page, origin, oidcToken)
    assert.equal(oidcIdentity.user.user_id, original.user.user_id)
    assert.equal(oidcIdentity.user.username, original.user.username)
    assert.deepEqual(Object.keys(oidcIdentity.user).sort(), ['user_id', 'username'])
    assert.equal(oidcIdentity.is_instance_owner, true)
    assert.equal(oidcIdentity.instance.mode, 'single_user')
    assert.equal(oidcIdentity.personal_tenant_id, original.personal_tenant_id)
    assert.equal(oidcIdentity.personal_project_id, original.personal_project_id)
    await page.reload()
    await page.getByRole('button', { name: '用户设置', exact: true }).waitFor()
    assert.equal((await requestIdentity(page, origin, oidcToken)).user.user_id, original.user.user_id)
    assert.deepEqual(failedApiRequests, [])
    assert.deepEqual(pageErrors, [])
    assert.deepEqual(consoleErrors, [])
    assert.equal(application.diagnostics().includes('panicked'), false)
  } catch (error) {
    await page?.screenshot({ path: path.join(artifacts, 'server-oidc-link-failure.png'), fullPage: true }).catch(() => {})
    throw error
  } finally {
    await browser?.close()
    await stopProcess(application)
    await provider.close()
    await rm(temporary, { recursive: true, force: true })
  }
})
