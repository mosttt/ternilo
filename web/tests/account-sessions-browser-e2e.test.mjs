import assert from 'node:assert/strict'
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { chromium } from 'playwright'
import { freePort, initializeServer, serverRequest, startOidcServer, stopProcess } from './platform-e2e-fixture.mjs'

async function openSettings(page) {
  await page.waitForURL(url => !url.pathname.startsWith('/auth/'))
  await page.locator('[data-app-frame]:visible, [data-user-settings]:visible').first().waitFor()
  const settings = page.locator('[data-user-settings]')
  if (!await settings.isVisible()) {
    const sidebar = page.getByRole('button', { name: '打开侧边栏', exact: true })
    if (await sidebar.isVisible()) await sidebar.click()
    await page.getByRole('button', { name: '用户设置', exact: true }).click()
  }
  await settings.getByRole('button', { name: '通用', exact: true }).click()
  await settings.locator('[data-account-sessions][aria-busy="false"]').waitFor()
  return settings.locator('[data-account-sessions]')
}

async function signIn(page, credentials) {
  await page.getByLabel('用户名', { exact: true }).fill(credentials.username)
  await page.getByLabel('密码', { exact: true }).fill(credentials.password)
  const pending = page.waitForResponse(response => new URL(response.url()).pathname === '/api/v1/auth/login')
  await page.getByRole('button', { name: '登录', exact: true }).click()
  const response = await pending
  assert.equal(response.status(), 200)
  await page.getByLabel('用户名', { exact: true }).waitFor({ state: 'hidden' })
  return response.json()
}

function observe(page, label, origin, report) {
  const connections = []
  const responseStatuses = new WeakMap()
  report.live[label] = connections
  page.on('pageerror', error => report.errors.push(`${label}: ${error.message}`))
  const revoked = () => connections.some(connection => connection.frames.some(frame => frame.type === 'error' && frame.code === 'policy_denied'))
  page.on('console', message => {
    if (message.type() !== 'error') return
    if (revoked() && message.text().includes('401') && message.location().url.endsWith('/api/v1/auth/session')) return
    report.errors.push(`${label} console: ${message.text()}`)
  })
  page.on('response', response => {
    const url = new URL(response.url())
    if (url.origin !== origin) return
    responseStatuses.set(response.request(), response.status())
    report.network.push({ page: label, method: response.request().method(), path: url.pathname, status: response.status() })
    if (response.status() === 401 && url.pathname === '/api/v1/auth/session' && revoked()) {
      report.expectedNetwork.push({ page: label, path: url.pathname, status: 401 })
    } else if (response.status() >= 400) report.errors.push(`${label} HTTP ${response.status()}: ${url.pathname}`)
  })
  page.on('requestfailed', request => {
    const pathname = new URL(request.url()).pathname
    const failure = request.failure()?.errorText
    if (request.method() === 'POST' && pathname === '/api/v1/auth/logout'
      && responseStatuses.get(request) === 204 && failure === 'net::ERR_ABORTED') {
      report.expectedNetwork.push({ page: label, path: pathname, status: 204, failure })
    } else report.errors.push(`${label} request failed: ${pathname}: ${failure}`)
  })
  page.on('websocket', socket => {
    if (new URL(socket.url()).pathname !== '/api/v1/live') return
    const connection = { openedAt: Date.now(), closedAt: null, frames: [] }
    connections.push(connection)
    socket.on('framereceived', event => {
      const frame = JSON.parse(String(event.payload))
      connection.frames.push({ type: frame.type, code: frame.code, at: Date.now() })
    })
    socket.on('close', () => { connection.closedAt = Date.now() })
    socket.on('socketerror', error => report.errors.push(`${label} WebSocket: ${error}`))
  })
  return connections
}

async function waitUntil(predicate, message, timeout = 2_000) {
  const deadline = Date.now() + timeout
  while (!predicate() && Date.now() < deadline) await new Promise(resolve => setTimeout(resolve, 20))
  assert.ok(predicate(), message)
}

