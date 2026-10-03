import { selectChoice } from './browser-select-fixture.mjs'
import assert from 'node:assert/strict'
import { spawn, execFile } from 'node:child_process'
import { cp, mkdir, mkdtemp, readFile, readdir, rm, writeFile } from 'node:fs/promises'
import { createServer } from 'node:http'
import { tmpdir } from 'node:os'
import path from 'node:path'
import { promisify } from 'node:util'
import test from 'node:test'
import { chromium } from 'playwright'
import { choose, computerModels } from './account-node-provider-fixture.mjs'
import { selectSpace, freePort, repository, stopProcess, waitForHttp } from './platform-e2e-fixture.mjs'

const serverBinary = process.env.TERNILO_E2E_SERVER_BINARY ?? path.join(repository, 'target/debug/ternilo-server')
const nodeBinary = process.env.TERNILO_E2E_NODE_BINARY ?? path.join(repository, 'target/debug/ternilo')
const execute = promisify(execFile)
const cleanEnvironment = Object.fromEntries(Object.entries(process.env).filter(([key]) => !key.startsWith('TERNILO_')))
const pausedMessage = 'this account is paused while the server is in single-user mode'

function start(binary, args, env = {}) {
  const child = spawn(binary, args, { cwd: repository, env: { ...cleanEnvironment, ...env }, stdio: ['ignore', 'pipe', 'pipe'] })
  let output = ''
  child.stdout.on('data', chunk => { output += chunk })
  child.stderr.on('data', chunk => { output += chunk })
  return { child, diagnostics: () => output }
}

async function until(read, predicate, label, timeout = 30_000) {
  const deadline = Date.now() + timeout
  let latest
  while (Date.now() < deadline) {
    latest = await read()
    if (predicate(latest)) return latest
    await new Promise(resolve => setTimeout(resolve, 100))
  }
  throw new Error(`${label}: ${JSON.stringify(latest)}`)
}

async function jsonRequest(origin, endpoint, token, body, method = body === undefined ? 'GET' : 'POST', tenantId) {
  const response = await fetch(`${origin}/api/v1${endpoint}`, {
    method,
    headers: { ...(token ? { authorization: `Bearer ${token}` } : {}), ...(tenantId ? { 'x-ternilo-tenant': tenantId } : {}), ...(body === undefined ? {} : { 'content-type': 'application/json' }) },
    ...(body === undefined ? {} : { body: JSON.stringify(body) }),
  })
  const text = await response.text()
  assert.ok(response.status === 204 || response.headers.get('content-type')?.includes('application/json'), `${method} ${endpoint}: expected JSON, got ${response.status} ${text.slice(0, 100)}`)
  const value = response.status === 204 ? null : JSON.parse(text)
  assert.equal(response.ok, true, `${method} ${endpoint}: ${response.status} ${JSON.stringify(value)}`)
  return value
}

async function localToken(origin) {
  const html = await (await fetch(origin)).text()
  const encoded = html.match(/window\.__TERNILO_BOOT__\s*=\s*(\{[^<]+\});/)
  assert.ok(encoded, 'local page exposes its existing authenticated API bootstrap')
  return JSON.parse(encoded[1]).apiToken
}

async function seedLocalHistory(temporary, processes) {
  const dataDirectory = path.join(temporary, 'seed-data')
  const workspacePath = path.join(temporary, 'seed-workspace')
  await mkdir(workspacePath)
  const origin = `http://127.0.0.1:${await freePort()}`
  const process = start(nodeBinary, ['serve', '--data-dir', dataDirectory, '--listen', new URL(origin).host])
  processes.push(process)
  await waitForHttp(origin, process)
  const token = await localToken(origin)
  const workspace = await jsonRequest(origin, '/workspaces', token, { path: workspacePath })
  const session = await jsonRequest(origin, '/sessions', token, { workspace_id: workspace.workspace_id, session_id: 'same-local-session' })
  await jsonRequest(origin, `/sessions/${session.identity.session_id}/queue`, token, { content: { kind: 'prompt', input: '/write existing-history.txt local-history-retained' } })
  await until(async () => readFile(path.join(workspacePath, 'existing-history.txt'), 'utf8').catch(() => ''), value => value === 'local-history-retained', 'seed local command')
  await until(() => jsonRequest(origin, `/sessions/${session.identity.session_id}/queue`, token), value => value.active_run_id == null && !value.items.some(item => item.placement === 'queued'), 'seed run completion')
  await stopProcess(process)
  return { dataDirectory, workspacePath, workspaceId: workspace.workspace_id, sessionId: session.identity.session_id }
}

async function cloneLocalHistory(seed, temporary, label) {
  const dataDirectory = path.join(temporary, `${label}-data`)
  const workspacePath = path.join(temporary, `${label}-workspace`)
  await cp(seed.dataDirectory, dataDirectory, { recursive: true })
  await cp(seed.workspacePath, workspacePath, { recursive: true })
  const rewrite = async directory => {
    for (const entry of await readdir(directory, { withFileTypes: true })) {
      const file = path.join(directory, entry.name)
      if (entry.isDirectory()) await rewrite(file)
      else if (/\.jsonl?$/.test(entry.name)) await writeFile(file, (await readFile(file, 'utf8')).replaceAll(seed.workspacePath, workspacePath))
    }
  }
  await rewrite(dataDirectory)
  return { name: label, dataDirectory, workspacePath, origin: `http://127.0.0.1:${await freePort()}` }
}

async function credentials(page) {
  return page.evaluate(() => ({ token: JSON.parse(sessionStorage.getItem('ternilo.native.session')).access_token, tenantId: localStorage.getItem('ternilo.current-tenant') }))
}
async function requestAs(page, origin, endpoint, body, method) {
  const { token, tenantId } = await credentials(page)
  return jsonRequest(origin, endpoint, token, body, method, tenantId)
}
async function openSettings(page, section) {
  await page.getByRole('button', { name: '用户设置', exact: true }).click()
  const dialog = page.locator('[data-user-settings]')
  await dialog.getByRole('button', { name: section, exact: true }).click()
  return dialog
}
async function closeSettings(page) {
  await page.getByRole('button', { name: '返回工作台', exact: true }).click()
  await page.locator('[data-user-settings]').waitFor({ state: 'hidden' })
}
async function setMode(page, mode) {
  await page.goto(new URL('/admin/instance', page.url()).toString())
  const administration = page.locator('[data-platform-admin]')
  await selectChoice(administration.getByLabel('访问模式'), mode)
  await administration.getByRole('button', { name: '保存访问模式', exact: true }).click()
  const [response] = await Promise.all([
    page.waitForResponse(response => new URL(response.url()).pathname === '/api/v1/admin/instance' && response.request().method() === 'PATCH'),
    page.getByRole('dialog', { name: '更改访问模式' }).getByRole('button', { name: '保存访问模式', exact: true }).click(),
  ])
  assert.equal(response.status(), 200)
  await administration.getByRole('link', { name: '返回工作台', exact: true }).click()
  await page.getByRole('button', { name: '用户设置', exact: true }).waitFor()
}

