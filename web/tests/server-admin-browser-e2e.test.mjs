import { selectChoice } from './browser-select-fixture.mjs'
import assert from 'node:assert/strict'
import path from 'node:path'
import { mkdir, mkdtemp, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import test from 'node:test'
import { chromium } from 'playwright'
import { freePort, initializeServer, serverRequest, startOidcServer, stopProcess } from './platform-e2e-fixture.mjs'

async function loginOwner(page, origin, owner) {
  await page.goto(`${origin}/admin/accounts`)
  await page.getByLabel('用户名', { exact: true }).fill(owner.username)
  await page.getByLabel('密码', { exact: true }).fill(owner.password)
  await page.getByRole('button', { name: '登录', exact: true }).click()
  await page.locator('[data-admin-account]').first().waitFor()
}

async function searchAccount(page, name) {
  const accounts = page.locator('[data-admin-accounts]')
  await accounts.getByLabel('搜索账号', { exact: true }).fill(name)
  const response = page.waitForResponse(response => new URL(response.url()).pathname === '/api/v1/admin/accounts'
    && new URL(response.url()).searchParams.get('query') === name)
  await accounts.getByRole('button', { name: '搜索', exact: true }).click()
  assert.equal((await response).status(), 200)
  await accounts.locator('[data-admin-account]').first().waitFor()
}

async function appoint(page, id, role) {
  const account = page.locator(`[data-admin-account="${id}"]`)
  await selectChoice(account.getByRole('combobox'), role)
  await account.getByRole('button', { name: '保存职责', exact: true }).click()
  const confirmation = page.getByRole('dialog', { name: '更新平台职责？', exact: true })
  const updated = page.waitForResponse(response => response.request().method() === 'PATCH'
    && new URL(response.url()).pathname === `/api/v1/admin/accounts/${id}/role`)
  await confirmation.getByRole('button', { name: '保存职责', exact: true }).click()
  assert.equal((await updated).status(), 200)
  await confirmation.waitFor({ state: 'hidden' })
  await page.waitForFunction(({ id, role }) => document.querySelector(`[data-admin-account="${id}"] select`)?.value === role, { id, role })
}

async function noOverflow(page, root) {
  assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth))
  assert.ok(await root.evaluate(element => element.scrollWidth <= element.clientWidth))
}

