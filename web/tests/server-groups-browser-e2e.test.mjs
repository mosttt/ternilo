import { selectChoice } from './browser-select-fixture.mjs'
import assert from 'node:assert/strict'
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { createHash } from 'node:crypto'
import { createServer } from 'node:http'
import path from 'node:path'
import { tmpdir } from 'node:os'
import test from 'node:test'
import { chromium } from 'playwright'
import { selectSpace, freePort, initializeServer, repository, registerOidcUser, serverRequest, startOidcServer, startProcess, stopProcess, waitForHttp } from './platform-e2e-fixture.mjs'

async function until(read, predicate, label) {
  const deadline = Date.now() + 30_000
  while (Date.now() < deadline) {
    const value = await read()
    if (predicate(value)) return value
    await new Promise(resolve => setTimeout(resolve, 100))
  }
  throw new Error(`Timed out: ${label}`)
}

async function modelFixture() {
  const records = []
  const server = createServer((request, response) => {
    if (request.method !== 'POST' || request.url !== '/v1/responses') {
      response.writeHead(404); response.end(); return
    }
    const chunks = []
    request.on('data', chunk => chunks.push(chunk))
    request.on('end', () => {
      const body = JSON.parse(Buffer.concat(chunks).toString())
      records.push(body)
      const user = body.input?.filter(item => item.role === 'user').at(-1)
      const input = user?.content?.filter(item => item.type === 'input_text').map(item => item.text).join('\n') ?? ''
      const text = body.instructions?.includes('You name software-agent conversations') ? 'Group collaboration'
        : input.includes('B2 group task') ? 'Group fixture: B2 group task' : 'Group fixture: Seed group history'
      const sse = event => `data: ${JSON.stringify(event)}\n\n`
      response.writeHead(200, { 'content-type': 'text/event-stream' })
      response.end(sse({ type: 'response.output_text.delta', output_index: 0, content_index: 0, delta: text })
        + sse({ type: 'response.completed', response: { status: 'completed', output: [{ type: 'message', role: 'assistant', content: [{ type: 'output_text', text }] }], usage: { input_tokens: 50, output_tokens: 15 } } }))
    })
  })
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve))
  return { baseUrl: `http://127.0.0.1:${server.address().port}/v1`, records, close: () => new Promise(resolve => server.close(resolve)) }
}

async function login(page, origin, owner) {
  await page.goto(origin)
  await page.getByLabel('用户名', { exact: true }).fill(owner.username)
  await page.getByLabel('密码', { exact: true }).fill(owner.password)
  await page.getByRole('button', { name: '登录', exact: true }).click()
  await page.getByRole('combobox', { name: '切换空间', exact: true }).waitFor()
}

async function sessionMenu(page, id, label) {
  const row = page.locator(`[data-sidebar-session-row][data-session-id="${id}"]`)
  await row.hover()
  await row.getByRole('button', { name: /的操作$/ }).click()
  await page.getByRole('menuitem', { name: label, exact: true }).click()
}

async function groupEditor(page, origin, id) {
  await page.goto(`${origin}/spaces/current`)
  await page.getByRole('tab', { name: '权限组', exact: true }).click()
  await page.locator(`[data-platform-group="${id}"]`).getByRole('button', { name: '管理权限组', exact: true }).click()
  return page.getByRole('dialog', { name: '管理权限组', exact: true })
}

async function changeGroupMember(editor, user, adding) {
  const members = editor.locator('[data-group-members]')
  const page = editor.page()
  if (adding) await members.getByRole('button', { name: '添加组员', exact: true }).click()
  await members.getByLabel('搜索成员', { exact: true }).fill(user.user.username)
  const [searched] = await Promise.all([
    page.waitForResponse(response => {
      const url = new URL(response.url())
      return response.request().method() === 'GET' && url.pathname.endsWith('/members') && url.searchParams.get('query') === user.user.username
    }),
    members.getByRole('button', { name: '搜索', exact: true }).click(),
  ])
  assert.equal(searched.status(), 200)
  const row = members.locator(`[data-group-member="${user.user.user_id}"]`)
  const [mutated] = await Promise.all([
    page.waitForResponse(response => response.request().method() === (adding ? 'PUT' : 'DELETE')
      && new URL(response.url()).pathname.endsWith(`/members/${encodeURIComponent(user.user.user_id)}`)),
    row.getByRole('button').click(),
  ])
  assert.equal(mutated.status(), 204)
  if (adding) await row.getByText('已加入', { exact: true }).waitFor()
  else await row.waitFor({ state: 'detached' })
  await until(() => members.getByRole('button', { name: adding ? '返回组员列表' : '添加组员', exact: true }).isEnabled(), Boolean, 'group mutation finished')
}