async function verifySharingManagement(page, origin, entry, kind, memberId, artifacts) {
  await selectSession(page, entry)
  const id = kind === 'session' ? entry.session.identity.session_id : entry.workspace.workspace_id
  const endpoint = `/${kind === 'session' ? 'sessions' : 'workspaces'}/${encodeURIComponent(id)}/sharing`
  const row = kind === 'session'
    ? page.locator(`[data-sidebar-session-row][data-session-id="${entry.session.identity.session_id}"]`)
    : page.locator('[data-sidebar-workspace-row]').filter({ hasText: entry.workspace.title })
  await row.hover()
  if (kind === 'workspace') await page.locator('button[aria-label^="复制工作区完整路径："]').waitFor()
  await row.getByRole('button', { name: /的操作$/ }).click()
  const [loaded] = await Promise.all([
    page.waitForResponse(response => new URL(response.url()).pathname === `/api/v1${endpoint}` && response.request().method() === 'GET'),
    page.getByRole('menuitem', { name: '共享…', exact: true }).click(),
  ])
  assert.equal(loaded.status(), 200, `owner reads ${kind} sharing`)
  const dialog = page.getByRole('dialog', { name: kind === 'session' ? '共享会话' : '共享工作区', exact: true })
  const candidates = dialog.locator('[data-sharing-candidates]')
  await selectChoice(candidates.getByLabel('共享给', { exact: true }), 'user')
  await candidates.getByLabel('搜索团队成员', { exact: true }).fill('member')
  const [searched] = await Promise.all([
    page.waitForResponse(response => {
      const url = new URL(response.url())
      return url.pathname === `/api/v1${endpoint}/candidates` && url.searchParams.get('kind') === 'user'
        && url.searchParams.get('query') === 'member' && response.request().method() === 'GET'
    }),
    candidates.getByRole('button', { name: '搜索', exact: true }).click(),
  ])
  assert.equal(searched.status(), 200, 'the owner searches bounded resource-sharing candidates')
  const candidatePage = await searched.json()
  assert.ok(candidatePage.candidates.length <= 25)
  assert.ok(candidatePage.candidates.some(subject => subject.kind === 'user' && subject.user.user_id === memberId))
  await candidates.locator(`[data-sharing-candidate="user:${memberId}"]`).click()
  assert.equal(await page.locator('button[aria-label^="复制工作区完整路径："]').isVisible(), false, 'workspace hover details stay closed after opening sharing')
  const permissions = { view: true, submit: kind === 'workspace', stop: kind === 'workspace', configure: false }
  if (kind === 'workspace') {
    await dialog.getByLabel('发送任务', { exact: true }).check()
    await dialog.getByLabel('停止任务', { exact: true }).check()
    await page.setViewportSize({ width: 390, height: 844 })
    await page.locator('[data-app-frame][data-mobile="true"]').waitFor()
  }
  const [saved] = await Promise.all([
    page.waitForResponse(response => new URL(response.url()).pathname === `/api/v1${endpoint}/user/${encodeURIComponent(memberId)}` && response.request().method() === 'PUT'),
    dialog.getByRole('button', { name: '保存共享权限', exact: true }).click(),
  ])
  assert.equal(saved.status(), 204)
  const snapshot = await requestAs(page, origin, endpoint)
  assert.equal(snapshot.access.is_owner, true)
  assert.deepEqual(snapshot.shares.find(share => share.subject.kind === 'user' && share.subject.user.user_id === memberId).permissions, permissions)
  await dialog.getByRole('button', { name: '移除“member”的共享权限', exact: true }).waitFor()
  await page.waitForFunction(() => document.querySelector('#sharing-kind')?.disabled === false)
  await page.screenshot({ path: path.join(artifacts, `server-${kind}-sharing.png`), fullPage: true, animations: 'disabled' })
  const [removed] = await Promise.all([
    page.waitForResponse(response => new URL(response.url()).pathname === `/api/v1${endpoint}/user/${encodeURIComponent(memberId)}` && response.request().method() === 'DELETE'),
    dialog.getByRole('button', { name: '移除“member”的共享权限', exact: true }).click(),
  ])
  assert.equal(removed.status(), 204)
  await dialog.getByText('当前资源没有直接共享授权。', { exact: true }).waitFor()
  assert.deepEqual((await requestAs(page, origin, endpoint)).shares, [])
  await page.waitForFunction(() => document.querySelector('#sharing-kind')?.disabled === false)
  await dialog.getByRole('button', { name: '关闭', exact: true }).click()
  await dialog.waitFor({ state: 'hidden' })
  await page.setViewportSize({ width: 1440, height: 960 })
  await page.locator('[data-app-frame]:not([data-mobile])').waitFor()
}
async function enrollNode(page, origin, node, processes) {
  const settings = await openSettings(page, '我的机器')
  await settings.getByLabel('电脑名称', { exact: true }).fill(node.name)
  await settings.getByRole('button', { name: '生成启动命令', exact: true }).click()
  const launch = page.getByRole('dialog', { name: '启动 Ternilo Node' })
  const command = await launch.locator('[data-node-launch-command]').textContent()
  const token = /--token "([^"\s]+)"/.exec(command)?.[1]
  node.id = /--node-id="([^"\s]+)"/.exec(command)?.[1]
  assert.ok(node.id)
  assert.ok(token)
  await launch.getByRole('button', { name: '我已保存，关闭', exact: true }).click()
  await closeSettings(page)
  const process = start(nodeBinary, ['serve', '--data-dir', node.dataDirectory, '--listen', new URL(node.origin).host, '--node-id', node.id, '--gateway-url', `${origin.replace('http:', 'ws:')}/api/v1/executors/connect`, '--allow-insecure-gateway'], { TERNILO_LOCAL_TOKEN: token })
  processes.push(process)
  await waitForHttp(node.origin, process)
  node.token = await localToken(node.origin)
  const local = await jsonRequest(node.origin, '/state', node.token)
  assert.equal(local.workspaces.length, 1)
  assert.equal(local.workspaces[0].path, node.workspacePath)
  await jsonRequest(node.origin, `/workspaces/${local.workspaces[0].workspace_id}`, node.token, { title: `${node.name} history` }, 'PATCH')
  return local
}
async function discovered(page, origin, ids) {
  return until(() => requestAs(page, origin, '/state'), state => ids.every(id => state.workspaces.some(workspace => workspace.node_id === id) && state.sessions.some(session => state.workspaces.some(workspace => workspace.node_id === id && workspace.workspace_id === session.workspace_id))), 'local history auto-discovery')
}
function sessionOn(state, id) {
  const workspace = state.workspaces.find(workspace => workspace.node_id === id)
  assert.ok(workspace, `discovered workspace for ${id}`)
  const session = state.sessions.find(session => session.workspace_id === workspace.workspace_id)
  assert.ok(session, `discovered session for ${id}`)
  return { workspace, session }
}
async function selectSession(page, entry, canSubmit = true) {
  const row = page.locator(`[data-sidebar-session-row][data-session-id="${entry.session.identity.session_id}"]`)
  // API discovery can complete before the live snapshot reaches the sidebar.
  // Re-evaluate the group while waiting instead of fixing a fallback too early.
  await until(async () => {
    if (await row.isVisible()) return true
    const computer = page.locator(`[data-sidebar-computer-group="node:${entry.workspace.node_id}"] [data-sidebar-computer-button]`)
    if (await computer.count() && await computer.getAttribute('aria-expanded') === 'false') await computer.click()
    const named = page.locator('[data-sidebar-workspace-button]').filter({ hasText: entry.workspace.title })
    const workspace = await named.count() ? named : page.locator('[data-sidebar-workspace-button]').filter({ hasText: '未分组' })
    if (!await workspace.count()) return false
    assert.equal(await workspace.count(), 1)
    if (await workspace.getAttribute('aria-expanded') === 'false') await workspace.click()
    return row.isVisible()
  }, Boolean, `sidebar discovers session ${entry.session.identity.session_id}`)
  await row.locator('button').first().click()
  await page.waitForFunction(id => localStorage.getItem('ternilo.current-session') === id, entry.session.identity.session_id)
  await page.locator('article[data-role="user"]').filter({ hasText: '/write existing-history.txt local-history-retained' }).waitFor()
  if (canSubmit) await page.getByRole('textbox', { name: '输入任务', exact: true }).waitFor()
  else {
    await page.getByText('你可以查看内容，当前共享权限不允许发送任务。', { exact: true }).waitFor()
    assert.equal(await page.getByRole('textbox', { name: '输入任务', exact: true }).count(), 0)
  }
}

