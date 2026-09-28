import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { chromium } from 'playwright'
import { freePort, initializeServer, repository, serverRequest, startOidcServer, stopProcess } from './platform-e2e-fixture.mjs'

async function organizationLogin(page, origin) {
  await page.goto(origin)
  await page.getByRole('button', { name: '使用组织账号登录', exact: true }).click()
}

async function existingOrganizationLogin(page, origin) {
  const authenticated = page.waitForResponse(response => new URL(response.url()).pathname === '/api/v1/auth/session' && response.status() === 200)
  await organizationLogin(page, origin)
  await authenticated
  await page.getByRole('dialog').waitFor({ state: 'hidden' })
}

async function chooseUsername(page, username, review = false) {
  const form = page.locator('[data-oidc-registration]')
  await form.waitFor()
  assert.equal(await page.locator('#server-password, #server-display-name, #server-account-token').count(), 0)
  await form.getByLabel('用户名', { exact: true }).fill(username)
  await form.getByLabel('邮箱', { exact: true }).fill(`${username}-oidc@example.test`)
  const response = page.waitForResponse(response => new URL(response.url()).pathname === '/api/v1/auth/oidc/register')
  await form.getByRole('button', { name: review ? '提交注册申请' : '完成注册并继续', exact: true }).click()
  return response
}

async function currentIdentity(page, origin) {
  const token = await page.evaluate(() => sessionStorage.getItem('ternilo.oidc.access'))
  assert.ok(token)
  assert.equal(await page.evaluate(() => sessionStorage.getItem('ternilo.native.session')), null)
  return serverRequest(origin, '/auth/session', { token })
}

async function noOverflow(page) {
  assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true)
}