async function assertLayout(page, locator) {
  // Viewport emulation can return before the application's resize handler runs.
  await page.waitForFunction(() => Number.parseFloat(document.documentElement.style.getPropertyValue('--ternilo-visual-viewport-height'))
    === Math.round(window.visualViewport?.height ?? window.innerHeight))
  await locator.evaluate(async element => {
    await Promise.all(element.getAnimations({ subtree: true })
      .filter(animation => animation.effect?.getTiming().iterations !== Infinity)
      .map(animation => animation.finished.catch(() => {})))
  })
  assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), 'document does not overflow horizontally')
  assert.ok(await locator.evaluate(element => element.scrollWidth <= element.clientWidth), 'dialog does not overflow horizontally')
  const bounds = await locator.boundingBox()
  assert.ok(bounds && bounds.x >= 0 && bounds.y >= 0 && bounds.x + bounds.width <= page.viewportSize().width + 1 && bounds.y + bounds.height <= page.viewportSize().height + 1, 'dialog remains inside the viewport')
  for (const button of await locator.getByRole('button').all()) {
    const size = await button.boundingBox()
    if (size) assert.ok(size.height >= 39.9 && size.width >= 39.9, `touch target: ${await button.getAttribute('aria-label') ?? await button.innerText()} (${size.width} × ${size.height})`)
  }
}