test('platform account administration separates personal spaces, team invitations, and delegated roles', { timeout: 180_000 }, async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-admin-browser-'))
  const artifacts = process.env.TERNILO_E2E_ARTIFACT_DIR ?? directory
  await mkdir(artifacts, { recursive: true })
  const audience = 'ternilo-admin-browser'
  const identities = Object.fromEntries(Array.from({ length: 28 }, (_, index) => {
    const suffix = String(index).padStart(2, '0')
    return [suffix, { subject: `b1-account-${suffix}`, email: `b1-${suffix}@example.test`, name: `B1 Account ${suffix}` }]
  }))
  const oidc = await startOidcServer({ audience, identities, initialIdentity: '00' })
  let application, browser, ownerPage, memberPage
  const errors = []
  try {
    const origin = `http://127.0.0.1:${await freePort()}`
    application = await initializeServer({ directory: path.join(directory, 'server'), origin,
      oidc: { issuer: oidc.issuer, audience, client_id: 'browser' }, mode: 'multi_user' })
    const ownerToken = application.owner.session.access_token
    const registrationPolicy = await serverRequest(origin, '/admin/registration', { token: ownerToken })
    await serverRequest(origin, '/admin/registration', { token: ownerToken, method: 'PATCH', body: { mode: 'open', require_approval: false, oidc_only: false, revision: registrationPolicy.revision } })
    const accounts = new Map()
    for (const key of Object.keys(identities)) {
      const identity = await registerOidcUser(origin, oidc.accessToken(key), `b1-account-${key}`)
      assert.equal(identity.platform_role, 'user')
      assert.notEqual(identity.personal_tenant_id, application.owner.session.personal_tenant_id)
      accounts.set(key, identity)
    }
    assert.equal(new Set([...accounts.values()].map(account => account.personal_tenant_id)).size, 28)
    browser = await chromium.launch({ headless: true })
    const ownerContext = await browser.newContext({ locale: 'zh-CN', viewport: { width: 1440, height: 960 } })
    const memberContext = await browser.newContext({ locale: 'zh-CN', viewport: { width: 1280, height: 900 } })
    ownerPage = await ownerContext.newPage()
    memberPage = await memberContext.newPage()
    for (const page of [ownerPage, memberPage]) {
      page.on('pageerror', error => errors.push(`page: ${error.message}`))
      page.on('console', message => { if (message.type() === 'error') errors.push(`console: ${message.text()}`) })
      page.on('response', response => { if (response.status() >= 400) errors.push(`HTTP ${response.status()}: ${response.url()}`) })
    }
    await loginOwner(ownerPage, origin, application.owner)
    const firstPage = await ownerPage.locator('[data-admin-account]').evaluateAll(rows => rows.map(row => row.dataset.adminAccount))
    assert.equal(firstPage.length, 25)
    const next = ownerPage.waitForResponse(response => new URL(response.url()).pathname === '/api/v1/admin/accounts'
      && new URL(response.url()).searchParams.has('cursor'))
    await ownerPage.getByRole('button', { name: '下一页', exact: true }).click()
    const nextPage = await (await next).json()
    assert.equal(nextPage.accounts.length, 4)
    assert.equal(nextPage.next_cursor, null)
    assert.equal(new Set([...firstPage, ...nextPage.accounts.map(account => account.user_id)]).size, 29)
    await ownerPage.getByRole('button', { name: '上一页', exact: true }).click()
    await searchAccount(ownerPage, 'b1-account-00')
    const member = accounts.get('00')
    await appoint(ownerPage, member.user.user_id, 'admin')
    assert.equal((await serverRequest(origin, '/auth/session', { token: oidc.accessToken('00') })).platform_role, 'admin')
    await ownerPage.screenshot({ path: path.join(artifacts, 'admin-accounts-desktop.png'), animations: 'disabled' })

    oidc.selectIdentity('00')
    await memberPage.goto(`${origin}/admin/accounts`)
    await memberPage.getByRole('button', { name: '使用 Organization 登录', exact: true }).click()
    await memberPage.locator('[data-admin-account]').first().waitFor()
    assert.equal(await memberPage.getByRole('button', { name: '保存职责', exact: true }).count(), 0)
    assert.equal(await memberPage.locator('[data-admin-invitations="platform"]').count(), 1)
    const privateResponse = await fetch(`${origin}/api/v1/tenants/${application.owner.session.personal_tenant_id}/members`, {
      headers: { authorization: `Bearer ${oidc.accessToken('00')}` },
    })
    assert.equal(privateResponse.status, 403)
    await appoint(ownerPage, member.user.user_id, 'auditor')
    await selectChoice(ownerPage.getByLabel('平台职责', { exact: true }), 'auditor')
    await ownerPage.waitForFunction(id => document.querySelectorAll('[data-admin-account]').length === 1 && document.querySelector('[data-admin-account]')?.getAttribute('data-admin-account') === id, member.user.user_id)
    await memberPage.reload()
    await memberPage.locator('[data-admin-account]').first().waitFor()
    assert.equal(await memberPage.locator('[data-admin-invitations]').count(), 0)
    assert.equal(await memberPage.getByRole('button', { name: '保存职责', exact: true }).count(), 0)

    await ownerPage.goto(`${origin}/spaces/current`)
    await ownerPage.locator('[data-space-management]').waitFor()
    assert.equal(await ownerPage.getByRole('tab', { name: '成员', exact: true }).count(), 0)
    await ownerPage.getByRole('button', { name: '创建团队', exact: true }).click()
    const create = ownerPage.getByRole('dialog', { name: '创建团队', exact: true })
    await create.getByLabel('团队名称').fill('B1 collaboration')
    await create.getByLabel('团队标识').fill('b1-collaboration')
    await create.getByRole('button', { name: '创建团队', exact: true }).click()
    await create.waitFor({ state: 'hidden' })
    const team = (await serverRequest(origin, '/tenants', { token: ownerToken })).tenants.find(space => space.slug === 'b1-collaboration')
    assert.ok(team)
    assert.equal(team.kind, 'team')
    const invite = ownerPage.locator(`[data-admin-invitations="${team.tenant_id}"]`)
    await invite.getByRole('button', { name: '生成邀请链接', exact: true }).click()
    const invitation = await invite.getByLabel('邀请链接', { exact: true }).inputValue()
    await memberPage.goto(invitation)
    const join = memberPage.getByRole('dialog', { name: '加入邀请中的团队', exact: true })
    await join.getByRole('button', { name: '加入团队', exact: true }).click()
    await join.waitFor({ state: 'hidden' })
    const spaces = (await serverRequest(origin, '/tenants', { token: oidc.accessToken('00') })).tenants
    assert.ok(spaces.some(space => space.tenant_id === member.personal_tenant_id && space.kind === 'personal'))
    assert.ok(spaces.some(space => space.tenant_id === team.tenant_id && space.role === 'member'))
    assert.equal((await serverRequest(origin, '/auth/session', { token: oidc.accessToken('00') })).personal_tenant_id, member.personal_tenant_id)

    await ownerPage.goto(`${origin}/admin/accounts`)
    await searchAccount(ownerPage, 'b1-account-00')
    for (const viewport of [{ width: 390, height: 844 }, { width: 320, height: 740 }, { width: 844, height: 390 }]) {
      await ownerPage.setViewportSize(viewport)
      await noOverflow(ownerPage, ownerPage.locator('[data-platform-admin]'))
      await ownerPage.screenshot({ path: path.join(artifacts, `admin-accounts-${viewport.width}.png`), animations: 'disabled' })
    }
    await ownerPage.goto(`${origin}/admin/workers`)
    await ownerPage.getByText('尚未启用托管执行', { exact: true }).waitFor()
    await noOverflow(ownerPage, ownerPage.locator('[data-platform-admin]'))
    await ownerPage.screenshot({ path: path.join(artifacts, 'admin-workers-disabled.png'), animations: 'disabled' })
    await ownerPage.reload()
    await ownerPage.getByText('尚未启用托管执行', { exact: true }).waitFor()
    const logout = ownerPage.getByRole('button', { name: '退出登录', exact: true })
    await logout.scrollIntoViewIfNeeded()
    const logoutBox = await logout.boundingBox()
    assert.ok(logoutBox && logoutBox.y >= 0 && logoutBox.y + logoutBox.height <= 390)
    assert.ok(await logout.evaluate(element => {
      const rectangle = element.getBoundingClientRect()
      return element.contains(document.elementFromPoint(rectangle.x + rectangle.width / 2, rectangle.y + rectangle.height / 2))
    }))
    await ownerPage.screenshot({ path: path.join(artifacts, 'admin-landscape-navigation.png'), animations: 'disabled' })
    await logout.click()
    await ownerPage.getByLabel('用户名', { exact: true }).waitFor()
    await ownerPage.getByLabel('密码', { exact: true }).waitFor()
    assert.deepEqual(errors, [])
    assert.ok(!application.diagnostics().includes('panicked'))
  } catch (error) {
    await ownerPage?.screenshot({ path: path.join(artifacts, 'admin-owner-failure.png'), animations: 'disabled' }).catch(() => {})
    await memberPage?.screenshot({ path: path.join(artifacts, 'admin-member-failure.png'), animations: 'disabled' }).catch(() => {})
    console.error({ artifacts, errors, diagnostics: application?.diagnostics() })
    throw error
  } finally {
    await browser?.close()
    if (application) await stopProcess(application)
    await oidc.close()
    await rm(directory, { recursive: true, force: true })
  }
})
