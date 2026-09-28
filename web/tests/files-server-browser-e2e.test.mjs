import { selectChoice } from './browser-select-fixture.mjs'
import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { chromium } from 'playwright'
import { selectSpace, freePort, initializeServer, repository, serverRequest, startProcess, stopProcess, waitForHttp } from './platform-e2e-fixture.mjs'

const nodeBinary = process.env.TERNILO_E2E_NODE_BINARY ?? path.join(repository, 'target/debug/ternilo')

async function until(read, predicate, label) {
  const deadline = Date.now() + 30_000
  let value
  while (Date.now() < deadline) {
    value = await read()
    if (predicate(value)) return value
    await new Promise(resolve => setTimeout(resolve, 100))
  }
  throw new Error(`${label}: ${JSON.stringify(value)}`)
}

async function loginFiles(page, origin, credentials) {
  const document = await page.goto(`${origin}/files`)
  assert.equal(document.status(), 200, 'Server serves the Files page at its direct URL')
  await page.getByLabel('用户名', { exact: true }).fill(credentials.username)
  await page.getByLabel('密码', { exact: true }).fill(credentials.password)
  await page.getByRole('button', { name: '登录', exact: true }).click()
  await page.locator('[data-files-shell]').getByRole('heading', { name: '文件', exact: true }).waitFor()
  assert.equal(new URL(page.url()).pathname, '/files', 'native login retains the Files destination')
}

async function browserRequest(page, endpoint) {
  return page.evaluate(async endpoint => {
    const token = JSON.parse(sessionStorage.getItem('ternilo.native.session')).access_token
    const response = await fetch(`/api/v1${endpoint}`, { headers: {
      authorization: `Bearer ${token}`,
      'x-ternilo-tenant': localStorage.getItem('ternilo.current-tenant'),
    } })
    return { status: response.status, body: await response.json() }
  }, endpoint)
}

async function assertNoOverflow(page) {
  assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true, 'Files fits the viewport')
}

async function verifyAccountId(page, origin, userId, artifacts) {
  await page.context().grantPermissions(['clipboard-read', 'clipboard-write'], { origin })
  await page.goto(`${origin}/settings/general`)
  const account = page.locator('[data-account-settings]')
  await account.locator('[data-account-id]').waitFor()
  assert.equal(await account.locator('[data-account-id]').textContent(), userId)
  await account.getByText('账号 ID', { exact: true }).waitFor()
  await account.getByRole('button', { name: '复制账号 ID', exact: true }).click()
  await account.getByRole('status').getByText('已复制账号 ID', { exact: true }).waitFor()
  assert.equal(await page.evaluate(() => navigator.clipboard.readText()), userId, 'clipboard contains the stable platform account ID')
  await account.screenshot({ path: path.join(artifacts, 'account-id-server.png') })
  await page.setViewportSize({ width: 390, height: 844 })
  await assertNoOverflow(page)
  await account.screenshot({ path: path.join(artifacts, 'account-id-server-mobile.png') })
  await selectChoice(page.getByRole('combobox', { name: '语言', exact: true }), 'en')
  await account.getByText('Account ID', { exact: true }).waitFor()
  await account.getByRole('button', { name: 'Copy account ID', exact: true }).click()
  await account.getByRole('status').getByText('Account ID copied', { exact: true }).waitFor()
  assert.equal(await page.evaluate(() => navigator.clipboard.readText()), userId)
  await account.screenshot({ path: path.join(artifacts, 'account-id-server-en.png') })
  await selectChoice(page.getByRole('combobox', { name: 'Language', exact: true }), 'zh')
  await page.reload()
  await account.locator('[data-account-id]').waitFor()
  assert.equal(await account.locator('[data-account-id]').textContent(), userId, 'account ID survives a page reload')
  await page.setViewportSize({ width: 1440, height: 1000 })
  await page.goto(`${origin}/files`)
  await page.locator('[data-files-shell]').waitFor()
}