async function verifySharedPlan(owner, member, origin, entry, sharingPath, permissions, artifacts) {
  const sessionId = entry.session.identity.session_id
  const sessionPath = `/sessions/${encodeURIComponent(sessionId)}`
  await requestAs(owner, origin, sessionPath, { mode: 'plan' }, 'PATCH')
  await requestAs(owner, origin, `${sessionPath}/queue`, { content: { kind: 'prompt', input: '/exit-plan # Shared plan\n\n1. inspect\n2. implement\n3. verify' } })
  const pending = await until(() => requestAs(member, origin, `/questions?session_id=${encodeURIComponent(sessionId)}`), value => value.some(item => item.question.presentation?.kind === 'plan_review'), 'the owner creates a shared plan review')
  const item = pending.find(item => item.question.presentation?.kind === 'plan_review')
  assert.equal(item.session_id, sessionId, 'HTTP pending questions use the public session ID')
  const plan = member.locator('[data-plan-review]')
  await plan.waitFor()
  assert.equal(await plan.locator('[data-plan-review-action="approve"]').isDisabled(), true, 'Submit does not allow a plan review')
  const { token, tenantId } = await credentials(member)
  const answerPath = `/questions/${encodeURIComponent(item.question.id)}/answer?session_id=${encodeURIComponent(sessionId)}`
  const denied = await member.request.post(`${origin}/api/v1${answerPath}`, { headers: { authorization: `Bearer ${token}`, 'x-ternilo-tenant': tenantId }, data: { selected: [item.question.presentation.approve_label], custom: null } })
  assert.equal(denied.status(), 403)
  assert.equal((await denied.json()).error.code, 'policy_denied')
  const configureOnly = { ...permissions, submit: false, configure: true }
  await requestAs(owner, origin, sharingPath, configureOnly, 'PUT')
  await member.getByRole('textbox', { name: '输入任务', exact: true }).waitFor({ state: 'hidden' })
  const approve = plan.locator('[data-plan-review-action="approve"]')
  await until(() => approve.isEnabled(), value => value, 'Configure-only can approve the plan without task submission')
  if (artifacts) await member.screenshot({ path: path.join(artifacts, 'server-member-plan-configure.png'), fullPage: true })
  const [answered] = await Promise.all([
    member.waitForResponse(response => new URL(response.url()).pathname === `/api/v1/questions/${encodeURIComponent(item.question.id)}/answer` && response.request().method() === 'POST'),
    approve.click(),
  ])
  assert.equal(answered.status(), 204)
  await plan.waitFor({ state: 'hidden' })
  await until(() => requestAs(owner, origin, `${sessionPath}/queue`), value => value.active_run_id == null, 'the approved plan completes')
  await requestAs(owner, origin, sharingPath, permissions, 'PUT')
  await member.getByRole('textbox', { name: '输入任务', exact: true }).waitFor()
}