async function readyConnection(connections) {
  await waitUntil(() => connections.some(connection => !connection.closedAt && connection.frames.some(frame => frame.type === 'ready')), 'browser establishes a real Live connection', 10_000)
  return connections.findLast(connection => !connection.closedAt && connection.frames.some(frame => frame.type === 'ready'))
}

function assertRetained(connection) {
  assert.equal(connection.closedAt, null, 'unrevoked Live connection remains open')
  assert.ok(!connection.frames.some(frame => frame.type === 'error'), 'unrevoked Live connection receives no authentication error')
}

async function assertLiveRevoked(connection, startedAt, report, label) {
  await waitUntil(() => connection.closedAt !== null, 'revoked Live connection closes without waiting for the five-second check')
  const denial = connection.frames.find(frame => frame.type === 'error' && frame.code === 'policy_denied' && frame.at >= startedAt)
  assert.ok(denial, 'revoked Live connection receives an authentication rejection')
  assert.ok(connection.closedAt - startedAt < 2_000, 'Live revocation completes within two seconds')
  assert.ok(!connection.frames.some(frame => frame.at > denial.at && ['workbench', 'activity', 'event_batch', 'session_metadata'].includes(frame.type)), 'no business frames follow authentication rejection')
  report.checks.push({ name: label, revokedWithinMs: connection.closedAt - startedAt })
}

async function confirmRevocation(page, label, pathname) {
  const dialog = page.getByRole('dialog', { name: label, exact: true })
  const pending = page.waitForResponse(response => new URL(response.url()).pathname === pathname
    && ['POST', 'DELETE'].includes(response.request().method()))
  await dialog.getByRole('button', { name: label, exact: true }).click()
  const response = await pending
  assert.equal(response.status(), 200)
  assert.equal(response.headers()['cache-control'], 'no-store')
  return response.json()
}

async function sessionStatus(origin, token) {
  const response = await fetch(`${origin}/api/v1/auth/session`, { headers: { authorization: `Bearer ${token}` } })
  await response.arrayBuffer()
  return response.status
}

async function checkLayout(page, sessions) {
  await sessions.scrollIntoViewIfNeeded()
  assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), 'page fits viewport')
  assert.ok(await sessions.evaluate(element => element.scrollWidth <= element.clientWidth), 'sessions fit viewport')
  for (const button of await sessions.locator('button:visible').all()) {
    const bounds = await button.boundingBox()
    assert.ok(bounds && bounds.width > 0 && bounds.height >= 40, 'session action has a touch target')
    assert.ok(bounds.x >= 0 && bounds.x + bounds.width <= page.viewportSize().width + 1, 'session action fits viewport')
  }
}