test('platform usernames remain canonical through native setup, OIDC registration, approval and invitation-only access', { timeout: 120_000 }, async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-username-browser-'))
  const artifacts = process.env.TERNILO_E2E_ARTIFACT_DIR ?? directory
  await mkdir(artifacts, { recursive: true })
  const oidc = await startOidcServer({ audience: 'username-browser', identities: {
    first: { subject: 'external-subject-first', email: 'provider-first@example.test', name: 'External display name' },
    pending: { subject: 'external-subject-pending', email: 'provider-pending@example.test', name: 'External pending name' },
    invitedOnly: { subject: 'external-subject-denied', email: 'provider-denied@example.test', name: 'External denied name' },
  }, initialIdentity: 'first' })
  let application, browser, page
  const errors = [], expectedFailures = [], handledFailures = [], assetHashes = {}
  const expectedConsoleLocations = new Set()
  const expectFailure = (method, status, pathname, origin) => {
    expectedFailures.push(`${method} ${status} ${pathname}`)
    expectedConsoleLocations.add(`${origin}${pathname}`)
  }
  try {
    const origin = `http://127.0.0.1:${await freePort()}`
    application = await initializeServer({ directory: path.join(directory, 'server'), origin,
      oidc: { issuer: oidc.issuer, audience: 'username-browser', client_id: 'username-browser' } })
    for (const asset of ['app.js', 'app.css']) {
      const served = createHash('sha256').update(Buffer.from(await (await fetch(`${origin}/assets/${asset}`)).arrayBuffer())).digest('hex')
      const expected = createHash('sha256').update(await readFile(path.join(repository, 'web/dist/assets', asset))).digest('hex')
      assert.equal(served, expected, `Server embeds current ${asset}`)
      assetHashes[asset] = served
    }
    assert.deepEqual(application.owner.session.user, { user_id: application.owner.session.user.user_id, username: application.owner.username })
    const token = application.owner.session.access_token
    const ownerRequest = (resource, options = {}) => serverRequest(origin, resource, { token, ...options })
    const setPolicy = async (mode, require_approval = false) => {
      const policy = await ownerRequest('/admin/registration')
      await ownerRequest('/admin/registration', { method: 'PATCH', body: { mode, require_approval, revision: policy.revision } })
    }
    await setPolicy('open')
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
    const context = await browser.newContext({ viewport: { width: 1440, height: 960 }, permissions: ['clipboard-read', 'clipboard-write'], serviceWorkers: 'block' })
    page = await context.newPage()
    page.on('pageerror', error => errors.push(`page: ${error.message}`))
    page.on('console', message => {
      if (message.type() !== 'error') return
      if (/status of (403|409)/.test(message.text()) && expectedConsoleLocations.has(message.location().url)) return
      errors.push(`console: ${message.location().url} ${message.text()}`)
    })
    page.on('response', response => {
      if (response.status() < 400) return
      const key = `${response.request().method()} ${response.status()} ${new URL(response.url()).pathname}`
      const index = expectedFailures.indexOf(key)
      if (index >= 0) { expectedFailures.splice(index, 1); handledFailures.push(key) }
      else errors.push(`HTTP ${key}`)
    })
    page.on('requestfailed', request => { if (!request.failure()?.errorText.includes('ERR_ABORTED')) errors.push(`request: ${request.url()} ${request.failure()?.errorText}`) })

    expectFailure('GET', 403, '/api/v1/auth/session', origin)
    await organizationLogin(page, origin)
    await page.getByRole('dialog', { name: '完善账号信息', exact: true }).waitFor()
    assert.equal(await page.evaluate(() => sessionStorage.getItem('ternilo.native.session')), null)
    expectFailure('POST', 409, '/api/v1/auth/oidc/register', origin)
    assert.equal((await chooseUsername(page, application.owner.username)).status(), 409)
    await page.getByRole('alert').filter({ hasText: '这个用户名已被使用' }).waitFor()
    assert.equal(await page.getByLabel('用户名', { exact: true }).inputValue(), application.owner.username)
    await page.setViewportSize({ width: 390, height: 844 })
    await noOverflow(page)
    await page.screenshot({ path: path.join(artifacts, 'username-conflict-mobile.png'), animations: 'disabled' })
    const activeResponse = await chooseUsername(page, 'Chosen-User')
    assert.equal(activeResponse.status(), 201)
    const active = await activeResponse.json()
    assert.equal(active.status, 'active')
    assert.equal(active.session, undefined, 'OIDC registration does not issue a native session')
    await page.getByRole('dialog').waitFor({ state: 'hidden' })
    const identity = await currentIdentity(page, origin)
    assert.deepEqual(identity.user, { user_id: active.user_id, username: 'chosen-user' })
    await page.goto(`${origin}/settings/general`)
    const account = page.locator('[data-account-settings]')
    await account.getByText('chosen-user', { exact: true }).waitFor()
    assert.equal(await account.locator('[data-account-id]').textContent(), active.user_id)
    assert.equal((await account.textContent()).includes('External display name'), false)
    assert.equal((await account.textContent()).includes('provider-first@example.test'), false)
    await account.getByRole('button', { name: '复制账号 ID', exact: true }).click()
    assert.equal(await page.evaluate(() => navigator.clipboard.readText()), active.user_id)
    await noOverflow(page)
    await page.screenshot({ path: path.join(artifacts, 'username-account-mobile.png'), animations: 'disabled' })
    await page.reload()
    await account.getByText('chosen-user', { exact: true }).waitFor()
    assert.equal((await currentIdentity(page, origin)).user.user_id, active.user_id)
    assert.equal(await page.locator('[data-oidc-registration]').count(), 0)

    await setPolicy('open', true)
    oidc.selectIdentity('pending')
    await page.evaluate(() => sessionStorage.clear())
    expectFailure('GET', 403, '/api/v1/auth/session', origin)
    await organizationLogin(page, origin)
    const pendingResponse = await chooseUsername(page, 'pending-user', true)
    assert.equal(pendingResponse.status(), 201)
    const pending = await pendingResponse.json()
    assert.equal(pending.status, 'pending')
    await page.locator('[data-registration-pending]').waitFor()
    await page.getByText('管理员审核通过后，使用同一个组织账号登录即可。', { exact: true }).waitFor()
    assert.equal(await page.evaluate(() => sessionStorage.getItem('ternilo.native.session')), null)
    assert.equal(await page.evaluate(() => sessionStorage.getItem('ternilo.oidc.access')), null)
    assert.equal(await page.evaluate(() => sessionStorage.getItem('ternilo.oidc.refresh')), null)
    await noOverflow(page)
    await page.screenshot({ path: path.join(artifacts, 'username-pending-mobile.png'), animations: 'disabled' })
    const pendingAccount = (await ownerRequest(`/admin/accounts?query=${pending.user_id}`)).accounts[0]
    assert.equal(pendingAccount.username, 'pending-user')
    await ownerRequest(`/admin/accounts/${pending.user_id}/review`, { body: { decision: 'approve', status_revision: pendingAccount.status_revision } })
    await existingOrganizationLogin(page, origin)
    assert.deepEqual((await currentIdentity(page, origin)).user, { user_id: pending.user_id, username: 'pending-user' })
    assert.equal(await page.locator('[data-oidc-registration]').count(), 0)

    await setPolicy('invite')
    oidc.selectIdentity('invitedOnly')
    await page.evaluate(() => sessionStorage.clear())
    expectFailure('GET', 403, '/api/v1/auth/session', origin)
    await organizationLogin(page, origin)
    await page.getByRole('alert').filter({ hasText: '当前平台仅接受管理员邀请' }).waitFor()
    assert.equal(await page.locator('[data-oidc-registration]').count(), 0)
    assert.equal(await page.getByRole('button', { name: '注册新账号', exact: true }).count(), 0)
    assert.equal(await page.evaluate(() => sessionStorage.getItem('ternilo.oidc.access')), null)
    oidc.selectIdentity('first')
    await existingOrganizationLogin(page, origin)
    assert.deepEqual((await currentIdentity(page, origin)).user, identity.user)
    assert.deepEqual(expectedFailures, [])
    assert.deepEqual(errors, [])
  } catch (error) {
    await page?.screenshot({ path: path.join(artifacts, 'username-failure.png'), fullPage: true }).catch(() => {})
    process.stderr.write(`${application?.diagnostics() ?? ''}\n`)
    throw error
  } finally {
    await writeFile(path.join(artifacts, 'username-observations.json'), JSON.stringify({ assetHashes, errors, handledFailures, expectedFailures }, null, 2))
    await browser?.close()
    await stopProcess(application)
    await oidc.close()
    await rm(directory, { recursive: true, force: true })
  }
})