async function verifySharedAttachments(owner, member, origin, entry) {
  const sessionPath = `/sessions/${encodeURIComponent(entry.session.identity.session_id)}`
  const privateSession = await requestAs(owner, origin, '/sessions', { workspace_id: entry.workspace.workspace_id })
  assert.equal(privateSession.workspace_id, entry.workspace.workspace_id, 'the private and shared sessions use the same Node workspace')
  const privatePath = `/sessions/${encodeURIComponent(privateSession.identity.session_id)}`
  const privateAttachment = { name: 'private-notes.txt', media_type: 'text/plain', content: 'owner-only attachment content' }
  const privateSubmission = await requestAs(owner, origin, `${privatePath}/queue`, { content: { kind: 'prompt', input: 'keep this attachment private' }, attachments: [privateAttachment] })
  const privateReference = privateSubmission.attachments[0]
  assert.match(privateReference.content, /^ternilo-attachment:\/\/sha256\/[a-f0-9]{64}$/)
  await until(() => requestAs(owner, origin, `${privatePath}/queue`), value => value.active_run_id == null && !value.items.some(item => item.placement === 'queued'), 'the private attachment is retained in history')
  const ownerResolved = await requestAs(owner, origin, `${sessionPath}/attachments/resolve`, { attachment: privateReference })
  assert.equal(ownerResolved.content, privateAttachment.content, 'the owner can resolve its existing object from the same Node')
  const { token, tenantId } = await credentials(member)
  const headers = { authorization: `Bearer ${token}`, 'x-ternilo-tenant': tenantId }
  const before = await requestAs(owner, origin, `${sessionPath}/queue`)
  for (const [endpoint, data] of [
    [`${sessionPath}/attachments/resolve`, { attachment: privateReference }],
    [`${sessionPath}/queue`, { content: { kind: 'prompt', input: 'must not import a private object' }, attachments: [privateReference] }],
  ]) {
    const denied = await member.request.post(`${origin}/api/v1${endpoint}`, { headers, data })
    assert.equal(denied.status(), 403, 'knowing a digest does not share its private content')
    assert.equal((await denied.json()).error.code, 'policy_denied')
  }
  const after = await requestAs(owner, origin, `${sessionPath}/queue`)
  assert.deepEqual(after.items, before.items, 'a rejected foreign attachment does not enter the shared queue')
  assert.equal(after.active_run_id, before.active_run_id)
  const sharedContent = 'a new attachment shared with this session'
  await member.locator('input[type="file"]').setInputFiles({ name: 'shared-notes.txt', mimeType: 'text/plain', buffer: Buffer.from(sharedContent) })
  const input = member.getByRole('textbox', { name: '输入任务', exact: true })
  await input.fill('read the newly shared attachment')
  const [submitted] = await Promise.all([
    member.waitForResponse(response => new URL(response.url()).pathname === `/api/v1${sessionPath}/queue` && response.request().method() === 'POST'),
    member.getByRole('button', { name: '发送', exact: true }).click(),
  ])
  assert.equal(submitted.status(), 201)
  const sharedReference = (await submitted.json()).attachments[0]
  assert.match(sharedReference.content, /^ternilo-attachment:\/\/sha256\/[a-f0-9]{64}$/)
  await until(() => requestAs(member, origin, `${sessionPath}/queue`), value => value.active_run_id == null && !value.items.some(item => item.placement === 'queued'), 'the member inline upload completes')
  const sharedResolved = await requestAs(member, origin, `${sessionPath}/attachments/resolve`, { attachment: sharedReference })
  assert.equal(sharedResolved.content, sharedContent, 'the member can resolve an attachment actually present in shared history')
}

