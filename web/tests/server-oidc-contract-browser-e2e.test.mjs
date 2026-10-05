import assert from 'node:assert/strict'
import { createHash, randomBytes } from 'node:crypto'
import { mkdir, mkdtemp, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { chromium } from 'playwright'
import { freePort, initializeServer, serverRequest, startOidcServer, stopProcess } from './platform-e2e-fixture.mjs'

test('Standard OIDC uses ID Token and UserInfo with opaque access tokens and stable account IDs', { timeout: 180000 }, async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-oidc-contract-'))
  const artifacts = process.env.TERNILO_E2E_ARTIFACT_DIR ?? directory
  await mkdir(artifacts, { recursive: true })
  let fault = ''
  let application, browser, ownerPage
  const errors = []
  const provider = await startOidcServer({
    audience: 'upstream-api-not-this-client', subject: 'linuxdo-style-stable-subject', email: 'external@example.test', name: 'External name', opaqueAccessTokens: true,
    validateClient: form => form.get('client_id') === 'browser-client' && form.get('client_secret') === 'fixture-client-secret',
    idTokenClaims(claims) {
      if (fault === 'missing-id-token') return null
      if (fault === 'signature') return claims
      const overrides = {
        nonce: { nonce: 'different-login' }, 'missing-nonce': { nonce: undefined },
        issuer: { iss: 'https://different-issuer.test/' }, audience: { aud: 'different-client' },
        party: { azp: 'different-client' }, 'multiple-audiences': { aud: ['browser-client', 'other-client'] },
        expired: { exp: 1 }, issued: { iat: Math.floor(Date.now() / 1000) + 3600 },
        hash: { at_hash: 'wrong-hash' }, subject: { sub: 'another-subject' },
      }
      return { ...claims, ...overrides[fault] }
    },
    transformIdToken(token) {
      if (fault !== 'signature') return token
      const parts = token.split('.')
      parts[2] = (parts[2][0] === 'A' ? 'B' : 'A') + parts[2].slice(1)
      return parts.join('.')
    },
    userInfoClaims: claims => fault === 'userinfo' ? { ...claims, sub: 'different-subject' } : fault === 'duplicate-email' ? { ...claims, email: 'browser-owner@example.test' } : claims,
  })
  try {
    const origin = `http://127.0.0.1:${await freePort()}`
    application = await initializeServer({ directory, origin, oidc: { issuer: provider.issuer, audience: '', client_id: 'browser-client' } })
    const ownerToken = application.owner.session.access_token
    const ownerId = application.owner.session.user.user_id
    const admin = (resource, options = {}) => serverRequest(origin, resource, { token: ownerToken, ...options })
    await admin('/admin/instance/authentication', { method: 'PUT', body: { revision: 0, public_url: origin,
      oidc_providers: [{ id: 'organization', name: 'Organization', enabled: true, ...{ issuer: provider.issuer, client_id: 'browser-client', scopes: 'openid profile email', token_auth_method: 'client_secret_post', client_secret: 'fixture-client-secret' } }], turnstile: null,
    } })
    await admin('/admin/registration', { method: 'PATCH', body: { mode: 'open', require_approval: false, oidc_only: false, revision: 1 } })

    const exchange = async () => {
      const verifier = randomBytes(48).toString('base64url')
      const nonce = randomBytes(24).toString('base64url')
      const query = new URLSearchParams({ response_type: 'code', client_id: 'browser-client', redirect_uri: `${origin}/auth/callback`, scope: 'openid profile email', state: 'fixture-state', nonce, code_challenge_method: 'S256', code_challenge: createHash('sha256').update(verifier).digest('base64url') })
      const redirect = await fetch(`${provider.issuer}/authorize?${query}`, { redirect: 'manual' })
      assert.equal(redirect.status, 302)
      const code = new URL(redirect.headers.get('location')).searchParams.get('code')
      return fetch(`${origin}/auth/token`, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ code, code_verifier: verifier, provider_id: 'organization', nonce }) })
    }
    for (const invalid of ['nonce', 'missing-nonce', 'issuer', 'audience', 'party', 'multiple-audiences', 'expired', 'issued', 'hash', 'signature', 'missing-id-token', 'userinfo']) {
      fault = invalid
      const response = await exchange()
      assert.equal(response.status, 401, `${invalid}: ${await response.text()}`)
      assert.equal(response.headers.get('cache-control'), 'no-store')
    }
    fault = ''
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
    const visitor = await browser.newPage({ locale: 'zh-CN', viewport: { width: 390, height: 850 }, hasTouch: true, isMobile: true, serviceWorkers: 'block' })
    visitor.on('pageerror', error => errors.push(error.message))
    fault = 'duplicate-email'
    await visitor.goto(origin)
    await visitor.getByRole('button', { name: '使用 Organization 登录', exact: true }).click()
    await visitor.locator('[data-oidc-registration]').waitFor()
    await visitor.getByLabel('用户名', { exact: true }).fill('duplicate-email-user')
    assert.equal(await visitor.getByLabel('邮箱', { exact: true }).count(), 0)
    const [duplicate] = await Promise.all([
      visitor.waitForResponse(response => response.url().endsWith('/auth/oidc/register')),
      visitor.getByRole('button', { name: '完成注册并继续', exact: true }).click(),
    ])
    assert.equal(duplicate.status(), 409)
    assert.match(await visitor.getByRole('alert').innerText(), /登录原账号/)
    fault = ''
    ownerPage = await browser.newPage({ locale: 'zh-CN', viewport: { width: 1280, height: 950 }, serviceWorkers: 'block' })
    ownerPage.on('pageerror', error => errors.push(error.message))
    ownerPage.on('console', message => { if (message.type() === 'error') errors.push(message.text()) })
    await ownerPage.goto(origin)
    await ownerPage.getByLabel('用户名', { exact: true }).fill(application.owner.username)
    await ownerPage.getByLabel('密码', { exact: true }).fill(application.owner.password)
    await ownerPage.getByRole('button', { name: '登录', exact: true }).click()
    await ownerPage.getByLabel('用户名', { exact: true }).waitFor({ state: 'hidden' })
    const nativeBefore = await ownerPage.evaluate(() => sessionStorage.getItem('ternilo.native.session'))
    await ownerPage.goto(`${origin}/admin/instance`)
    await ownerPage.getByRole('button', { name: '使用 LINUX DO 配置', exact: true }).click()
    assert.equal(await ownerPage.getByLabel('Issuer 地址', { exact: true }).inputValue(), 'https://connect.linux.do/')
    assert.equal(await ownerPage.getByLabel('Client ID', { exact: true }).inputValue(), '')
    assert.equal(await ownerPage.getByLabel('Client Secret', { exact: true }).inputValue(), '')
    await ownerPage.screenshot({ path: path.join(artifacts, 'linuxdo-settings-preset.png') })
    await ownerPage.goto(origin)
    await ownerPage.getByRole('button', { name: '用户设置', exact: true }).click()
    const [linked] = await Promise.all([
      ownerPage.waitForResponse(response => response.url().endsWith('/auth/oidc-link') && response.request().method() === 'POST'),
      ownerPage.getByRole('button', { name: '关联 Organization', exact: true }).click(),
    ])
    assert.equal(linked.status(), 200, await linked.text())
    await ownerPage.getByRole('button', { name: '用户设置', exact: true }).waitFor()
    assert.equal(await ownerPage.evaluate(() => sessionStorage.getItem('ternilo.native.session')), nativeBefore)
    await visitor.getByRole('button', { name: '使用其他账号', exact: true }).click()
    const [login] = await Promise.all([
      visitor.waitForResponse(response => response.url() === `${origin}/auth/token`),
      visitor.getByRole('button', { name: '使用 Organization 登录', exact: true }).click(),
    ])
    assert.equal(login.status(), 200, await login.text())
    const tokens = await login.json()
    assert.match(tokens.access_token, /^ter_o_/)
    assert.match(tokens.refresh_token, /^ter_r_/)
    assert.equal(JSON.stringify(tokens).includes('opaque-access-'), false)
    const identity = await serverRequest(origin, '/auth/session', { token: tokens.access_token })
    assert.equal(identity.user.user_id, ownerId)
    assert.equal(identity.email, application.owner.email)
    await visitor.close()
    fault = 'missing-id-token'
    const refresh = await fetch(`${origin}/auth/refresh`, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ refresh_token: tokens.refresh_token, provider_id: 'organization' }) })
    assert.equal(refresh.status, 200, await refresh.clone().text())
    const next = await refresh.json()
    assert.notEqual(next.access_token, tokens.access_token)
    assert.equal((await serverRequest(origin, '/auth/session', { token: next.access_token })).user.user_id, ownerId)
    const replay = await fetch(`${origin}/auth/refresh`, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ refresh_token: tokens.refresh_token, provider_id: 'organization' }) })
    assert.equal(replay.status, 401)
    await serverRequest(origin, '/auth/logout', { token: next.access_token, body: {} })
    assert.equal((await fetch(`${origin}/api/v1/auth/session`, { headers: { authorization: `Bearer ${next.access_token}` } })).status, 401)
    fault = ''
    const beforeSwap = await (await exchange()).json()
    fault = 'subject'
    const swapped = await fetch(`${origin}/auth/refresh`, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ refresh_token: beforeSwap.refresh_token, provider_id: 'organization' }) })
    assert.equal(swapped.status, 401)
    assert.deepEqual(errors, [])
  } catch (error) {
    await ownerPage?.screenshot({ path: path.join(artifacts, 'oidc-contract-failure.png') }).catch(() => {})
    throw new Error(`${error.stack}\n${application?.diagnostics() ?? ''}`)
  } finally {
    await browser?.close()
    await stopProcess(application)
    await provider.close()
    await rm(directory, { recursive: true, force: true })
  }
})