test('team groups authorize real Node collaboration and revoke derived fork access', { timeout: 360_000 }, async () => {
  const groupName = 'Reviewers / international-collaboration-maintainers-with-a-long-unbroken-group-name'
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-groups-browser-'))
  const artifacts = process.env.TERNILO_E2E_ARTIFACT_DIR ?? directory
  await mkdir(artifacts, { recursive: true })
  const identities = Object.fromEntries(Array.from({ length: 28 }, (_, index) => {
    const id = String(index).padStart(2, '0')
    return [id, { subject: `groups-${id}`, email: `groups-${id}@example.test`, name: `B2 Member ${id}` }]
  }))
  const oidc = await startOidcServer({ audience: 'groups-browser', identities, initialIdentity: '00' })
  const model = await modelFixture()
  let application, node, browser, owner, memberPage
  const errors = [], expectedRevocation = []
  const assetHashes = {}
  let revoked = false, sessionId, childId
  try {
    const origin = `http://127.0.0.1:${await freePort()}`
    application = await initializeServer({ directory: path.join(directory, 'server'), origin,
      oidc: { issuer: oidc.issuer, audience: 'groups-browser', client_id: 'groups-browser' }, mode: 'multi_user' })
    const token = application.owner.session.access_token
    const registrationPolicy = await serverRequest(origin, '/admin/registration', { token: token })
    await serverRequest(origin, '/admin/registration', { token: token, method: 'PATCH', body: { mode: 'open', require_approval: false, revision: registrationPolicy.revision } })
    const team = (await serverRequest(origin, '/tenants', { token, body: { slug: 'groups-team', display_name: 'Group collaboration' } })).tenant
    const members = []
    for (const key of Object.keys(identities).sort()) {
      const identity = await registerOidcUser(origin, oidc.accessToken(key), `groups-${key}`)
      members.push(identity)
      await serverRequest(origin, `/tenants/${team.tenant_id}/members/${identity.user.user_id}`, { token, method: 'PUT', body: { role: 'member' } })
    }
    const project = (await serverRequest(origin, '/projects', { token, tenantId: team.tenant_id })).projects[0]
    const enrollment = (await serverRequest(origin, `/tenants/${team.tenant_id}/my-computer-enrollments`, { token, body: { executor_id: 'groups-node', project_id: project.project_id, ttl_seconds: 600 } })).enrollment
    const credential = (await serverRequest(origin, '/enrollments/consume', { body: { token: enrollment.token } })).credential
    const nodeOrigin = `http://127.0.0.1:${await freePort()}`
    node = startProcess(path.join(repository, 'target/debug/ternilo'), ['serve', '--listen', new URL(nodeOrigin).host, '--data-dir', path.join(directory, 'node'), '--gateway-url', `${origin.replace('http:', 'ws:')}/api/v1/executors/connect`, '--allow-insecure-gateway', '--node-id', 'groups-node'], { TERNILO_LOCAL_TOKEN: credential.token })
    await waitForHttp(nodeOrigin, node)
    for (const [endpoint, baseUrl] of Object.entries({ server: origin, local: nodeOrigin })) {
      assetHashes[endpoint] = {}
      for (const asset of ['app.js', 'app.css']) {
        const response = await fetch(`${baseUrl}/assets/${asset}`)
        assert.equal(response.status, 200)
        const served = createHash('sha256').update(Buffer.from(await response.arrayBuffer())).digest('hex')
        const expected = createHash('sha256').update(await readFile(path.join(repository, 'web/dist/assets', asset))).digest('hex')
        assert.equal(served, expected, `${endpoint} embeds current ${asset}`)
        assetHashes[endpoint][asset] = served
      }
    }
    const html = await (await fetch(nodeOrigin)).text()
    const encodedBoot = html.match(/window\.__TERNILO_BOOT__\s*=\s*(\{[^<]+\});/)
    assert.ok(encodedBoot, 'Local page exposes its API bootstrap')
    const nodeToken = JSON.parse(encodedBoot[1]).apiToken
    const workspacePath = path.join(directory, 'workspace')
    await mkdir(workspacePath)
    const workspace = await serverRequest(nodeOrigin, '/workspaces', { token: nodeToken, body: { path: workspacePath } })
    const session = await serverRequest(nodeOrigin, '/sessions', { token: nodeToken, body: { workspace_id: workspace.workspace_id } })
    await serverRequest(nodeOrigin, '/credentials', { token: nodeToken, body: { name: 'GROUP_MODEL_KEY', value: 'groups-fixture-key' } })
    await serverRequest(nodeOrigin, '/providers', { token: nodeToken, body: {
      id: 'groups-fixture', display_name: 'Group model', base_url: model.baseUrl, protocol: 'openai-responses', api_key_ref: 'GROUP_MODEL_KEY',
      defaults: { context_window: 128000, max_output_tokens: 4096 }, models: [{ id: 'groups-model', settings: { mode: 'inherit' } }],
      timeout_ms: 30000, max_attempts: 1, retry_base_delay_ms: 50,
    } })
    await serverRequest(nodeOrigin, `/sessions/${session.identity.session_id}`, { token: nodeToken, method: 'PATCH', body: { model: { provider: 'named_provider', provider_id: 'groups-fixture', model: 'groups-model' } } })
    await serverRequest(nodeOrigin, `/sessions/${session.identity.session_id}/queue`, { token: nodeToken, body: { content: { kind: 'prompt', input: 'Seed group history' } } })
    await until(() => serverRequest(nodeOrigin, `/sessions/${session.identity.session_id}/queue`, { token: nodeToken }), queue => !queue.active_run_id && !queue.items.some(item => item.placement === 'queued'), 'seed turn completion')
    const state = await until(() => serverRequest(origin, '/state', { token, tenantId: team.tenant_id }), state => state.sessions.length === 1, 'Node history discovery')
    sessionId = state.sessions[0].identity.session_id
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
    owner = await (await browser.newContext({ viewport: { width: 1440, height: 1000 }, serviceWorkers: 'block' })).newPage()
    memberPage = await (await browser.newContext({ viewport: { width: 1280, height: 900 }, serviceWorkers: 'block' })).newPage()
    const expectedRevokedResponse = (url, status) => revoked && [400, 403, 404].includes(status)
      && [sessionId, childId].some(id => id && url.pathname.startsWith(`/api/v1/sessions/${encodeURIComponent(id)}/`))
    for (const page of [owner, memberPage]) {
      page.on('pageerror', error => errors.push(`page: ${error.message}`))
      page.on('console', message => {
        if (message.type() !== 'error') return
        const location = message.location().url
        const status = Number(message.text().match(/\b(400|403|404)\b/)?.[1])
        if (location && expectedRevokedResponse(new URL(location), status)) return
        errors.push(`console: ${message.text()}`)
      })
      page.on('response', response => {
        if (response.status() < 400) return
        const url = new URL(response.url())
        if (expectedRevokedResponse(url, response.status())) expectedRevocation.push(url.pathname)
        else errors.push(`HTTP ${response.status()}: ${url.pathname}`)
      })
    }
    await login(owner, origin, application.owner)
    await selectSpace(owner, team.tenant_id)
    await owner.goto(`${origin}/spaces/current`)
    await until(() => owner.locator('[data-platform-member]').count(), count => count === 25, 'first member page')
    const firstMembers = await owner.locator('[data-platform-member]').evaluateAll(rows => rows.map(row => row.dataset.platformMember))
    assert.equal(firstMembers.length, 25)
    await owner.locator('[data-platform-members]').getByRole('button', { name: '下一页', exact: true }).click()
    await until(() => owner.locator('[data-platform-member]').count(), count => count === 4, 'member second page')
    const lastMembers = await owner.locator('[data-platform-member]').evaluateAll(rows => rows.map(row => row.dataset.platformMember))
    assert.equal(new Set([...firstMembers, ...lastMembers]).size, 29)
    await owner.getByRole('tab', { name: '权限组', exact: true }).click()
    await owner.getByRole('button', { name: '新建权限组', exact: true }).click()
    let editor = owner.getByRole('dialog', { name: '新建权限组', exact: true })
    await editor.getByLabel('名称', { exact: true }).fill(groupName)
    await editor.getByLabel('说明（可选）', { exact: true }).fill('Collaborate on the selected conversation')
    const created = owner.waitForResponse(response => response.request().method() === 'POST' && new URL(response.url()).pathname === `/api/v1/tenants/${team.tenant_id}/groups`)
    await editor.getByRole('button', { name: '新建权限组', exact: true }).click()
    const group = await (await created).json()
    editor = owner.getByRole('dialog', { name: '管理权限组', exact: true })
    await changeGroupMember(editor, members[0], true)
    await editor.locator('[data-slot="dialog-close"]').click()
    await editor.waitFor({ state: 'hidden' })
    await owner.goto(origin)
    await sessionMenu(owner, sessionId, '共享…')
    let sharing = owner.getByRole('dialog', { name: '共享会话', exact: true })
    await selectChoice(sharing.locator('#sharing-kind'), 'group')
    await sharing.locator('[data-sharing-candidate]').filter({ hasText: 'Reviewers' }).click()
    await sharing.getByLabel('发送任务', { exact: true }).check()
    await sharing.getByRole('button', { name: '保存共享权限', exact: true }).click()
    await sharing.locator(`[data-sharing-grant="group:${group.group_id}"]`).waitFor()
    for (const viewport of [{ width: 390, height: 844 }, { width: 320, height: 740 }, { width: 844, height: 390 }]) {
      await owner.setViewportSize(viewport)
      await assertLayout(owner, sharing)
      await owner.screenshot({ path: path.join(artifacts, `group-sharing-${viewport.width}.png`), animations: 'disabled' })
    }
    await sharing.locator('[data-slot="dialog-close"]').click()
    await owner.setViewportSize({ width: 1440, height: 1000 })

    oidc.selectIdentity('00')
    await memberPage.goto(origin)
    await memberPage.getByRole('button', { name: '使用组织账号登录', exact: true }).click()
    await selectSpace(memberPage, team.tenant_id)
    await memberPage.locator(`[data-sidebar-session-row][data-session-id="${sessionId}"]`).click()
    await sessionMenu(memberPage, sessionId, '查看权限…')
    let permissions = memberPage.getByRole('dialog', { name: '共享会话', exact: true })
    await permissions.getByText('Reviewers', { exact: false }).first().waitFor()
    assert.equal(await permissions.locator('[data-sharing-candidates]').count(), 0)
    assert.equal(await permissions.getByRole('button', { name: '保存共享权限', exact: true }).count(), 0)
    await permissions.locator('[data-slot="dialog-close"]').click()
    const input = memberPage.getByRole('textbox', { name: '输入任务', exact: true })
    await input.fill('B2 group task')
    await memberPage.getByRole('button', { name: '发送', exact: true }).click()
    await memberPage.getByText('Group fixture: B2 group task', { exact: true }).waitFor()
    const forked = memberPage.waitForResponse(response => response.request().method() === 'POST' && new URL(response.url()).pathname === `/api/v1/sessions/${sessionId}/fork`)
    await sessionMenu(memberPage, sessionId, '分叉会话')
    childId = (await (await forked).json()).identity.session_id
    assert.ok(childId && childId !== sessionId)
    await memberPage.locator(`[data-sidebar-session-row][data-session-id="${childId}"]`).waitFor()
    editor = await groupEditor(owner, origin, group.group_id)
    await changeGroupMember(editor, members[1], true)
    await editor.locator('[data-slot="dialog-close"]').click()
    const peerState = await serverRequest(origin, '/state', { token: oidc.accessToken('01'), tenantId: team.tenant_id })
    assert.ok(peerState.sessions.some(session => session.identity.session_id === sessionId))
    assert.ok(!peerState.sessions.some(session => session.identity.session_id === childId))
    editor = await groupEditor(owner, origin, group.group_id)
    for (const viewport of [{ width: 390, height: 844 }, { width: 320, height: 740 }, { width: 844, height: 390 }]) {
      await owner.setViewportSize(viewport)
      await assertLayout(owner, editor)
      await owner.screenshot({ path: path.join(artifacts, `group-editor-${viewport.width}.png`), animations: 'disabled' })
    }
    revoked = true
    await changeGroupMember(editor, members[0], false)
    await memberPage.locator(`[data-sidebar-session-row][data-session-id="${childId}"]`).waitFor({ state: 'detached' })
    const revokedState = await serverRequest(origin, '/state', { token: oidc.accessToken('00'), tenantId: team.tenant_id })
    assert.equal(revokedState.sessions.length, 0)
    await editor.locator('[data-slot="dialog-close"]').click()
    await serverRequest(origin, `/sessions/${sessionId}/sharing/user/${members[0].user.user_id}`, { token, tenantId: team.tenant_id, method: 'PUT', body: { view: true, submit: false, stop: false, configure: false } })
    await memberPage.locator(`[data-sidebar-session-row][data-session-id="${sessionId}"]`).waitFor()
    await memberPage.locator(`[data-sidebar-session-row][data-session-id="${sessionId}"]`).click()
    await memberPage.getByText('你可以查看内容，当前共享权限不允许发送任务。', { exact: true }).waitFor()
    await owner.setViewportSize({ width: 1440, height: 1000 })
    await owner.locator(`[data-platform-group="${group.group_id}"]`).getByRole('button', { name: `删除权限组“${groupName}”`, exact: true }).click()
    await owner.getByRole('dialog', { name: /删除权限组/ }).getByRole('button', { name: '删除权限组', exact: true }).click()
    const remaining = await serverRequest(origin, '/state', { token: oidc.accessToken('00'), tenantId: team.tenant_id })
    assert.deepEqual(remaining.sessions.map(session => session.identity.session_id), [sessionId])
    await until(() => serverRequest(origin, '/state', { token: oidc.accessToken('01'), tenantId: team.tenant_id }), state => state.sessions.length === 0, 'peer group revocation')
    assert.ok(model.records.some(record => JSON.stringify(record.input).includes('B2 group task')))
    assert.deepEqual(errors, [])
    await writeFile(path.join(artifacts, 'groups-browser-result.json'), JSON.stringify({ passed: true, assetHashes, members: members.length, firstPage: firstMembers.length, secondPage: lastMembers.length, modelRequests: model.records.length, errors, expectedRevocation }, null, 2))
  } catch (error) {
    await owner?.screenshot({ path: path.join(artifacts, 'groups-owner-failure.png'), animations: 'disabled' }).catch(() => {})
    await memberPage?.screenshot({ path: path.join(artifacts, 'groups-member-failure.png'), animations: 'disabled' }).catch(() => {})
    console.error({ artifacts, errors, expectedRevocation, server: application?.diagnostics(), node: node?.diagnostics() })
    throw error
  } finally {
    await browser?.close()
    if (node) await stopProcess(node)
    if (application) await stopProcess(application)
    await model.close()
    await oidc.close()
    await rm(directory, { recursive: true, force: true })
  }
})