async function verifySharedSessionUse(owner, member, origin, entry, node, localSessionId, fixture, artifacts, revokedReadErrors) {
  const identity = await requestAs(member, origin, '/auth/session')
  const { token, tenantId } = await credentials(member)
  const sessionId = entry.session.identity.session_id
  const sessionPath = `/sessions/${encodeURIComponent(sessionId)}`
  const sharingPath = `${sessionPath}/sharing/user/${encodeURIComponent(identity.user.user_id)}`
  const targetQuery = `?session_id=${encodeURIComponent(sessionId)}`
  const headers = { authorization: `Bearer ${token}`, 'x-ternilo-tenant': tenantId }
  const permissions = { view: true, submit: false, stop: false, configure: false }
  const share = () => requestAs(owner, origin, sharingPath, permissions, 'PUT')
  const deny = async (endpoint, method, data) => {
    const response = await member.request.fetch(`${origin}/api/v1${endpoint}`, { method, headers, ...(data === undefined ? {} : { data }) })
    assert.equal(response.status(), 403, `${method} ${endpoint} is forbidden by shared permissions`)
    assert.equal((await response.json()).error.code, 'policy_denied')
  }
  const providerPath = `/providers${targetQuery}`
  const { source, ...provider } = (await requestAs(owner, origin, providerPath))[0]
  assert.equal(source, 'user')
  provider.models.push({ ...provider.models[0], id: 'owner-a-alternative', display_name: 'owner-a alternative' })
  await requestAs(owner, origin, providerPath, provider, 'POST')
  const originalDefault = await requestAs(owner, origin, `/default-model${targetQuery}`)

  await share()
  await member.goto(origin)
  await member.locator(`[data-sidebar-session-row][data-session-id="${sessionId}"]`).waitFor({ state: 'attached' })
  await selectSession(member, entry, false)
  await member.getByText('你可以查看内容，当前共享权限不允许发送任务。', { exact: true }).waitFor()
  const sharedState = await requestAs(member, origin, '/state')
  assert.deepEqual(sharedState.workspaces, [], 'sharing one session does not grant its parent workspace')
  assert.deepEqual(sharedState.sessions.map(session => session.identity.session_id), [sessionId])
  const shared = await requestAs(member, origin, `${sessionPath}/sharing`)
  assert.equal(shared.access.is_owner, false)
  assert.deepEqual(shared.access.permissions, permissions)
  assert.deepEqual(shared.shares, [])
  assert.equal(Object.hasOwn(shared, 'members'), false, 'sharing snapshots never embed the team member directory')
  assert.equal(shared.next_cursor, null, 'a shared viewer cannot page through resource grants')
  await deny(`${sessionPath}/sharing/candidates?kind=user`, 'GET')
  await deny(`${sessionPath}/sharing/candidates?kind=group`, 'GET')
  const providers = await requestAs(member, origin, providerPath)
  assert.equal(providers.find(value => value.id === provider.id).base_url, '')
  assert.doesNotMatch(JSON.stringify(providers), /owner-a-secret|owner-b-model/)
  const inventory = await requestAs(member, origin, `/credentials${targetQuery}`)
  assert.ok(inventory.references.some(reference => reference.configured))
  assert.ok(inventory.references.every(reference => reference.writable === false))
  assert.doesNotMatch(JSON.stringify(inventory), /owner-a-secret/)
  assert.equal((await requestAs(member, origin, `/agent-presets${targetQuery}`)).authorable, false)
  const settings = await openSettings(member, '通用')
  for (const name of ['模型', '插件', '凭据与登录']) assert.equal(await settings.getByRole('button', { name, exact: true }).count(), 0)
  await closeSettings(member)
  await deny(`${sessionPath}/queue`, 'POST', { content: { kind: 'prompt', input: 'must not run' } })
  await deny(sessionPath, 'PATCH', { model: { provider: 'profile_default' } })
  await deny(providerPath, 'POST', provider)
  await deny(`/credentials${targetQuery}`, 'POST', { name: 'forbidden', value: 'forbidden' })

  permissions.configure = true
  await share()
  const defaultWrites = []
  const recordDefault = request => { if (new URL(request.url()).pathname === '/api/v1/default-model' && request.method() === 'PUT') defaultWrites.push(request.url()) }
  member.on('request', recordDefault)
  const picker = member.getByRole('button', { name: /owner-a model/ })
  await picker.waitFor()
  await picker.click()
  await member.getByRole('menuitem', { name: /^模型/ }).hover()
  const [changed] = await Promise.all([
    member.waitForResponse(response => new URL(response.url()).pathname === `/api/v1${sessionPath}` && response.request().method() === 'PATCH'),
    member.getByRole('menuitem', { name: /owner-a alternative/ }).click(),
  ])
  assert.equal(changed.status(), 200)
  await member.getByRole('button', { name: /owner-a alternative/ }).waitFor()
  await owner.getByRole('button', { name: /owner-a alternative/ }).waitFor()
  await owner.getByText('当前模型不可用', { exact: true }).waitFor({ state: 'hidden' })
  assert.deepEqual(defaultWrites, [])
  member.off('request', recordDefault)
  assert.deepEqual(await requestAs(owner, origin, `/default-model${targetQuery}`), originalDefault)
  await deny(providerPath, 'POST', provider)
  await deny(`/credentials${targetQuery}`, 'POST', { name: 'forbidden', value: 'forbidden' })
  permissions.submit = true
  await share()
  await deny(`${sessionPath}/queue`, 'POST', { content: { kind: 'prompt', input: '/extension-enable shared-test 1.0.0' } })

  permissions.configure = false
  await share()
  const input = member.getByRole('textbox', { name: '输入任务', exact: true })
  await input.waitFor()
  await member.waitForFunction(() => [...document.querySelectorAll('button')].some(button => button.textContent.includes('owner-a alternative') && button.disabled))
  assert.equal(await member.getByRole('button', { name: /owner-a alternative/ }).isDisabled(), true)
  await deny(`${sessionPath}/queue`, 'POST', { content: { kind: 'prompt', input: '/extension-mount shared-test 1.0.0 {}' } })
  await input.fill('keep-streaming shared submit without stop')
  await member.getByRole('button', { name: '发送', exact: true }).click()
  await member.locator('article[data-role="assistant"]').filter({ hasText: 'owner-a-alternative answer' }).waitFor()
  assert.deepEqual(fixture.calls.at(-1), { model: 'owner-a-alternative', authorization: 'Bearer owner-a-secret' })
  const inbox = await until(() => requestAs(owner, origin, `${sessionPath}/queue`), value => Boolean(value.active_run_id), 'shared submission starts on the owner Node')
  assert.equal(await member.getByRole('button', { name: '停止运行', exact: true }).isDisabled(), true)
  await deny(`${sessionPath}/turns/${encodeURIComponent(inbox.active_run_id)}`, 'DELETE')
  await input.fill('queued shared work')
  await member.getByRole('button', { name: '发送', exact: true }).click()
  const queuedInbox = await until(() => requestAs(owner, origin, `${sessionPath}/queue`), value => value.items.some(item => item.placement === 'queued'), 'shared submit can queue more work')
  const queued = queuedInbox.items.find(item => item.placement === 'queued')
  const removeQueued = member.getByRole('button', { name: '删除排队消息', exact: true })
  assert.equal(await removeQueued.isDisabled(), true)
  await deny(`${sessionPath}/queue/${encodeURIComponent(queued.id)}`, 'DELETE')
  permissions.stop = true
  await share()
  await member.waitForFunction(() => [...document.querySelectorAll('button')].some(button => button.getAttribute('aria-label') === '停止运行' && !button.disabled))
  await removeQueued.click()
  await until(() => requestAs(owner, origin, `${sessionPath}/queue`), value => !value.items.some(item => item.id === queued.id), 'stop permission allows removing queued work')
  await member.getByRole('button', { name: '停止运行', exact: true }).click()
  await until(() => requestAs(owner, origin, `${sessionPath}/queue`), value => value.active_run_id == null, 'independently granted stop finishes the shared run')
  await member.getByRole('button', { name: '停止运行', exact: true }).waitFor({ state: 'hidden' })
  await verifySharedAttachments(owner, member, origin, entry)
  await verifySharedPlan(owner, member, origin, entry, sharingPath, permissions, artifacts)
  await member.screenshot({ path: path.join(artifacts, 'server-member-shared-session.png'), fullPage: true })

  let receivedEvents = 0
  const observe = socket => socket.on('framereceived', frame => { try { if (JSON.parse(String(frame.payload)).type === 'event_batch') receivedEvents += 1 } catch {} })
  let revocationStarted = null
  let verifiedReads = 0
  const verificationFailures = []
  const scopedRead = url => url.origin === origin && (
    ['queue', 'commands', 'history', 'stats', 'projection', 'plugins', 'workspace'].some(resource => url.pathname === `/api/v1${sessionPath}/${resource}`)
    || ['/api/v1/catalog', '/api/v1/model-options'].includes(url.pathname) && url.searchParams.get('session_id') === sessionId)
  await member.route(scopedRead, async route => {
    if (route.request().method() !== 'GET') { await route.continue(); return }
    // Read the real upstream response before delivery; aborted UI fetches may lose their CDP body.
    const response = await route.fetch()
    if (revocationStarted !== null && [400, 403].includes(response.status())) {
      try {
        const error = response.status() === 400
          ? { code: 'invalid_input', message: 'session does not exist' }
          : { code: 'policy_denied', message: 'this resource has not been shared with the requested permission' }
        assert.deepEqual(await response.json(), { error }, 'a revoked session read reveals no data')
        revokedReadErrors.push({ url: route.request().url(), since: revocationStarted, status: response.status() })
        verifiedReads++
      } catch (error) { verificationFailures.push(error) }
    }
    await route.fulfill({ response })
  })
  member.on('websocket', observe)
  await member.reload()
  await input.fill('keep-streaming while sharing is revoked')
  await member.getByRole('button', { name: '发送', exact: true }).click()
  await until(() => jsonRequest(node.origin, `/sessions/${localSessionId}/queue`, node.token), value => Boolean(value.active_run_id), 'second shared run starts before revocation')
  await until(async () => receivedEvents, value => value > 0, 'shared websocket receives events')
  revocationStarted = Date.now()
  await requestAs(owner, origin, sharingPath, undefined, 'DELETE')
  await member.locator(`[data-sidebar-session-row][data-session-id="${sessionId}"]`).waitFor({ state: 'detached' })
  assert.equal(await member.locator('article[data-role="user"]').count(), 0)
  const afterRevocation = receivedEvents
  await new Promise(resolve => setTimeout(resolve, 650))
  assert.equal(receivedEvents, afterRevocation, 'revoked member receives no further session events')
  const hiddenRead = await member.evaluate(async ({ endpoint, token, tenantId }) => {
    const response = await fetch(endpoint, { headers: { authorization: `Bearer ${token}`, 'x-ternilo-tenant': tenantId } })
    return { status: response.status, body: await response.json() }
  }, { endpoint: `${origin}/api/v1${sessionPath}/queue`, token, tenantId })
  assert.equal(hiddenRead.status, 400)
  assert.deepEqual(hiddenRead.body, { error: { code: 'invalid_input', message: 'session does not exist' } }, 'revoked browser reads reveal no queue data')
  for (const endpoint of [`${sessionPath}/commands`, `${sessionPath}/workspace`, `/catalog${targetQuery}`, `/model-options${targetQuery}`]) {
    const deniedRead = await member.evaluate(async ({ url, headers }) => {
      const response = await fetch(url, { headers })
      return { status: response.status, body: await response.json() }
    }, { url: `${origin}/api/v1${endpoint}`, headers })
    assert.equal(deniedRead.status, 400)
    assert.deepEqual(deniedRead.body, { error: { code: 'invalid_input', message: 'session does not exist' } })
  }
  const hidden = await member.request.post(`${origin}/api/v1${sessionPath}/queue`, { headers, data: { content: { kind: 'prompt', input: 'must not run after revocation' } } })
  assert.equal(hidden.status(), 400)
  assert.deepEqual((await hidden.json()).error, { code: 'invalid_input', message: 'session does not exist' })
  assert.ok((await jsonRequest(node.origin, `/sessions/${localSessionId}/queue`, node.token)).active_run_id, 'revocation does not cancel the owner task')
  fixture.release()
  await until(() => jsonRequest(node.origin, `/sessions/${localSessionId}/queue`, node.token), value => value.active_run_id == null, 'owner task finishes after sharing is revoked')
  await member.unrouteAll({ behavior: 'wait' })
  assert.deepEqual(verificationFailures, [])
  assert.ok(verifiedReads >= 5, 'all explicit revoked reads were checked before browser delivery')
  console.log(`Verified ${verifiedReads} scoped read(s) denied after session sharing was revoked.`)
  member.off('websocket', observe)
}
async function configureModel(page, node, baseUrl) {
  const { tenantId } = await credentials(page)
  await page.getByRole('button', { name: '我的模型', exact: true }).click()
  const settings = await computerModels(page, tenantId, node.id)
  await settings.getByRole('button', { name: '添加 Provider', exact: true }).first().click()
  const editor = settings.locator('[data-provider-editor="new"]')
  await editor.getByLabel('Provider ID', { exact: true }).fill('same-provider-id')
  await editor.getByLabel('显示名称', { exact: true }).fill(`${node.name} provider`)
  await editor.getByLabel('API Key', { exact: true }).fill(`${node.name}-secret`)
  await editor.getByLabel('API 地址', { exact: true }).fill(baseUrl)
  await editor.locator('[id$="-provider-defaults-context"]').fill('128K')
  await editor.locator('[id$="-provider-defaults-output"]').fill('8K')
  await editor.getByLabel('模型 ID 1', { exact: true }).fill(`${node.name}-model`)
  await editor.getByLabel('显示名称（可选） 1', { exact: true }).fill(`${node.name} model`)
  await editor.getByRole('button', { name: '添加 Provider', exact: true }).click()
  await settings.getByText(`${node.name} provider`, { exact: true }).waitFor()
  await page.getByRole('link', { name: '返回工作台', exact: true }).click()
  await choose(page, 'node', 'same-provider-id', `${node.name}-model`)
  await page.getByRole('button', { name: new RegExp(`${node.name} model`) }).waitFor()
}
async function modelFixture() {
  const calls = [], streams = new Set()
  const frame = value => `data: ${JSON.stringify(value)}\n\n`
  const complete = response => response.end(frame({ type: 'response.completed', response: { status: 'completed', output: [], usage: { input_tokens: 10, output_tokens: 5 } } }))
  const server = createServer(async (request, response) => {
    if (request.method !== 'POST' || request.url !== '/v1/responses') { response.writeHead(404); response.end(); return }
    let input = ''; for await (const chunk of request) input += chunk
    const body = JSON.parse(input)
    calls.push({ model: body.model, authorization: request.headers.authorization })
    response.writeHead(200, { 'content-type': 'text/event-stream', 'cache-control': 'no-cache' })
    response.write(frame({ type: 'response.output_text.delta', output_index: 0, content_index: 0, delta: `${body.model} answer ` }))
    const latestInput = body.input.findLast(item => item.role === 'user')
    if (JSON.stringify(latestInput).includes('keep-streaming')) {
      const timer = setInterval(() => response.write(frame({ type: 'response.output_text.delta', output_index: 0, content_index: 0, delta: 'still running ' })), 100)
      const release = () => { clearInterval(timer); complete(response); streams.delete(release) }
      streams.add(release)
      response.on('close', () => { clearInterval(timer); streams.delete(release) })
    } else complete(response)
  })
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve))
  return { baseUrl: `http://127.0.0.1:${server.address().port}/v1`, calls, release: () => [...streams].forEach(release => release()), close: async () => { [...streams].forEach(release => release()); await new Promise(resolve => server.close(resolve)) } }
}