test('native Server Files preserves Node versions, enforces ownership and explains offline content', { timeout: 180_000 }, async t => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-files-server-browser-'))
  const artifacts = process.env.TERNILO_E2E_ARTIFACT_DIR ?? directory
  await mkdir(artifacts, { recursive: true })
  const observations = { assets: {}, responses: [], failedRequests: [], consoleErrors: [], pageErrors: [], expectedErrors: [] }
  const expectedErrors = new Map()
  let application, node, browser, owner, member
  const observe = (page, actor) => {
    page.on('pageerror', error => observations.pageErrors.push({ actor, message: error.message }))
    page.on('requestfailed', request => {
      if (request.failure()?.errorText === 'net::ERR_ABORTED') return
      observations.failedRequests.push({ actor, path: new URL(request.url()).pathname, error: request.failure()?.errorText })
    })
    page.on('response', response => {
      const url = new URL(response.url())
      if (url.pathname.startsWith('/api/')) observations.responses.push({ actor, method: response.request().method(), path: url.pathname, status: response.status() })
      if (response.status() < 400) return
      const record = { actor, path: url.pathname, status: response.status() }
      if (expectedErrors.get(`${actor}:${url.pathname}`)?.has(response.status())) observations.expectedErrors.push(record)
      else observations.failedRequests.push(record)
    })
    page.on('console', message => {
      if (message.type() !== 'error') return
      const location = message.location().url
      const status = Number(message.text().match(/\b(403|503)\b/)?.[1])
      if (location && expectedErrors.get(`${actor}:${new URL(location).pathname}`)?.has(status)) return
      observations.consoleErrors.push({ actor, message: message.text(), location })
    })
  }
  try {
    const origin = `http://127.0.0.1:${await freePort()}`
    application = await initializeServer({ directory: path.join(directory, 'server'), origin, mode: 'multi_user' })
    for (const asset of ['app.js', 'app.css']) {
      const response = await fetch(`${origin}/assets/${asset}`)
      assert.equal(response.status, 200)
      const served = createHash('sha256').update(Buffer.from(await response.arrayBuffer())).digest('hex')
      const expected = createHash('sha256').update(await readFile(path.join(repository, 'web/dist/assets', asset))).digest('hex')
      observations.assets[asset] = { served, expected }
      assert.equal(served, expected, `Server embeds the current ${asset}`)
    }
    const token = application.owner.session.access_token
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
    owner = await (await browser.newContext({ viewport: { width: 1440, height: 1000 }, serviceWorkers: 'block' })).newPage()
    observe(owner, 'owner')
    await loginFiles(owner, origin, application.owner)
    await owner.locator('[data-files-empty]').getByText('没有符合条件的文件。', { exact: true }).waitFor()
    assert.equal(await owner.locator('[data-files-offline]').count(), 0)
    await owner.screenshot({ path: path.join(artifacts, 'files-server-empty.png'), fullPage: true })
    await verifyAccountId(owner, origin, application.owner.session.user.user_id, artifacts)

    const team = (await serverRequest(origin, '/tenants', { token, body: { slug: 'files-team', display_name: 'Files team' } })).tenant
    const tenantId = team.tenant_id
    const project = (await serverRequest(origin, '/projects', { token, tenantId })).projects[0]
    const nodeId = 'files-browser-node'
    const enrollment = (await serverRequest(origin, `/tenants/${tenantId}/my-computer-enrollments`, { token, body: { executor_id: nodeId, project_id: project.project_id, ttl_seconds: 600 } })).enrollment
    const credential = (await serverRequest(origin, '/enrollments/consume', { body: { token: enrollment.token } })).credential
    const nodeOrigin = `http://127.0.0.1:${await freePort()}`
    const nodeEnvironment = Object.fromEntries(Object.keys(process.env).filter(key => key.startsWith('TERNILO_')).map(key => [key, undefined]))
    node = startProcess(nodeBinary, ['serve', '--listen', new URL(nodeOrigin).host, '--data-dir', path.join(directory, 'node'), '--gateway-url', `${origin.replace('http:', 'ws:')}/api/v1/executors/connect`, '--allow-insecure-gateway', '--node-id', nodeId], { ...nodeEnvironment, TERNILO_LOCAL_TOKEN: credential.token })
    await waitForHttp(nodeOrigin, node)
    const html = await (await fetch(nodeOrigin)).text()
    const encodedBoot = html.match(/window\.__TERNILO_BOOT__\s*=\s*(\{[^<]+\});/)
    assert.ok(encodedBoot, 'Node exposes its authenticated API bootstrap')
    const nodeToken = JSON.parse(encodedBoot[1]).apiToken
    const workspacePath = path.join(directory, 'workspace')
    await mkdir(workspacePath)
    const workspace = await serverRequest(nodeOrigin, '/workspaces', { token: nodeToken, body: { path: workspacePath } })
    const session = await serverRequest(nodeOrigin, '/sessions', { token: nodeToken, body: { workspace_id: workspace.workspace_id } })
    const fileName = 'server-proof.txt'
    const versions = ['server-files-first-version', 'server-files-second-version']
    for (const contents of versions) {
      await serverRequest(nodeOrigin, `/sessions/${session.identity.session_id}/queue`, { token: nodeToken, body: { content: { kind: 'prompt', input: `/write ${fileName} ${contents}` } } })
      await until(() => readFile(path.join(workspacePath, fileName), 'utf8').catch(() => ''), value => value === contents, 'Node writes the file')
      await until(() => serverRequest(nodeOrigin, `/sessions/${session.identity.session_id}/queue`, { token: nodeToken }), queue => !queue.active_run_id && !queue.items.some(item => item.placement === 'queued'), 'Node finishes the write')
    }
    await until(() => serverRequest(origin, '/state', { token, tenantId }), state => state.sessions.length === 1, 'Server discovers the Node history')
    const inventory = await until(() => serverRequest(origin, '/files', { token, tenantId }), page => page.items.length === 2, 'Server caches both Node file versions')
    assert.deepEqual(inventory.offline_sources, [])
    assert.equal(inventory.items.every(file => file.source_status === 'online' && file.kind === 'generated' && file.name === fileName), true)
    const [newest, oldest] = inventory.items
    assert.notEqual(newest.id, oldest.id, 'each generated version has its own file ID')
    assert.notEqual(oldest.session_id, session.identity.session_id, 'Server routes its mapped session ID to the Node')
    await owner.reload()
    await selectSpace(owner, tenantId)
    await until(() => owner.locator('[data-file-id]').count(), count => count === 2, 'owner sees both generated versions')
    const identityAfterSpaceChange = await browserRequest(owner, '/auth/session')
    assert.equal(identityAfterSpaceChange.body.user.user_id, application.owner.session.user.user_id, 'switching spaces keeps the same account ID')
    assert.equal(await owner.locator('[data-files-offline]').count(), 0)
    await assertNoOverflow(owner)
    await owner.screenshot({ path: path.join(artifacts, 'files-server-online.png'), fullPage: true })
    const oldRow = owner.locator(`[data-file-id="${oldest.id}"]`)
    await oldRow.getByRole('button', { name: `预览 ${fileName}`, exact: true }).click()
    const preview = owner.getByRole('dialog', { name: fileName, exact: true })
    await preview.getByLabel('文件文本内容', { exact: true }).waitFor()
    assert.equal(await preview.getByLabel('文件文本内容', { exact: true }).textContent(), versions[0], 'older preview retains its original bytes')
    const [download] = await Promise.all([
      owner.waitForEvent('download'),
      preview.getByRole('button', { name: '下载', exact: true }).click(),
    ])
    assert.equal(download.suggestedFilename(), fileName)
    const downloadStream = await download.createReadStream()
    const chunks = []
    for await (const chunk of downloadStream) chunks.push(chunk)
    assert.equal(Buffer.concat(chunks).toString('utf8'), versions[0], 'download serves the selected version')
    await owner.screenshot({ path: path.join(artifacts, 'files-server-preview.png'), fullPage: true })
    await preview.getByRole('button', { name: '关闭预览', exact: true }).click()
    await owner.setViewportSize({ width: 390, height: 844 })
    await assertNoOverflow(owner)
    await owner.screenshot({ path: path.join(artifacts, 'files-server-mobile.png'), fullPage: true })
    await owner.setViewportSize({ width: 1440, height: 1000 })

    const memberCredentials = { username: 'files-member', password: 'files-member-test-password' }
    const accountInvitation = await serverRequest(origin, '/admin/invitations', { token, body: { tenant_id: null, role: 'member' } })
    const memberSession = await serverRequest(origin, '/auth/invitations/accept', { body: { token: accountInvitation.token, email: 'files-member@example.test', ...memberCredentials } })
    const invitation = await serverRequest(origin, '/admin/invitations', { token, body: { tenant_id: tenantId, role: 'member' } })
    await serverRequest(origin, '/invitations/accept', { token: memberSession.access_token, body: { token: invitation.token } })
    member = await (await browser.newContext({ viewport: { width: 1280, height: 900 }, serviceWorkers: 'block' })).newPage()
    observe(member, 'member')
    await loginFiles(member, origin, memberCredentials)
    await selectSpace(member, tenantId)
    await member.locator('[data-files-empty]').getByText('没有符合条件的文件。', { exact: true }).waitFor()
    const memberInventory = await browserRequest(member, `/files?session_id=${encodeURIComponent(oldest.session_id)}`)
    assert.equal(memberInventory.status, 200)
    assert.deepEqual(memberInventory.body.items, [], 'a private session filter does not disclose file metadata')
    const contentPath = `/api/v1/sessions/${encodeURIComponent(oldest.session_id)}/files/${encodeURIComponent(oldest.id)}/content`
    expectedErrors.set(`member:${contentPath}`, new Set([403]))
    const denied = await browserRequest(member, contentPath.slice('/api/v1'.length))
    assert.equal(denied.status, 403, 'guessing another member\'s file ID is forbidden')
    assert.equal(denied.body.error.code, 'policy_denied')
    assert.equal(await member.locator('[data-file-id]').count(), 0)
    await member.screenshot({ path: path.join(artifacts, 'files-server-member-empty.png'), fullPage: true })

    await stopProcess(node)
    await until(() => serverRequest(origin, '/files', { token, tenantId }), page => page.items.length === 2 && page.items.every(file => file.source_status === 'offline'), 'Server marks the stopped Node offline')
    await owner.getByRole('button', { name: '刷新', exact: true }).click()
    await owner.locator('[data-files-offline]').getByText('部分文件来源离线，当前列表可能不完整。重新连接后刷新查看。', { exact: true }).waitFor()
    assert.equal(await owner.locator('[data-file-id]').count(), 2, 'cached file metadata remains visible offline')
    assert.equal(await owner.getByText('来源离线', { exact: true }).count(), 2)
    expectedErrors.set(`owner:${contentPath}`, new Set([503]))
    await oldRow.getByRole('button', { name: `预览 ${fileName}`, exact: true }).click()
    await preview.getByRole('alert').getByText('无法预览此文件', { exact: true }).waitFor()
    assert.match(await preview.getByRole('alert').textContent(), /node is offline/i)
    await owner.screenshot({ path: path.join(artifacts, 'files-server-offline-preview.png'), fullPage: true })
    await preview.getByRole('button', { name: '关闭预览', exact: true }).click()
    await oldRow.getByRole('button', { name: `下载 ${fileName}`, exact: true }).click()
    await oldRow.getByRole('alert').waitFor()
    assert.match(await oldRow.getByRole('alert').textContent(), /下载失败.*node is offline/i)
    await owner.screenshot({ path: path.join(artifacts, 'files-server-offline.png'), fullPage: true })
    await owner.getByRole('searchbox', { name: '搜索文件名', exact: true }).fill('absent-file')
    await owner.getByRole('button', { name: '搜索', exact: true }).click()
    await owner.locator('[data-files-empty]').getByText('当前已同步的记录中没有符合条件的文件。', { exact: true }).waitFor()
    assert.equal(await owner.locator('[data-files-offline]').isVisible(), true)
    assert.equal(await owner.getByText('在会话中上传附件或生成文件后，会显示在这里。', { exact: true }).count(), 0)
    await owner.screenshot({ path: path.join(artifacts, 'files-server-offline-empty.png'), fullPage: true })
    assert.equal(observations.expectedErrors.some(error => error.actor === 'member' && error.status === 403), true)
    assert.equal(observations.expectedErrors.filter(error => error.actor === 'owner' && error.status === 503).length, 2)
    assert.deepEqual(observations.pageErrors, [])
    assert.deepEqual(observations.consoleErrors, [])
    assert.deepEqual(observations.failedRequests, [])
  } catch (error) {
    if (owner) await owner.screenshot({ path: path.join(artifacts, 'files-server-failure.png'), fullPage: true }).catch(() => {})
    t.diagnostic(application?.diagnostics() ?? '')
    t.diagnostic(node?.diagnostics() ?? '')
    throw error
  } finally {
    await writeFile(path.join(artifacts, 'files-server-observations.json'), `${JSON.stringify(observations, null, 2)}\n`)
    await browser?.close()
    await stopProcess(node)
    await stopProcess(application)
    await rm(directory, { recursive: true, force: true })
  }
})
