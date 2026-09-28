import { selectChoice } from './browser-select-fixture.mjs'
import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { chromium } from 'playwright'
import { freePort, initializeServer, repository, serverRequest, stopProcess } from './platform-e2e-fixture.mjs'

async function login(page, origin, username, password, destination = '/') {
  await page.goto(`${origin}${destination}`)
  await page.getByLabel('用户名', { exact: true }).fill(username)
  await page.getByLabel('密码', { exact: true }).fill(password)
  await page.getByRole('button', { name: '登录', exact: true }).click()
  await page.getByRole('dialog').waitFor({ state: 'hidden' })
}

async function savePolicy(page, mode, approval = false) {
  const panel = page.locator('[data-registration-settings]')
  await selectChoice(panel.getByLabel('注册方式', { exact: true }), mode)
  if (mode === 'open') await panel.getByRole('checkbox', { name: /新注册账号需要审核/ }).setChecked(approval)
  else assert.equal(await panel.getByRole('checkbox').count(), 0)
  const response = page.waitForResponse(response => new URL(response.url()).pathname === '/api/v1/admin/registration' && response.request().method() === 'PATCH')
  await panel.getByRole('button', { name: '保存注册设置', exact: true }).click()
  assert.equal((await response).status(), 200)
  await page.waitForFunction(() => document.querySelector('[data-registration-settings] button[type="submit"]')?.disabled)
}

async function register(page, origin, username, review) {
  await page.goto(origin)
  await page.getByRole('button', { name: '注册新账号', exact: true }).click()
  assert.equal(await page.locator('#server-account-token').count(), 0)
  assert.equal(await page.getByRole('button', { name: '使用邀请创建账号', exact: true }).count(), 0)
  await page.getByLabel('用户名', { exact: true }).fill(username)
  await page.getByLabel('密码', { exact: true }).fill('registration-password')
  assert.equal(await page.getByRole('button', { name: review ? '提交注册申请' : '注册并登录', exact: true }).isDisabled(), true)
  const email = page.getByLabel('邮箱', { exact: true })
  assert.equal(await email.getAttribute('type'), 'email')
  await email.fill(`${username}@example.test`)
  const response = page.waitForResponse(response => new URL(response.url()).pathname === '/api/v1/auth/register')
  await page.getByRole('button', { name: review ? '提交注册申请' : '注册并登录', exact: true }).click()
  const result = await response
  assert.equal(result.status(), 201)
  return result.json()
}

async function review(page, id, decision) {
  const row = page.locator(`[data-admin-account="${id}"]`)
  const label = decision === 'approve' ? '通过审核' : '拒绝申请'
  await row.getByRole('button', { name: label, exact: true }).click()
  const dialog = page.getByRole('dialog', { name: decision === 'approve' ? '通过注册审核？' : '拒绝注册申请？', exact: true })
  const response = page.waitForResponse(response => new URL(response.url()).pathname === `/api/v1/admin/accounts/${id}/review` && response.request().method() === 'POST')
  await dialog.getByRole('button', { name: label, exact: true }).click()
  assert.equal((await response).status(), 200)
  await dialog.waitFor({ state: 'hidden' })
}

async function changeAccountStatus(page, id, action, artifacts) {
  const row = page.locator(`[data-admin-account="${id}"]`)
  const username = await row.locator('strong').textContent()
  const labels = { ban: '封禁账号', unban: '解除封禁', remove: '注销账号' }
  const titles = { ban: '封禁这个账号？', unban: '解除账号封禁？', remove: '永久注销这个账号？' }
  await row.getByRole('button', { name: `账号“${username}”的操作`, exact: true }).click()
  await page.getByRole('menuitem', { name: labels[action], exact: true }).click()
  const dialog = page.getByRole('dialog', { name: titles[action], exact: true })
  assert.ok((await dialog.textContent()).includes(username))
  if (action === 'remove') {
    assert.match(await dialog.textContent(), /不能恢复/)
    assert.match(await dialog.textContent(), /历史会话、用量和审计归属保留/)
    assert.match(await dialog.textContent(), /不会自动删除项目文件/)
  }
  await assertLayout(page, '[role="dialog"]')
  await page.screenshot({ path: path.join(artifacts, `account-${action}-confirmation.png`), animations: 'disabled' })
  const response = page.waitForResponse(response => new URL(response.url()).pathname === `/api/v1/admin/accounts/${id}/status` && response.request().method() === 'POST')
  await dialog.getByRole('button', { name: labels[action], exact: true }).click()
  assert.equal((await response).status(), 200)
  await dialog.waitFor({ state: 'hidden' })
}