test('SQLite Server preserves two private Node histories, shared permissions, and paused member streams', { timeout: 360_000 }, async () => {
  const temporary = await mkdtemp(path.join(tmpdir(), 'ternilo-server-accounts-nodes-'))
  const artifacts = process.env.TERNILO_E2E_ARTIFACT_DIR ?? temporary
  await mkdir(artifacts, { recursive: true })
  const processes = []
  let browser, fixture
  const pageErrors = [], consoleErrors = [], revokedReadErrors = []
  try {
    const seed = await seedLocalHistory(temporary, processes)
    const nodes = await Promise.all(['owner-a', 'owner-b', 'member-c'].map(id => cloneLocalHistory(seed, temporary, id)))
    const origin = `http://127.0.0.1:${await freePort()}`
    const config = path.join(temporary, 'server/config.json')
    const initialized = await execute(serverBinary, ['init', '--config-dir', path.dirname(config), '--non-interactive', '--listen', new URL(origin).host, '--public-url', origin], { cwd: repository, env: cleanEnvironment })
    const savedConfig = JSON.parse(await readFile(config, 'utf8'))
    assert.match(savedConfig.database_url, /^sqlite:/)
    assert.equal(savedConfig.oidc, null)
    assert.equal(savedConfig.managed_execution_enabled, false)
    const setupUrl = initialized.stdout.match(/Complete owner setup at: (\S+)/)?.[1]
    assert.ok(setupUrl)
    const server = start(serverBinary, ['serve', '--config-dir', path.dirname(config)])
    processes.push(server)
    await waitForHttp(`${origin}/auth/config`, server)
    fixture = await modelFixture()
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
    const ownerContext = await browser.newContext({ viewport: { width: 1440, height: 960 }, serviceWorkers: 'block' })
    const memberContext = await browser.newContext({ viewport: { width: 1280, height: 900 }, serviceWorkers: 'block' })
    const owner = await ownerContext.newPage(), member = await memberContext.newPage()
    for (const page of [owner, member]) {
      page.on('pageerror', error => pageErrors.push(error.message))
      page.on('console', message => { if (message.type() === 'error' && !message.text().includes('403')) consoleErrors.push({ message: message.text(), location: message.location().url, at: Date.now() }) })
      await page.addInitScript(() => localStorage.setItem('ternilo.theme', 'light'))
    }
    await owner.goto(setupUrl)
    await owner.getByLabel('用户名', { exact: true }).fill('owner')
    assert.equal(new URL(owner.url()).hash, '')
    await owner.getByLabel('密码', { exact: true }).fill('owner-test-password')
    await owner.getByLabel('邮箱', { exact: true }).fill('owner@example.test')
    await owner.getByRole('button', { name: '创建管理员并继续', exact: true }).click()
    await owner.getByRole('dialog').waitFor({ state: 'hidden' })
    assert.equal((await requestAs(owner, origin, '/admin/instance')).mode, 'single_user')
    const personal = await requestAs(owner, origin, '/auth/session')
    const team = (await requestAs(owner, origin, '/tenants', { slug: 'node-sharing-team', display_name: 'Node sharing team' })).tenant
    assert.notEqual(team.tenant_id, personal.personal_tenant_id)
    await owner.reload()
    await selectSpace(owner, team.tenant_id)
    const localA = await enrollNode(owner, origin, nodes[0], processes)
    const localB = await enrollNode(owner, origin, nodes[1], processes)
    assert.equal(localA.workspaces[0].workspace_id, localB.workspaces[0].workspace_id)
    assert.equal(localA.sessions[0].identity.session_id, localB.sessions[0].identity.session_id)
    const ownedState = await discovered(owner, origin, [nodes[0].id, nodes[1].id])
    const a = sessionOn(ownedState, nodes[0].id), b = sessionOn(ownedState, nodes[1].id)
    assert.notEqual(a.workspace.workspace_id, b.workspace.workspace_id)
    assert.notEqual(a.session.identity.session_id, b.session.identity.session_id)
    assert.notEqual(a.session.identity.session_id, seed.sessionId)
    for (const [index, entry] of [[0, a], [1, b]]) {
      await selectSession(owner, entry)
      await configureModel(owner, nodes[index], fixture.baseUrl)
      await owner.getByRole('textbox', { name: '输入任务', exact: true }).fill(`${nodes[index].name} private draft`)
    }
    await selectSession(owner, a)
    assert.equal(await owner.getByRole('textbox', { name: '输入任务', exact: true }).inputValue(), 'owner-a private draft')
    await owner.getByRole('button', { name: /owner-a model/ }).waitFor()
    await selectSession(owner, b)
    assert.equal(await owner.getByRole('textbox', { name: '输入任务', exact: true }).inputValue(), 'owner-b private draft')
    await owner.getByRole('button', { name: /owner-b model/ }).waitFor()
    const inventoryA = await requestAs(owner, origin, `/providers?session_id=${encodeURIComponent(a.session.identity.session_id)}`)
    const inventoryB = await requestAs(owner, origin, `/providers?session_id=${encodeURIComponent(b.session.identity.session_id)}`)
    assert.match(JSON.stringify(inventoryA), /owner-a-model/)
    assert.doesNotMatch(JSON.stringify(inventoryA), /owner-b-model/)
    assert.match(JSON.stringify(inventoryB), /owner-b-model/)
    assert.doesNotMatch(JSON.stringify(inventoryB), /owner-a-model/)
    await setMode(owner, 'multi_user')
    await owner.goto(`${origin}/spaces/current`)
    const spaceManagement = owner.locator('[data-space-management]')
    await spaceManagement.getByRole('button', { name: '生成邀请链接', exact: true }).click()
    const invitation = await spaceManagement.getByLabel('邀请链接', { exact: true }).inputValue()
    await owner.getByRole('link', { name: '返回工作台', exact: true }).click()
    const accountInvitation = await requestAs(owner, origin, '/admin/invitations', { tenant_id: null, role: 'member', expires_in_seconds: 3600 })
    await member.goto(`${origin}/#invite=${encodeURIComponent(accountInvitation.token)}`)
    await member.getByLabel('用户名', { exact: true }).fill('member')
    await member.getByLabel('邮箱', { exact: true }).fill('member@example.test')
    await member.getByLabel('密码', { exact: true }).fill('member-test-password')
    await member.getByRole('button', { name: '创建账号', exact: true }).click()
    await member.getByRole('dialog').waitFor({ state: 'hidden' })
    await member.goto(invitation)
    await member.getByRole('button', { name: '加入团队', exact: true }).click()
    await member.getByRole('dialog').waitFor({ state: 'hidden' })
    const registered = await requestAs(member, origin, '/auth/session')
    assert.notEqual(registered.personal_tenant_id, personal.personal_tenant_id)
    assert.notEqual(registered.personal_tenant_id, team.tenant_id)
    await selectSpace(member, team.tenant_id)
    assert.deepEqual((await requestAs(member, origin, '/state')).workspaces, [])
    const memberCredentials = await credentials(member)
    const memberIdentity = await requestAs(member, origin, '/auth/session')
    // Keep background chat reads out of the grant-editor checks; the active-chat revocation case follows.
    await member.goto(`${origin}/spaces/current`)
    await verifySharingManagement(owner, origin, a, 'session', memberIdentity.user.user_id, artifacts)
    await verifySharingManagement(owner, origin, a, 'workspace', memberIdentity.user.user_id, artifacts)
    await verifySharedSessionUse(owner, member, origin, a, nodes[0], seed.sessionId, fixture, artifacts, revokedReadErrors)
    const denied = await member.request.get(`${origin}/api/v1/sessions/${a.session.identity.session_id}/events`, { headers: { authorization: `Bearer ${memberCredentials.token}`, 'x-ternilo-tenant': memberCredentials.tenantId } })
    assert.equal(denied.status(), 400)
    assert.deepEqual((await denied.json()).error, { code: 'invalid_input', message: 'session does not exist' }, 'private owner history is unavailable to an invited member')
    await enrollNode(member, origin, nodes[2], processes)
    const memberState = await discovered(member, origin, [nodes[2].id])
    const c = sessionOn(memberState, nodes[2].id)
    assert.equal(memberState.workspaces.length, 1)
    assert.equal((await requestAs(owner, origin, '/state')).workspaces.length, 2)
    assert.equal(new Set([a.workspace.workspace_id, b.workspace.workspace_id, c.workspace.workspace_id]).size, 3)
    assert.equal(new Set([a.session.identity.session_id, b.session.identity.session_id, c.session.identity.session_id]).size, 3)
    await selectSession(member, c)
    await configureModel(member, nodes[2], fixture.baseUrl)
    let receivedEvents = 0
    member.on('websocket', socket => socket.on('framereceived', frame => { try { if (JSON.parse(String(frame.payload)).type === 'event_batch') receivedEvents += 1 } catch {} }))
    await member.reload()
    await member.getByRole('textbox', { name: '输入任务', exact: true }).fill('keep-streaming while access changes')
    await member.getByRole('button', { name: '发送', exact: true }).click()
    await member.locator('article[data-role="assistant"]').filter({ hasText: 'member-c-model answer' }).waitFor()
    await setMode(owner, 'single_user')
    await member.getByRole('dialog', { name: '访问已暂停' }).waitFor({ timeout: 15_000 })
    await member.screenshot({ path: path.join(artifacts, 'server-member-paused.png'), fullPage: true })
    assert.equal(await member.getByRole('textbox', { name: '输入任务', exact: true }).count(), 0)
    const afterPause = receivedEvents
    await new Promise(resolve => setTimeout(resolve, 650))
    assert.equal(receivedEvents, afterPause, 'paused member receives no further events')
    const activeInbox = await jsonRequest(nodes[2].origin, `/sessions/${seed.sessionId}/queue`, nodes[2].token)
    assert.ok(activeInbox.active_run_id, 'pausing access does not cancel the local task')
    const blocked = await member.request.post(`${origin}/api/v1/sessions/${c.session.identity.session_id}/queue`, { headers: { authorization: `Bearer ${memberCredentials.token}`, 'x-ternilo-tenant': memberCredentials.tenantId }, data: { content: { kind: 'prompt', input: 'must not run' } } })
    assert.equal(blocked.status(), 403)
    assert.deepEqual((await blocked.json()).error, { code: 'policy_denied', message: pausedMessage })
    await member.getByRole('button', { name: '重试访问', exact: true }).click()
    await member.getByRole('dialog', { name: '访问已暂停' }).waitFor()
    fixture.release()
    await until(() => jsonRequest(nodes[2].origin, `/sessions/${seed.sessionId}/queue`, nodes[2].token), value => value.active_run_id == null, 'retained local task completes')
    await setMode(owner, 'multi_user')
    await member.getByRole('button', { name: '重试访问', exact: true }).click()
    await member.getByRole('dialog', { name: '访问已暂停' }).waitFor({ state: 'hidden' })
    await member.getByRole('textbox', { name: '输入任务', exact: true }).waitFor()
    assert.equal(await member.evaluate(() => localStorage.getItem('ternilo.current-session')), c.session.identity.session_id)
    const restored = await discovered(owner, origin, [nodes[0].id, nodes[1].id])
    assert.equal(restored.sessions.find(session => session.identity.session_id === a.session.identity.session_id)?.workspace_id, a.workspace.workspace_id)
    assert.equal(restored.sessions.find(session => session.identity.session_id === b.session.identity.session_id)?.workspace_id, b.workspace.workspace_id)
    await owner.waitForFunction(() => [...document.querySelectorAll('button')].some(button => button.textContent.includes('owner-a alternative') && !button.disabled))
    await member.locator('article[data-role="assistant"]').filter({ hasText: 'member-c-model answer' }).waitFor()
    await member.getByRole('button', { name: /member-c model/ }).waitFor()
    await owner.getByText('当前模型不可用', { exact: true }).waitFor({ state: 'hidden' })
    await owner.screenshot({ path: path.join(artifacts, 'server-owner-desktop.png'), fullPage: true })
    await member.setViewportSize({ width: 390, height: 844 })
    await member.locator('[data-app-frame][data-mobile="true"]').waitFor()
    await member.waitForFunction(() => document.querySelector('[data-app-sidebar-column]').getBoundingClientRect().right <= 0)
    assert.equal(await member.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true)
    await member.screenshot({ path: path.join(artifacts, 'server-member-mobile.png'), fullPage: true })
    assert.deepEqual(pageErrors, [])
    const unexpectedConsoleErrors = consoleErrors.filter(error => {
      const expected = revokedReadErrors.findIndex(read => read.url === error.location && error.at >= read.since
        && error.message === `Failed to load resource: the server responded with a status of ${read.status} (${read.status === 400 ? 'Bad Request' : 'Forbidden'})`)
      if (expected < 0) return true
      revokedReadErrors.splice(expected, 1)
      return false
    })
    assert.deepEqual(unexpectedConsoleErrors, [])
  } catch (error) {
    if (process.env.TERNILO_E2E_ARTIFACT_DIR && browser) {
      for (const [index, context] of browser.contexts().entries()) {
        const page = context.pages()[0]
        if (page) await page.screenshot({ path: path.join(artifacts, `server-failure-${index}.png`), fullPage: true }).catch(() => {})
      }
    }
    error.message += `\nDiagnostics: ${processes.map(process => process.diagnostics()).join('\n')}`
    throw error
  } finally {
    await browser?.close()
    fixture?.release()
    for (const process of processes.reverse()) await stopProcess(process)
    await fixture?.close()
    await rm(temporary, { recursive: true, force: true })
  }
})