test('self-service browser sessions preserve account boundaries and external OIDC login', { timeout: 180_000 }, async () => {
  const temporary = await mkdtemp(path.join(tmpdir(), 'ternilo-account-sessions-browser-'))
  const artifacts = process.env.TERNILO_E2E_ARTIFACT_DIR ?? temporary
  await mkdir(artifacts, { recursive: true })
  const origin = `http://127.0.0.1:${await freePort()}`
  const provider = await startOidcServer({ audience: 'sessions-browser', subject: 'sessions-member', email: 'sessions-member@example.test', name: 'Sessions member' })
  let application, browser, page
  const report = { status: 'running', checks: [], screenshots: [], errors: [], expectedNetwork: [], network: [], live: {} }
  const screenshot = async (name, target = page) => {
    const filename = `${name}.png`
    await target.screenshot({ path: path.join(artifacts, filename), animations: 'disabled' })
    report.screenshots.push(filename)
  }
  try {
    application = await initializeServer({
      directory: path.join(temporary, 'server'), origin,
      oidc: { issuer: provider.issuer, audience: 'sessions-browser', client_id: 'sessions-browser' },
    })
    const ownerToken = application.owner.session.access_token
    const owner = (resource, options = {}) => serverRequest(origin, resource, { token: ownerToken, ...options })
    const registration = await owner('/admin/registration')
    await owner('/admin/registration', { method: 'PATCH', body: { mode: 'open', require_approval: false, revision: registration.revision } })
    const credentials = { username: 'sessions-member', password: 'sessions-browser-password' }
    const { session: member } = await serverRequest(origin, '/auth/register', { body: { ...credentials, email: 'sessions-member@example.test' } })
    const memberRequest = (resource, options = {}) => serverRequest(origin, resource, { token: member.access_token, ...options })
    await memberRequest('/auth/oidc-link', { body: { access_token: provider.accessToken() } })
    const initial = await memberRequest('/auth/sessions')
    const registrationId = initial.sessions[0].session_id
    const cross = await fetch(`${origin}/api/v1/auth/sessions/${registrationId}`, { method: 'DELETE', headers: { authorization: `Bearer ${ownerToken}` } })
    assert.equal(cross.status, 409)
    await cross.arrayBuffer()
    assert.equal(await sessionStatus(origin, registrationId), 401)
    assert.equal(await sessionStatus(origin, member.access_token), 200)
    await memberRequest('/auth/logout', { method: 'POST' })
    report.checks.push({ name: 'cross-account revoke denied; public ID rejected as bearer' })

    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
    const victimPage = await browser.newPage({ serviceWorkers: 'block' })
    const victimConnections = observe(victimPage, 'victim', origin, report)
    await victimPage.goto(origin)
    const victim = await signIn(victimPage, credentials)
    const victimConnection = await readyConnection(victimConnections)
    const targetId = (await serverRequest(origin, '/auth/sessions', { token: victim.access_token })).sessions[0].session_id
    const extraPage = await browser.newPage({ serviceWorkers: 'block' })
    const extraConnections = observe(extraPage, 'other-member-session', origin, report)
    await extraPage.goto(origin)
    const extra = await signIn(extraPage, credentials)
    const extraConnection = await readyConnection(extraConnections)
    const ownerPage = await browser.newPage({ serviceWorkers: 'block' })
    const ownerConnections = observe(ownerPage, 'other-account', origin, report)
    await ownerPage.goto(origin)
    await signIn(ownerPage, application.owner)
    const ownerConnection = await readyConnection(ownerConnections)
    page = await browser.newPage({ viewport: { width: 1440, height: 1000 }, hasTouch: true, serviceWorkers: 'block' })
    const currentConnections = observe(page, 'actor', origin, report)
    await page.goto(origin)
    const current = await signIn(page, credentials)
    const currentConnection = await readyConnection(currentConnections)
    let sessions = await openSettings(page)
    assert.equal(await page.locator('[data-account-id]').textContent(), member.user.user_id)
    assert.equal(await page.locator('[data-account-email]').textContent(), 'sessions-member@example.test')
    assert.equal(await sessions.locator('[data-account-session]').count(), 3)
    assert.equal(await sessions.locator('[data-current-session]').count(), 1)
    const currentRow = sessions.locator('[data-account-session]').filter({ has: page.locator('[data-current-session]') })
    assert.match(await currentRow.textContent(), /首次来源 IP：127\.0\.0\.1/)
    assert.match(await currentRow.textContent(), /最近来源 IP：127\.0\.0\.1/)
    assert.match(await currentRow.textContent(), /最后活动：/)
    assert.match(await currentRow.textContent(), /Chrome/)
    assert.match(await currentRow.textContent(), /主机名：浏览器不提供此信息/)
    const sessionDetails = await serverRequest(origin, '/auth/sessions', { token: current.access_token })
    const currentDetails = sessionDetails.sessions.find(session => session.is_current)
    assert.equal(currentDetails.first_ip, '127.0.0.1')
    assert.equal(currentDetails.last_ip, '127.0.0.1')
    assert.ok(currentDetails.last_active_at_ms >= currentDetails.created_at_ms)
    assert.match(currentDetails.user_agent, /Chrome/)
    const memberSessionIds = await sessions.locator('[data-account-session]').evaluateAll(elements => elements.map(element => element.getAttribute('data-account-session')))

    for (const width of [1440, 390, 320]) {
      await page.setViewportSize({ width, height: 1000 })
      for (const [theme, name] of [['light', '浅色'], ['dark', '深色']]) {
        await page.getByRole('combobox', { name: '界面主题', exact: true }).click()
        await page.getByRole('option', { name, exact: true }).click()
        await checkLayout(page, sessions)
        assert.equal(await page.evaluate(() => document.documentElement.style.colorScheme), theme)
        await sessions.getByRole('button', { name: '撤销当前会话', exact: true }).click()
        const dialog = page.getByRole('dialog', { name: '撤销当前会话', exact: true })
        assert.match(await dialog.textContent(), /立即退出当前登录/)
        assert.ok(await dialog.evaluate(element => element.scrollWidth <= element.clientWidth))
        await dialog.getByRole('button', { name: '取消', exact: true }).click()
        await page.locator('[data-settings-dialog]').waitFor({ state: 'hidden' })
        assert.equal(await sessionStatus(origin, current.access_token), 200)
        await screenshot(`verified-account-sessions-${width}-${theme}`)
        report.checks.push({ name: `layout-${width}-${theme}; cancelled current revocation preserves login` })
      }
    }
    await sessions.locator(`[data-account-session="${targetId}"]`).getByRole('button', { name: '撤销会话', exact: true }).click()
    const singleStartedAt = Date.now()
    assert.deepEqual(await confirmRevocation(page, '撤销会话', `/api/v1/auth/sessions/${targetId}`), { revoked_count: 1, current_revoked: false })
    await assertLiveRevoked(victimConnection, singleStartedAt, report, 'individual revocation closes connected victim')
    assertRetained(extraConnection)
    assertRetained(currentConnection)
    assertRetained(ownerConnection)
    await victimPage.close()
    await sessions.locator(`[data-account-session="${targetId}"]`).waitFor({ state: 'detached' })
    assert.equal(await sessionStatus(origin, victim.access_token), 401)
    await sessions.getByRole('button', { name: '撤销其他会话', exact: true }).click()
    const bulkStartedAt = Date.now()
    assert.deepEqual(await confirmRevocation(page, '撤销其他会话', '/api/v1/auth/sessions/revoke-others'), { revoked_count: 1, current_revoked: false })
    await assertLiveRevoked(extraConnection, bulkStartedAt, report, 'bulk revocation closes other connected member session')
    assertRetained(currentConnection)
    assertRetained(ownerConnection)
    await extraPage.close()
    await page.waitForFunction(() => document.querySelectorAll('[data-account-session]').length === 1)
    assert.equal(await sessionStatus(origin, extra.access_token), 401)
    assert.equal(await sessionStatus(origin, current.access_token), 200)
    assert.equal(await sessionStatus(origin, ownerToken), 200)
    const currentId = await sessions.locator('[data-account-session]').getAttribute('data-account-session')
    await sessions.getByRole('button', { name: '撤销当前会话', exact: true }).click()
    assert.deepEqual(await confirmRevocation(page, '撤销当前会话', `/api/v1/auth/sessions/${currentId}`), { revoked_count: 1, current_revoked: true })
    await page.getByLabel('用户名', { exact: true }).waitFor()
    await waitUntil(() => currentConnection.closedAt !== null, 'self-revocation closes the current Live connection')
    assertRetained(ownerConnection)
    assert.deepEqual(await page.evaluate(() => ['localStorage', 'sessionStorage'].map(storage => window[storage].getItem('ternilo.native.session'))), [null, null])
    assert.equal(await sessionStatus(origin, current.access_token), 401)
    report.checks.push({ name: 'confirmed current revocation signs out, clears credentials and closes Live' })
    await page.getByLabel('用户名', { exact: true }).waitFor({ state: 'visible' })
    await screenshot('verified-current-session-signed-out')

    const switchedOwner = await signIn(page, application.owner)
    sessions = await openSettings(page)
    assert.equal(await page.locator('[data-account-id]').textContent(), switchedOwner.user.user_id)
    const ownerSessionIds = await sessions.locator('[data-account-session]').evaluateAll(elements => elements.map(element => element.getAttribute('data-account-session')))
    assert.ok(ownerSessionIds.length > 0)
    assert.ok(ownerSessionIds.every(sessionId => !memberSessionIds.includes(sessionId)), 'switching accounts does not retain the former account session list')
    assert.equal(await page.locator('[data-settings-dialog]').count(), 0)
    await screenshot('verified-switched-owner-sessions', page.locator('[data-account-settings]'))
    report.checks.push({ name: 'same browser switches member to owner without former sessions or dialog' })
    await page.getByRole('button', { name: '返回工作台', exact: true }).click()
    const sidebar = page.getByRole('button', { name: '打开侧边栏', exact: true })
    if (await sidebar.isVisible()) await sidebar.click()
    await page.getByRole('button', { name: '退出登录', exact: true }).click()
    await page.getByLabel('用户名', { exact: true }).waitFor()
    assert.equal(await sessionStatus(origin, switchedOwner.access_token), 401)

    const nativeForOidc = await serverRequest(origin, '/auth/login', { body: credentials })
    const oidcLogin = page.waitForResponse(response => new URL(response.url()).pathname === '/api/v1/auth/session'
      && !response.request().headers().authorization?.startsWith('Bearer kns_'))
    await page.getByRole('button', { name: '使用组织账号登录', exact: true }).click()
    const oidcResponse = await oidcLogin
    assert.equal(oidcResponse.status(), 200)
    assert.equal((await oidcResponse.json()).user.user_id, member.user.user_id)
    await page.waitForURL(url => url.origin === origin && !url.pathname.startsWith('/auth/') && !url.search && !url.hash)
    await page.getByLabel('用户名', { exact: true }).waitFor({ state: 'hidden' })
    sessions = await openSettings(page)
    assert.equal(await page.locator('[data-account-id]').textContent(), member.user.user_id)
    const oidcSessionIds = await sessions.locator('[data-account-session]').evaluateAll(elements => elements.map(element => element.getAttribute('data-account-session')))
    assert.ok(oidcSessionIds.every(sessionId => !ownerSessionIds.includes(sessionId)), 'switching back to the member through OIDC does not retain owner sessions')
    const oidcConnection = await readyConnection(currentConnections)
    await sessions.locator('[data-account-sessions-oidc]').waitFor()
    assert.equal(await sessions.locator('[data-current-session]').count(), 0)
    assert.match(await sessions.textContent(), /不会结束身份提供方的外部会话/)
    await sessions.getByRole('button', { name: '撤销全部原生会话', exact: true }).click()
    assert.deepEqual(await confirmRevocation(page, '撤销全部原生会话', '/api/v1/auth/sessions/revoke-others'), { revoked_count: 1, current_revoked: false })
    await sessions.getByText('没有有效的原生登录会话。', { exact: true }).waitFor()
    assert.equal(await sessionStatus(origin, nativeForOidc.access_token), 401)
    const oidcToken = await page.evaluate(() => sessionStorage.getItem('ternilo.oidc.access'))
    assert.ok(oidcToken)
    assert.equal(await sessionStatus(origin, oidcToken), 200)
    assert.equal(await sessionStatus(origin, ownerToken), 200)
    assertRetained(oidcConnection)
    assertRetained(ownerConnection)
    report.checks.push({ name: 'OIDC revokes only native sessions while external login and Live remain valid' })
    await page.getByRole('combobox', { name: '语言', exact: true }).click()
    await page.getByRole('option', { name: 'English', exact: true }).click()
    await sessions.getByText('Browser sign-in sessions', { exact: true }).waitFor()
    assert.doesNotMatch(await sessions.textContent(), /[\u3400-\u9fff]/)
    await checkLayout(page, sessions)
    await screenshot('verified-oidc-empty-english-mobile')
    assert.deepEqual(report.errors, [])
    report.status = 'passed'
  } catch (error) {
    report.status = 'failed'
    report.failure = error.message.replace(/(code|state)=[^&\s"]+/g, '$1=[redacted]')
    await screenshot('failure').catch(() => {})
    error.message += `\n${report.errors.join('\n')}\n${application?.diagnostics() ?? ''}`
    throw error
  } finally {
    await writeFile(path.join(artifacts, 'result.json'), `${JSON.stringify(report, null, 2)}\n`)
    await browser?.close()
    await stopProcess(application)
    await provider.close()
    await rm(temporary, { recursive: true, force: true })
  }
})