async function assertLayout(page, selector) {
  const element = page.locator(selector)
  assert.equal(await element.evaluate(element => element.scrollWidth <= element.clientWidth), true)
  assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true)
}

async function accountSocket(origin, bearerToken, tenantId, expectReady) {
  const observed = { socket: new WebSocket(`${origin.replace('http:', 'ws:')}/api/v1/live`), frames: [], closed: false }
  await new Promise((resolve, reject) => {
    let authenticated = false
    const timeout = setTimeout(() => { observed.socket.close(); reject(new Error('Account socket did not settle')) }, 10_000)
    const finish = () => { clearTimeout(timeout); resolve() }
    observed.socket.addEventListener('open', () => observed.socket.send(JSON.stringify({ type: 'hello', protocol_version: 1, bearer_token: bearerToken, tenant_id: tenantId })))
    observed.socket.addEventListener('message', event => {
      observed.frames.push(JSON.parse(event.data))
      if (expectReady && observed.frames.some(frame => frame.type === 'ready') && observed.frames.some(frame => frame.type === 'workbench')) { authenticated = true; finish() }
    })
    observed.socket.addEventListener('close', () => {
      observed.closed = true
      clearTimeout(timeout)
      if (!expectReady) finish()
      else if (!authenticated) reject(new Error('Account socket closed before authentication'))
    })
    observed.socket.addEventListener('error', () => { clearTimeout(timeout); reject(new Error('Account socket transport failed')) })
  })
  return observed
}

async function waitForSocketClose(observed) {
  const deadline = Date.now() + 10_000
  while (!observed.closed && Date.now() < deadline) await new Promise(resolve => setTimeout(resolve, 20))
  assert.equal(observed.closed, true, 'banning an account closes its authenticated live connection')
  assert.equal(observed.frames.some(frame => frame.type === 'error'), true, 'revocation sends an authentication error before closing')
}


async function verifySettingsLayout(page, origin, artifacts) {
  await page.setViewportSize({ width: 1920, height: 1080 })
  await page.goto(`${origin}/settings/general`)
  const content = page.locator('[data-settings-page-content]')
  await content.getByLabel('语言', { exact: true }).waitFor()
  const bounds = await content.boundingBox()
  const navigation = page.getByRole('navigation', { name: '设置分类', exact: true })
  for (const label of ['通用', '模型', '插件', 'Agent 预设', '凭据与登录', '我的机器', '关于与诊断']) {
    await navigation.getByRole('button', { name: label, exact: true }).click()
    await page.waitForFunction(() => document.querySelector('[data-settings-page-content]')?.textContent.trim().length > 0)
    const actual = await content.boundingBox()
    assert.ok(Math.abs(actual.x - bounds.x) <= 1, `${label}: consistent content origin`)
    assert.ok(Math.abs(actual.width - bounds.width) <= 1, `${label}: consistent content width`)
    const heading = await content.locator('header').first().boundingBox()
    assert.ok(heading && Math.abs(heading.x - bounds.x) <= 1 && Math.abs(heading.width - bounds.width) <= 1, `${label}: visible section uses the shared content column`)
    await assertLayout(page, '[data-settings-page-content]')
    if (label === '通用' || label === '我的机器') await page.screenshot({ path: path.join(artifacts, label === '通用' ? 'settings-general-desktop.png' : 'settings-computers-desktop.png'), animations: 'disabled' })
  }
  for (const width of [390, 320]) {
    await page.setViewportSize({ width, height: 844 })
    for (const label of ['通用', '我的机器']) {
      await navigation.getByRole('button', { name: label, exact: true }).click()
      await assertLayout(page, '[data-settings-page-content]')
      if (label === '通用') {
        await selectChoice(content.getByLabel('对话正文字号', { exact: true }), '16')
        assert.equal(await content.getByLabel('对话正文字号', { exact: true }).inputValue(), '16')
      }
    }
  }
  for (const width of [1920, 390, 320]) {
    await page.setViewportSize({ width, height: 960 })
    for (const route of ['/models', '/admin/accounts', '/admin/workers', '/admin/instance', '/files']) {
      await page.goto(`${origin}${route}`)
      const main = page.locator('main').first()
      await main.waitFor()
      if (route === '/admin/accounts') await main.locator('[data-admin-account]').first().waitFor()
      else if (route === '/admin/workers') await main.getByText('尚未启用托管执行', { exact: true }).waitFor()
      else if (route === '/admin/instance') await main.getByLabel('访问模式', { exact: true }).waitFor()
      else if (route === '/models') await main.locator('[data-model-access]').waitFor()
      else if (route === '/files') await main.locator('[data-files-empty]').waitFor()
      await assertLayout(page, 'main')
      assert.ok((await main.textContent()).trim().length > 0, `${route}: content remains available`)
    }
  }
}

