import assert from 'node:assert/strict'
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { createHash } from 'node:crypto'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { chromium } from 'playwright'
import { localApi, until } from './model-device-fixture.mjs'
import { freePort, initializeServer, repository, selectSpace, serverRequest, startProcess, stopProcess, waitForHttp } from './platform-e2e-fixture.mjs'

async function openArchive(browser, origin, account, tenantId, mobile, errors, network) {
  const context = await browser.newContext({ locale: 'zh-CN', viewport: { width: 1440, height: 960 }, serviceWorkers: 'block', hasTouch: mobile })
  await context.addInitScript(() => localStorage.setItem('ternilo.locale', 'zh'))
  const page = await context.newPage()
  page.on('pageerror', error => errors.push(error.message))
  page.on('console', message => { if (message.type() === 'error') errors.push(message.text()) })
  page.on('response', response => {
    const pathname = new URL(response.url()).pathname
    network.push({ method: response.request().method(), pathname, status: response.status() })
    if (response.status() >= 400) errors.push(`${response.status()} ${pathname}`)
  })
  page.on('requestfailed', request => {
    if (!request.failure()?.errorText.includes('ERR_ABORTED')) errors.push(`${request.failure()?.errorText} ${new URL(request.url()).pathname}`)
  })
  await page.goto(origin)
  if (account) {
    await page.getByLabel('用户名', { exact: true }).fill(account.username)
    await page.getByLabel('密码', { exact: true }).fill(account.password)
    await page.getByRole('button', { name: '登录', exact: true }).click()
    await page.locator('[data-app-frame]').waitFor()
    await selectSpace(page, tenantId)
  }
  if (mobile) {
    await page.setViewportSize({ width: 390, height: 844 })
    await page.getByRole('button', { name: '打开侧边栏', exact: true }).click()
  }
  await page.getByRole('button', { name: '归档会话', exact: true }).click()
  const dialog = page.getByRole('dialog', { name: '归档会话', exact: true })
  await dialog.waitFor()
  return { context, page, dialog }
}

async function checkLayout(page, dialog, mobile, layouts) {
  const before = await dialog.locator('button').evaluateAll(buttons => buttons.map(button => {
    const bounds = button.getBoundingClientRect()
    return { label: button.textContent, width: bounds.width, height: bounds.height }
  }))
  await dialog.evaluate(async element => { await Promise.all(element.getAnimations().map(animation => animation.finished)) })
  const after = await dialog.locator('button').evaluateAll(buttons => buttons.map(button => {
    const bounds = button.getBoundingClientRect()
    return { label: button.textContent, width: bounds.width, height: bounds.height }
  }))
  layouts.push({ mobile, before, after })
  assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), 'page fits viewport')
  assert.ok(await dialog.evaluate(element => element.scrollWidth <= element.clientWidth), 'archive metadata fits viewport')
  const bounds = await dialog.boundingBox()
  assert.ok(bounds.x >= 0 && bounds.x + bounds.width <= page.viewportSize().width + 1)
  if (mobile) {
    for (const button of await dialog.locator('button:visible').all()) {
      const target = await button.boundingBox()
      assert.ok(target.width >= 40 && target.height >= 40, `mobile actions have touch targets: ${await button.textContent()} ${JSON.stringify(target)}`)
    }
  }
}

async function screenshot(page, name) {
  const artifacts = process.env.TERNILO_E2E_ARTIFACT_DIR
  if (!artifacts) return
  await mkdir(artifacts, { recursive: true })
  await page.screenshot({ path: path.join(artifacts, `${name}.png`), fullPage: true })
}