test('OIDC uses immutable provider email and automatically registers usable upstream usernames with explicit fallbacks', { timeout: 120000 }, async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-oidc-profile-'))
  const artifacts = process.env.TERNILO_E2E_ARTIFACT_DIR ?? directory
  await mkdir(artifacts, { recursive: true })
  const usernames = { supplied: 'upstream-user', collision: 'browser-owner', invalid: 'not a valid username', missing: 'email-missing-user', review: 'reviewed-upstream-user' }
  const provider = await startOidcServer({
    identities: {
      supplied: { subject: 'supplied', email: 'userinfo@example.test' },
      collision: { subject: 'collision', email: 'collision@example.test' },
      invalid: { subject: 'invalid', email: 'invalid-name@example.test' },
      missing: { subject: 'missing' },
      review: { subject: 'review', email: 'review@example.test' },
    },
    initialIdentity: 'supplied', opaqueAccessTokens: true,
    idTokenClaims: claims => ({ ...claims, email: undefined }),
    userInfoClaims: claims => ({ ...claims, preferred_username: usernames[claims.sub] }),
  })
  let application, browser
  const errors = []
  try {
    const origin = `http://127.0.0.1:${await freePort()}`
    application = await initializeServer({ directory, origin, oidc: { issuer: provider.issuer, audience: '', client_id: 'profile-client' } })
    const ownerToken = application.owner.session.access_token
    await serverRequest(origin, '/admin/registration', { token: ownerToken, method: 'PATCH', body: { mode: 'open', require_approval: false, oidc_only: true, revision: 1 } })
    browser = await chromium.launch({ headless: true })
    const page = await browser.newPage({ locale: 'zh-CN', viewport: { width: 390, height: 850 }, serviceWorkers: 'block' })
    page.on('pageerror', error => errors.push(error.message))
    page.on('response', response => { if (response.status() >= 500) errors.push(`${response.status()} ${new URL(response.url()).pathname}`) })
    const signIn = async identity => {
      provider.selectIdentity(identity)
      await page.goto(origin)
      const exchange = page.waitForResponse(response => response.url().endsWith('/auth/token'))
      await page.getByRole('button', { name: '使用 Organization 登录', exact: true }).click()
      return (await exchange).json()
    }
    const identity = async () => serverRequest(origin, '/auth/session', { token: await page.evaluate(() => sessionStorage.getItem('ternilo.oidc.access')) })
    const logout = async () => {
      await page.getByRole('button', { name: '打开侧边栏', exact: true }).click()
      await page.getByRole('button', { name: '退出登录', exact: true }).click()
      await page.getByRole('button', { name: '使用 Organization 登录', exact: true }).waitFor()
      assert.equal(await page.evaluate(() => sessionStorage.getItem('ternilo.oidc.email')), null)
      assert.equal(await page.evaluate(() => sessionStorage.getItem('ternilo.oidc.username')), null)
    }
    const supplied = await signIn('supplied')
    assert.equal(supplied.email, 'userinfo@example.test')
    assert.equal(supplied.username, 'upstream-user')
    await page.getByRole('button', { name: '退出登录', exact: true }).waitFor()
    assert.equal((await identity()).user.username, 'upstream-user')
    assert.equal((await identity()).email, 'userinfo@example.test')
    await page.reload()
    await page.getByRole('button', { name: '退出登录', exact: true }).waitFor()
    assert.equal(await page.locator('[data-oidc-registration]').count(), 0)
    await logout()

    await signIn('collision')
    await page.getByRole('alert').filter({ hasText: '这个用户名已被使用' }).waitFor()
    assert.equal(await page.getByLabel('邮箱', { exact: true }).count(), 0)
    assert.match(await page.locator('[data-oidc-email]').innerText(), /collision@example.test/)
    await page.getByLabel('用户名', { exact: true }).fill('chosen-after-collision')
    await page.route('**/api/v1/auth/oidc/register', route => route.continue({ postData: JSON.stringify({ ...route.request().postDataJSON(), email: 'forged-contact@example.test' }) }))
    await page.getByRole('button', { name: '完成注册并继续', exact: true }).click()
    await page.getByRole('button', { name: '退出登录', exact: true }).waitFor()
    assert.equal((await identity()).email, 'collision@example.test', 'Client request edits cannot replace the authenticated provider email')
    await page.unroute('**/api/v1/auth/oidc/register')
    await logout()

    await signIn('invalid')
    await page.locator('[data-oidc-registration]').waitFor()
    assert.equal(await page.getByLabel('用户名', { exact: true }).inputValue(), '')
    assert.equal(await page.getByLabel('邮箱', { exact: true }).count(), 0)
    await page.screenshot({ path: path.join(artifacts, 'oidc-username-fallback-mobile.png') })
    await page.getByLabel('用户名', { exact: true }).fill('chosen-valid-user')
    await page.getByRole('button', { name: '完成注册并继续', exact: true }).click()
    await page.getByRole('button', { name: '退出登录', exact: true }).waitFor()
    assert.equal((await identity()).email, 'invalid-name@example.test')
    await logout()

    await signIn('missing')
    await page.locator('[data-oidc-registration]').waitFor()
    await page.locator('[data-oidc-username]').filter({ hasText: 'email-missing-user' }).waitFor()
    assert.equal(await page.getByLabel('邮箱', { exact: true }).inputValue(), '')
    assert.equal(await page.getByRole('button', { name: '完成注册并继续', exact: true }).isEnabled(), false)
    await page.getByLabel('邮箱', { exact: true }).fill('manual-contact@example.test')
    await page.getByRole('button', { name: '完成注册并继续', exact: true }).click()
    await page.getByRole('button', { name: '退出登录', exact: true }).waitFor()
    assert.equal((await identity()).email, 'manual-contact@example.test')
    await logout()

    const policy = await serverRequest(origin, '/admin/registration', { token: ownerToken })
    await serverRequest(origin, '/admin/registration', { token: ownerToken, method: 'PATCH', body: { ...policy, require_approval: true } })
    await signIn('review')
    await page.locator('[data-registration-pending]').waitFor()
    assert.match(await page.locator('[data-registration-pending]').innerText(), /reviewed-upstream-user/)
    const accounts = await serverRequest(origin, '/admin/accounts?limit=50', { token: ownerToken })
    assert.equal(accounts.accounts.find(account => account.username === 'reviewed-upstream-user').status, 'pending')
    assert.deepEqual(errors, [])
  } finally {
    await browser?.close()
    await stopProcess(application)
    await provider.close()
    await rm(directory, { recursive: true, force: true })
  }
})