test('account registration enforces invitation, open signup, and approval through distinct user and admin pages', { timeout: 180_000 }, async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-registration-browser-'))
  const artifacts = process.env.TERNILO_E2E_ARTIFACT_DIR ?? directory
  await mkdir(artifacts, { recursive: true })
  let application, browser
  const errors = []
  const assetHashes = {}
  const expectedFailures = []
  const handledFailures = []
  const expectedConsoleLocations = new Set()
  const pages = {}
  const sockets = []
  const socketEvidence = {}
  try {
    const origin = `http://127.0.0.1:${await freePort()}`
    application = await initializeServer({ directory: path.join(directory, 'server'), origin })
    for (const asset of ['app.js', 'app.css']) {
      const response = await fetch(`${origin}/assets/${asset}`)
      const served = createHash('sha256').update(Buffer.from(await response.arrayBuffer())).digest('hex')
      const expected = createHash('sha256').update(await readFile(path.join(repository, 'web/dist/assets', asset))).digest('hex')
      assert.equal(served, expected, `Server embeds current ${asset}`)
      assetHashes[asset] = served
    }
    const token = application.owner.session.access_token
    const ownerRequest = (resource, options = {}) => serverRequest(origin, resource, { token, ...options })
    assert.equal((await ownerRequest('/admin/registration')).mode, 'invite')
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
    const contexts = await Promise.all([0, 1, 2, 3].map(() => browser.newContext({ viewport: { width: 1440, height: 960 }, serviceWorkers: 'block' })))
    const [owner, user, admin, auditor] = await Promise.all(contexts.map(context => context.newPage()))
    Object.assign(pages, { owner, user, admin, auditor })
    for (const page of [owner, user, admin, auditor]) {
      page.on('pageerror', error => errors.push(`page: ${error.message}`))
      page.on('console', message => {
        if (message.type() !== 'error') return
        if (message.text().includes('status of 403') && expectedConsoleLocations.has(message.location().url)) return
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
    }
    await login(owner, origin, application.owner.username, application.owner.password, '/admin/accounts')
    await owner.locator('[data-admin-invitations="platform"]').waitFor()
    const ownerRow = owner.locator(`[data-admin-account="${application.owner.session.user.user_id}"]`)
    await ownerRow.waitFor()
    assert.equal(await ownerRow.getByRole('button').count(), 0, 'the instance owner has no dangerous account actions')
    await user.goto(origin)
    assert.equal(await user.getByRole('button', { name: '注册新账号', exact: true }).count(), 0)
    await user.getByRole('button', { name: '使用邀请创建账号', exact: true }).waitFor()
    const accounts = {}
    for (const role of ['admin', 'auditor']) {
      const invitation = await ownerRequest('/admin/invitations', { body: { tenant_id: null, role: 'member', expires_in_seconds: 3600 } })
      const session = await serverRequest(origin, '/auth/invitations/accept', { body: { token: invitation.token, username: `registration-${role}`, email: `registration-${role}@example.test`, password: 'registration-password' } })
      const account = (await ownerRequest(`/admin/accounts?query=${encodeURIComponent(session.user.user_id)}`)).accounts[0]
      await ownerRequest(`/admin/accounts/${account.user_id}/role`, { method: 'PATCH', body: { role, role_revision: account.role_revision } })
      accounts[role] = session
    }
    await owner.getByRole('button', { name: '生成邀请链接', exact: true }).click()
    const invitationUrl = await owner.getByLabel('邀请链接', { exact: true }).inputValue()
    assert.ok(new URLSearchParams(new URL(invitationUrl).hash.slice(1)).has('invite'))
    await user.goto(invitationUrl)
    await user.getByLabel('用户名', { exact: true }).fill('invited-user')
    await user.getByLabel('邮箱', { exact: true }).fill('invited-user@example.test')
    await user.getByLabel('密码', { exact: true }).fill('registration-password')
    await user.getByRole('button', { name: '创建账号', exact: true }).click()
    await user.getByRole('dialog').waitFor({ state: 'hidden' })
    assert.equal(await user.locator('[data-registration-pending]').count(), 0)
    await savePolicy(owner, 'open')
    assert.equal(await owner.locator('[data-admin-invitations="platform"]').count(), 0)
    await contexts[1].clearCookies()
    await user.evaluate(() => sessionStorage.clear())
    const open = await register(user, origin, 'open-user', false)
    assert.equal(open.status, 'active')
    assert.ok(open.session.access_token)
    await user.getByRole('dialog').waitFor({ state: 'hidden' })
    await user.goto(`${origin}/admin/accounts`)
    await user.getByText('你没有这个页面的管理权限。', { exact: true }).waitFor()
    await login(admin, origin, 'registration-admin', 'registration-password', '/admin/accounts')
    assert.equal(await admin.locator(`[data-admin-account="${accounts.admin.user.user_id}"]`).getByRole('button').count(), 0, 'administrators cannot operate on their own account')
    await savePolicy(admin, 'open', true)
    await admin.setViewportSize({ width: 390, height: 844 })
    await assertLayout(admin, '[data-admin-accounts]')
    await user.evaluate(() => sessionStorage.clear())
    await user.setViewportSize({ width: 390, height: 640 })
    const pending = await register(user, origin, 'review-user', true)
    assert.equal(pending.status, 'pending')
    assert.equal(pending.session, null)
    await user.locator('[data-registration-pending]').waitFor()
    assert.equal(await user.evaluate(() => sessionStorage.getItem('ternilo.native.session')), null)
    await assertLayout(user, '[role="dialog"]')
    await user.screenshot({ path: path.join(artifacts, 'registration-pending-mobile.png'), animations: 'disabled' })
    await user.getByRole('button', { name: '返回登录', exact: true }).click()
    await user.getByLabel('密码', { exact: true }).fill('registration-password')
    expectedFailures.push('POST 403 /api/v1/auth/login')
    expectedConsoleLocations.add(`${origin}/api/v1/auth/login`)
    const denied = user.waitForResponse(response => new URL(response.url()).pathname === '/api/v1/auth/login')
    await user.getByRole('button', { name: '登录', exact: true }).click()
    assert.equal((await denied).status(), 403)
    await user.getByRole('alert').filter({ hasText: '等待管理员审核' }).waitFor()
    await selectChoice(admin.getByLabel('账号状态', { exact: true }), 'pending')
    await admin.locator(`[data-admin-account="${pending.user_id}"]`).waitFor()
    await admin.screenshot({ path: path.join(artifacts, 'registration-review-mobile.png'), animations: 'disabled' })
    await review(admin, pending.user_id, 'approve')
    await user.getByRole('button', { name: '登录', exact: true }).click()
    await user.getByRole('dialog').waitFor({ state: 'hidden' })
    await user.evaluate(() => sessionStorage.clear())
    const rejected = await register(user, origin, 'rejected-user', true)
    await admin.getByRole('button', { name: '刷新', exact: true }).click()
    await admin.locator(`[data-admin-account="${rejected.user_id}"]`).waitFor()
    await review(admin, rejected.user_id, 'reject')
    await user.getByRole('button', { name: '返回登录', exact: true }).click()
    await user.getByLabel('密码', { exact: true }).fill('registration-password')
    expectedFailures.push('POST 403 /api/v1/auth/login')
    await user.getByRole('button', { name: '登录', exact: true }).click()
    await user.getByRole('alert').filter({ hasText: '未通过审核' }).waitFor()
    await selectChoice(admin.getByLabel('账号状态', { exact: true }), 'rejected')
    await admin.getByLabel('搜索账号', { exact: true }).fill('rejected-user@example.test')
    await admin.getByRole('button', { name: '搜索', exact: true }).click()
    let target = admin.locator(`[data-admin-account="${rejected.user_id}"]`)
    await target.getByText('rejected-user@example.test', { exact: true }).waitFor()
    await review(admin, rejected.user_id, 'approve')
    await user.getByRole('button', { name: '登录', exact: true }).click()
    await user.getByRole('dialog').waitFor({ state: 'hidden' })
    await user.goto(`${origin}/settings/general`)
    await user.locator('[data-account-email]').waitFor()
    assert.equal(await user.locator('[data-account-email]').textContent(), 'rejected-user@example.test')
    assert.equal(await user.locator('[data-account-id]').textContent(), rejected.user_id)
    const beforeBan = await user.evaluate(() => JSON.parse(sessionStorage.getItem('ternilo.native.session')).access_token)
    const beforeBanIdentity = await serverRequest(origin, '/auth/session', { token: beforeBan })
    const originalSocket = await accountSocket(origin, beforeBan, beforeBanIdentity.personal_tenant_id, true)
    sockets.push(originalSocket)
    socketEvidence.authenticatedBeforeBan = true
    await selectChoice(admin.getByLabel('账号状态', { exact: true }), 'active')
    await target.waitFor()
    await changeAccountStatus(admin, rejected.user_id, 'ban', artifacts)
    await waitForSocketClose(originalSocket)
    socketEvidence.banSentErrorAndClosed = true
    const oldCredential = () => fetch(`${origin}/api/v1/auth/session`, { headers: { authorization: `Bearer ${beforeBan}` } })
    assert.ok([401, 403].includes((await oldCredential()).status), 'ban rejects an already issued credential')
    await user.evaluate(() => sessionStorage.clear())
    await user.goto(origin)
    await user.getByLabel('用户名', { exact: true }).fill('rejected-user')
    await user.getByLabel('密码', { exact: true }).fill('registration-password')
    expectedFailures.push('POST 403 /api/v1/auth/login')
    await user.getByRole('button', { name: '登录', exact: true }).click()
    await user.getByRole('alert').filter({ hasText: '账号已被封禁' }).waitFor()
    await selectChoice(admin.getByLabel('账号状态', { exact: true }), 'banned')
    await target.waitFor()
    await changeAccountStatus(admin, rejected.user_id, 'unban', artifacts)
    assert.equal(originalSocket.closed, true)
    const retiredSocket = await accountSocket(origin, beforeBan, beforeBanIdentity.personal_tenant_id, false)
    sockets.push(retiredSocket)
    assert.equal(retiredSocket.frames.some(frame => frame.type === 'ready'), false, 'unbanning cannot reauthenticate a revoked token')
    assert.equal(retiredSocket.frames.some(frame => frame.type === 'error'), true)
    socketEvidence.oldSocketStayedClosed = true
    socketEvidence.revokedTokenCouldNotReconnect = true
    assert.ok([401, 403].includes((await oldCredential()).status), 'unbanning does not restore the previous credential')
    await user.getByRole('button', { name: '登录', exact: true }).click()
    await user.getByRole('dialog').waitFor({ state: 'hidden' })
    await admin.setViewportSize({ width: 1440, height: 960 })
    await selectChoice(admin.getByLabel('账号状态', { exact: true }), 'active')
    await target.waitFor()
    await changeAccountStatus(admin, rejected.user_id, 'remove', artifacts)
    await selectChoice(admin.getByLabel('账号状态', { exact: true }), 'removed')
    await admin.getByLabel('搜索账号', { exact: true }).fill(rejected.user_id)
    await admin.getByRole('button', { name: '搜索', exact: true }).click()
    await target.locator('[data-account-status="removed"]').waitFor()
    assert.equal(await target.getByRole('button').count(), 0, 'a closed account remains searchable without restore actions')
    await admin.screenshot({ path: path.join(artifacts, 'account-closed-directory-desktop.png'), animations: 'disabled' })
    await user.evaluate(() => sessionStorage.clear())
    await user.goto(origin)
    await user.getByLabel('用户名', { exact: true }).fill('rejected-user')
    await user.getByLabel('密码', { exact: true }).fill('registration-password')
    expectedFailures.push('POST 403 /api/v1/auth/login')
    await user.getByRole('button', { name: '登录', exact: true }).click()
    await user.getByRole('alert').filter({ hasText: '账号已注销' }).waitFor()
    await user.evaluate(() => sessionStorage.clear())
    await admin.getByLabel('搜索账号', { exact: true }).fill('')
    await admin.getByRole('button', { name: '搜索', exact: true }).click()
    for (let index = 0; index < 19; index += 1) {
      await serverRequest(origin, '/auth/register', { body: {
        username: `paging-user-${index}`, email: `paging-user-${index}@example.test`, password: 'registration-password',
      } })
    }
    await owner.getByRole('button', { name: '刷新', exact: true }).click()
    await owner.waitForFunction(() => document.querySelectorAll('[data-admin-account]').length === 25)
    const firstPageIds = await owner.locator('[data-admin-account]').evaluateAll(elements => elements.map(element => element.getAttribute('data-admin-account')))
    await owner.getByRole('button', { name: '下一页', exact: true }).click()
    await owner.waitForFunction(() => {
      const count = document.querySelectorAll('[data-admin-account]').length
      return count > 0 && count < 25
    })
    const secondPageIds = await owner.locator('[data-admin-account]').evaluateAll(elements => elements.map(element => element.getAttribute('data-admin-account')))
    assert.equal(secondPageIds.some(id => firstPageIds.includes(id)), false)
    await owner.getByRole('button', { name: '上一页', exact: true }).click()
    await owner.waitForFunction(() => document.querySelectorAll('[data-admin-account]').length === 25)
    await login(auditor, origin, 'registration-auditor', 'registration-password', '/admin/accounts')
    assert.equal(await auditor.getByLabel('注册方式', { exact: true }).isDisabled(), true)
    assert.equal(await auditor.getByRole('button', { name: '保存注册设置', exact: true }).count(), 0)
    assert.equal(await auditor.getByRole('button', { name: '拒绝申请', exact: true }).count(), 0)
    await owner.getByRole('button', { name: '退出登录', exact: true }).click()
    await owner.getByRole('button', { name: '注册新账号', exact: true }).waitFor()
    assert.equal(await owner.getByRole('button', { name: '使用邀请创建账号', exact: true }).count(), 0, 'same-tab logout refreshes the current registration policy')
    await login(owner, origin, application.owner.username, application.owner.password, '/admin/accounts')
    await selectChoice(admin.getByLabel('账号状态', { exact: true }), '')
    await savePolicy(admin, 'invite')
    assert.equal(await admin.getByRole('checkbox').count(), 0)
    await admin.locator('[data-admin-invitations="platform"]').waitFor()
    await user.reload()
    assert.equal(await user.getByRole('button', { name: '注册新账号', exact: true }).count(), 0)
    await user.getByRole('button', { name: '使用邀请创建账号', exact: true }).waitFor()
    await user.evaluate(() => localStorage.setItem('ternilo.locale', 'en'))
    await user.reload()
    await user.getByRole('button', { name: 'Create an account with an invitation', exact: true }).waitFor()
    await user.screenshot({ path: path.join(artifacts, 'registration-invite-mobile-en.png'), animations: 'disabled' })
    await verifySettingsLayout(owner, origin, artifacts)
    assert.deepEqual(expectedFailures, [], 'all expected denials were exercised')
    assert.equal(handledFailures.length, 4)
    assert.deepEqual(errors, [])
  } catch (error) {
    await Promise.allSettled(Object.entries(pages).map(([name, page]) => page.screenshot({ path: path.join(artifacts, `account-failure-${name}.png`), animations: 'disabled' })))
    process.stderr.write(`${application?.diagnostics() ?? ''}\n`)
    throw error
  } finally {
    sockets.forEach(observed => observed.socket.close())
    await writeFile(path.join(artifacts, 'registration-observations.json'), JSON.stringify({ assetHashes, errors, handledFailures, missingExpectedFailures: expectedFailures, socketEvidence }, null, 2))
    await browser?.close()
    await stopProcess(application)
    await rm(directory, { recursive: true, force: true })
  }
})