async function previewArchive(page, dialog, sessionId, expectedCount, network, name) {
  const previousWrites = network.filter(entry => entry.method !== 'GET').length
  const response = page.waitForResponse(value => new URL(value.url()).pathname === `/api/v1/sessions/${sessionId}/archive-history`)
  await dialog.locator(`[data-archived-session="${sessionId}"]`).getByRole('button', { name: '查看历史', exact: true }).click()
  const read = await response
  assert.equal(read.status(), 200)
  assert.equal(read.headers()['cache-control'], 'no-store')
  let result = await read.json()
  assert.equal(result.events.length, Math.min(200, expectedCount))
  let sequences = result.events.map(event => event.seq)
  const preview = dialog.locator(`[data-archive-preview="${sessionId}"]`)
  await preview.getByText(`已读取 ${sequences.length} 条历史事件。`, { exact: true }).waitFor()
  while (result.next_before_seq !== null) {
    const before = result.next_before_seq
    const olderResponse = page.waitForResponse(value => {
      const url = new URL(value.url())
      return url.pathname === `/api/v1/sessions/${sessionId}/archive-history` && url.searchParams.get('before_seq') === String(before)
    })
    await preview.getByRole('button', { name: '加载更早', exact: true }).click()
    const olderRead = await olderResponse
    assert.equal(olderRead.status(), 200)
    assert.equal(olderRead.headers()['cache-control'], 'no-store')
    result = await olderRead.json()
    assert.ok(result.events.length <= 200)
    assert.ok(result.events.at(-1).seq < before)
    sequences = [...result.events.map(event => event.seq), ...sequences]
    await preview.getByText(`已读取 ${sequences.length} 条历史事件。`, { exact: true }).waitFor()
  }
  assert.deepEqual(sequences, Array.from({ length: expectedCount }, (_, index) => index))
  assert.equal(await dialog.getByRole('button', { name: '恢复会话', exact: true }).count(), 0)
  assert.equal(await preview.locator('textarea, input').count(), 0)
  await preview.getByRole('button', { name: '原始事件', exact: true }).click()
  assert.equal(await preview.locator('[data-archive-event]').count(), Math.min(50, expectedCount))
  await preview.locator('[data-archive-event] summary').first().click()
  await preview.locator('[data-archive-event] pre').first().waitFor()
  assert.ok(await dialog.evaluate(element => element.scrollWidth <= element.clientWidth))
  await screenshot(page, `${name}-history`)
  assert.equal(network.filter(entry => entry.method !== 'GET').length, previousWrites, 'preview sends no mutation')
  await dialog.getByRole('button', { name: '返回归档列表', exact: true }).click()
}

test('archive restore preserves Local and Server Node sessions on desktop and mobile', { timeout: 180_000 }, async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-session-archive-'))
  const environment = { XDG_STATE_HOME: path.join(directory, 'state') }
  const processes = []
  const errors = []
  const network = []
  const cases = []
  const layouts = []
  let completed = false
  let browser
  try {
    const serverOrigin = `http://127.0.0.1:${await freePort()}`
    const server = await initializeServer({ directory: path.join(directory, 'server'), origin: serverOrigin, environment })
    processes.push(server)
    const adminSession = server.owner.session
    const admin = (resource, options = {}) => serverRequest(serverOrigin, resource, { token: adminSession.access_token, ...options })
    const registration = await admin('/admin/registration')
    await admin('/admin/registration', { method: 'PATCH', body: { mode: 'open', require_approval: false, revision: registration.revision } })
    const account = { username: 'archive-member', email: 'archive-member@example.test', password: 'archive-member-password' }
    const { session: member } = await serverRequest(serverOrigin, '/auth/register', { body: account })
    const { tenant } = await admin('/tenants', { body: { slug: 'archive-team', display_name: 'Archive team' } })
    const tenantId = tenant.tenant_id
    await admin(`/tenants/${tenantId}/members/${member.user.user_id}`, { method: 'PUT', body: { role: 'member' } })
    const owner = (resource, options = {}) => serverRequest(serverOrigin, resource, { token: member.access_token, tenantId, ...options })
    const [project] = (await owner('/projects')).projects
    const enrolled = await owner(`/tenants/${tenantId}/my-computer-enrollments`, { body: { name: 'archive-node', project_id: project.project_id, ttl_seconds: 600 } })
    const enrolledComputerId = enrolled.enrollment.executor_id
    const credential = await owner('/enrollments/consume', { body: { token: enrolled.enrollment.token } })
    const localOrigin = `http://127.0.0.1:${await freePort()}`
    const binary = process.env.TERNILO_E2E_NODE_BINARY ?? path.join(repository, 'target/debug/ternilo')
    const node = startProcess(binary, ['serve', '--listen', new URL(localOrigin).host, '--data-dir', path.join(directory, 'node'),
      '--node-id', enrolledComputerId, '--gateway-url', `${serverOrigin.replace('http:', 'ws:')}/api/v1/executors/connect`, '--allow-insecure-gateway'],
    { ...environment, TERNILO_LOCAL_TOKEN: credential.credential.token })
    processes.push(node)
    await waitForHttp(localOrigin, node)
    for (const origin of [serverOrigin, localOrigin]) {
      for (const asset of ['app.js', 'app.css']) {
        const response = await fetch(`${origin}/assets/${asset}`)
        assert.equal(response.status, 200)
        assert.equal(createHash('sha256').update(Buffer.from(await response.arrayBuffer())).digest('hex'), createHash('sha256').update(await readFile(path.join(repository, 'web/dist/assets', asset))).digest('hex'))
      }
    }
    const local = await localApi(localOrigin)
    const folder = path.join(directory, 'workspace')
    await mkdir(folder)
    const workspace = await local('/workspaces', { body: { path: folder } })
    const session = await local('/sessions', { body: { workspace_id: workspace.workspace_id } })
    const localId = session.identity.session_id
    await local(`/sessions/${localId}/turns`, { body: { input: '/code "archive-retained-history"' } })
    for (let index = 0; index < 75; index++) await local(`/sessions/${localId}/commands/feedback`, { body: { text: `Archived note ${index}` } })
    const state = await until(() => owner('/state'), value => value.sessions.length === 1, 'Node session discovered')
    const publicId = state.sessions[0].identity.session_id
    const events = await local(`/sessions/${localId}/events`)
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
    await owner(`/sessions/${publicId}/archive`, { method: 'POST' })
    assert.deepEqual(await admin('/sessions/archived', { tenantId }), [], 'administrator cannot list a private Node archive')
    await assert.rejects(() => admin(`/sessions/${publicId}/archive-events`, { tenantId }))
    await assert.rejects(() => admin(`/sessions/${publicId}/archive-history`, { tenantId }))
    await assert.rejects(() => admin(`/sessions/${publicId}/restore`, { tenantId, method: 'POST' }))
    const sharing = `/sessions/${publicId}/sharing/user/${adminSession.user.user_id}`
    await owner(sharing, { method: 'PUT', body: { view: true, submit: true, stop: true, configure: true } })
    await assert.rejects(() => admin(`/sessions/${publicId}/restore`, { tenantId, method: 'POST' }), /403/)
    const reader = await openArchive(browser, serverOrigin, server.owner, tenantId, true, errors, network)
    const sharedRow = reader.dialog.locator(`[data-archived-session="${publicId}"]`)
    await sharedRow.waitFor()
    assert.equal(await sharedRow.getByRole('button', { name: '恢复会话', exact: true }).isDisabled(), true)
    await checkLayout(reader.page, reader.dialog, true, layouts)
    await screenshot(reader.page, 'archive-shared-reader-mobile')
    await previewArchive(reader.page, reader.dialog, publicId, events.length, network, 'archive-shared-reader-mobile')
    cases.push({ surface: 'server-node-shared-reader', mobile: true, restoreDisabled: true })
    await reader.context.close()
    await owner(sharing, { method: 'DELETE' })
    assert.deepEqual(await admin('/sessions/archived', { tenantId }), [])
    await assert.rejects(() => admin(`/sessions/${publicId}/archive-events`, { tenantId }))
    await assert.rejects(() => admin(`/sessions/${publicId}/archive-history`, { tenantId }))
    await owner(`/sessions/${publicId}/restore`, { method: 'POST' })
    for (const platform of [false, true]) {
      for (const mobile of [false, true]) {
        const request = platform ? owner : local
        const sessionId = platform ? publicId : localId
        await request(`/sessions/${sessionId}/archive`, { method: 'POST' })
        const before = (await local('/sessions/archived')).find(record => record.identity.session_id === localId)
        const queue = await local(`/sessions/${localId}/queue`)
        const opened = await openArchive(browser, platform ? serverOrigin : localOrigin, platform ? account : null, tenantId, mobile, errors, network)
        const row = opened.dialog.locator(`[data-archived-session="${sessionId}"]`)
        await row.waitFor()
        await checkLayout(opened.page, opened.dialog, mobile, layouts)
        const artifactName = `archive-${platform ? 'server-node' : 'local'}-${mobile ? 'mobile' : 'desktop'}`
        await screenshot(opened.page, `${artifactName}-panel`)
        await previewArchive(opened.page, opened.dialog, sessionId, events.length, network, artifactName)
        if (mobile) {
          await opened.page.setViewportSize({ width: 320, height: 844 })
          await previewArchive(opened.page, opened.dialog, sessionId, events.length, network, `${artifactName}-320`)
          await opened.page.setViewportSize({ width: 390, height: 844 })
        }
        assert.deepEqual(await local(`/sessions/${localId}/queue`), queue)
        assert.deepEqual((await local('/sessions/archived')).find(record => record.identity.session_id === localId), before)
        const response = opened.page.waitForResponse(value => new URL(value.url()).pathname === `/api/v1/sessions/${sessionId}/restore`)
        await row.getByRole('button', { name: '恢复会话', exact: true }).click()
        assert.equal((await response).status(), 200)
        await row.waitFor({ state: 'hidden' })
        await opened.dialog.getByRole('status').filter({ hasText: '已恢复' }).waitFor()
        await opened.dialog.getByRole('button', { name: '关闭', exact: true }).click()
        await opened.page.locator(`[data-sidebar-session-row][data-session-id="${sessionId}"]`).waitFor()
        const after = (await local('/sessions')).find(record => record.identity.session_id === localId)
        assert.deepEqual(after, { ...before, archived_at_ms: null })
        assert.deepEqual(await local(`/sessions/${localId}/events`), events)
        assert.deepEqual(await local(`/sessions/${localId}/queue`), queue)
        assert.deepEqual(await request('/sessions/archived'), [])
        await assert.rejects(() => request(`/sessions/${sessionId}/archive-events`), /409/)
        await assert.rejects(() => request(`/sessions/${sessionId}/archive-history`), /409/)
        const duplicate = await request(`/sessions/${sessionId}/restore`, { method: 'POST' })
        assert.equal(duplicate.identity.session_id, sessionId)
        await screenshot(opened.page, `${artifactName}-restored`)
        cases.push({ surface: platform ? 'server-node' : 'local', mobile, restoreStatus: 200, retainedEvents: events.length, queueUnchanged: true, metadataUnchanged: true })
        await opened.context.close()
      }
    }
    await owner(`/sessions/${publicId}/archive`, { method: 'POST' })
    await owner(`/sessions/${publicId}`, { method: 'DELETE' })
    await assert.rejects(() => owner(`/sessions/${publicId}/restore`, { method: 'POST' }))
    await assert.rejects(() => local(`/sessions/${localId}/restore`, { method: 'POST' }))
    await assert.rejects(() => owner(`/sessions/${publicId}/archive-events`))
    await assert.rejects(() => local(`/sessions/${localId}/archive-events`))
    assert.deepEqual(await owner('/sessions/archived'), [])
    assert.deepEqual(await local('/sessions/archived'), [])
    assert.deepEqual(errors, [])
    completed = true
  } finally {
    const artifacts = process.env.TERNILO_E2E_ARTIFACT_DIR
    if (artifacts) {
      await mkdir(artifacts, { recursive: true })
      if (!completed && browser) {
        for (const [contextIndex, context] of browser.contexts().entries()) {
          for (const [pageIndex, page] of context.pages().entries()) {
            await screenshot(page, `failure-${contextIndex}-${pageIndex}`).catch(() => undefined)
          }
        }
      }
      await writeFile(path.join(artifacts, 'archive-results.json'), JSON.stringify({ completed, cases, errors, network, layouts }, null, 2))
    }
    await browser?.close()
    for (const process of processes.reverse()) await stopProcess(process)
    await rm(directory, { recursive: true, force: true })
  }
})
