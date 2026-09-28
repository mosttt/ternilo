import { selectChoice } from './browser-select-fixture.mjs'
import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { createServer } from 'node:http'
import path from 'node:path'
import { tmpdir } from 'node:os'
import test from 'node:test'
import { chromium } from 'playwright'
import { selectSpace, execute, freePort, initializeServer, repository, registerOidcUser, serverRequest, startOidcServer, startProcess, stopProcess } from './platform-e2e-fixture.mjs'

async function until(read, predicate, label) {
  const deadline = Date.now() + 45_000
  while (Date.now() < deadline) {
    const value = await read()
    if (predicate(value)) return value
    await new Promise(resolve => setTimeout(resolve, 100))
  }
  throw new Error(`Timed out: ${label}`)
}

async function modelFixture() {
  const calls = []
  const server = createServer((request, response) => {
    if (request.method !== 'POST' || request.url !== '/v1/chat/completions') { response.writeHead(404); response.end(); return }
    const chunks = []
    request.on('data', chunk => chunks.push(chunk))
    request.on('end', () => {
      const body = JSON.parse(Buffer.concat(chunks).toString())
      calls.push({ body, key: request.headers.authorization, bytes: { request: Buffer.byteLength(JSON.stringify(body)), messages: Buffer.byteLength(JSON.stringify(body.messages ?? [])), tools: Buffer.byteLength(JSON.stringify(body.tools ?? [])), system: Buffer.byteLength(JSON.stringify((body.messages ?? []).filter(message => ['system', 'developer'].includes(message.role)))) } })
      const latestUser = body.messages.findLastIndex(message => message.role === 'user')
      const needsTool = JSON.stringify(body.messages[latestUser]).includes('Write the managed proof') && body.tools?.some(tool => tool.function?.name === 'write_file') && !body.messages.slice(latestUser + 1).some(message => message.role === 'tool')
      const proof = request.headers.authorization === 'Bearer account-worker-secret' ? 'Account model from My Models executes in a team workspace.\n' : 'Managed model access uses the explicitly selected owner budget.\n'
      const tool = { index: 0, id: 'managed-write', type: 'function', function: { name: 'write_file', arguments: JSON.stringify({ path: 'managed-model-proof.txt', content: proof }) } }
      const usage = { prompt_tokens: 100, completion_tokens: 20, total_tokens: 120, prompt_tokens_details: { cached_tokens: 10 }, completion_tokens_details: { reasoning_tokens: 5 } }
      const identity = { id: `managed-${calls.length}`, created: 1, model: body.model }
      const content = 'Managed model task completed.'
      if (!body.stream) {
        response.writeHead(200, { 'content-type': 'application/json' })
        response.end(JSON.stringify({ ...identity, object: 'chat.completion', choices: [{ index: 0, message: { role: 'assistant', content }, finish_reason: 'stop' }], usage })); return
      }
      const sse = value => `data: ${JSON.stringify(value)}\n\n`
      response.writeHead(200, { 'content-type': 'text/event-stream' })
      response.end(sse({ ...identity, object: 'chat.completion.chunk', choices: [{ index: 0, delta: needsTool ? { role: 'assistant', tool_calls: [tool] } : { role: 'assistant', content }, finish_reason: null }] })
        + sse({ ...identity, object: 'chat.completion.chunk', choices: [{ index: 0, delta: {}, finish_reason: needsTool ? 'tool_calls' : 'stop' }] })
        + sse({ ...identity, object: 'chat.completion.chunk', choices: [], usage }) + 'data: [DONE]\n\n')
    })
  })
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve))
  return { calls, baseUrl: `http://127.0.0.1:${server.address().port}/v1`, close: () => new Promise(resolve => server.close(resolve)) }
}

async function dismissNotifications(page) {
  const dismiss = page.getByRole('button', { name: '关闭通知', exact: true })
  while (await dismiss.count()) await dismiss.first().click()
}

async function loginOwner(page, application) {
  await page.goto(application.origin)
  await page.getByLabel('用户名', { exact: true }).fill(application.owner.username)
  await page.getByLabel('密码', { exact: true }).fill(application.owner.password)
  await page.getByRole('button', { name: '登录', exact: true }).click()
  await page.getByRole('combobox', { name: '切换空间', exact: true }).waitFor()
}

async function selectSession(page, tenantId, sessionId) {
  await selectSpace(page, tenantId)
  const row = page.locator(`[data-sidebar-session-row][data-session-id="${sessionId}"]`)
  await row.waitFor()
  await row.click()
  await page.locator('[data-composer-input]').waitFor()
}

async function assertCollaboratorIdentity(ownerPage, memberPage, text, account) {
  for (const [page, label] of [[ownerPage, account.username], [memberPage, '你']]) {
    const message = page.locator('article[data-role="user"]').filter({ hasText: text })
    await until(() => message.count(), count => count === 1, `completed collaborator message shown for ${label}`)
    await until(() => message.locator('[data-input-identity-label]').textContent(), value => value === label, `collaborator identity label ${label}`)
    assert.equal((await message.textContent()).includes(account.user_id), false, 'account ID is not displayed inline')
  }
}

function assertPersistedSubmitter(submission, events, account) {
  assert.deepEqual(submission.provenance.author, { kind: 'account', user_id: account.user_id, username: account.username }, 'canonical submission identifies the collaborator independently of billing')
  assert.equal(submission.provenance.input_id, submission.id)
  const messages = events.filter(event => event.run_id === submission.run_id && event.type === 'user_message')
  assert.equal(messages.length, 1, 'the completed Worker run persists one submitted user message')
  assert.deepEqual(messages[0].provenance, submission.provenance, 'the persisted Worker message retains the canonical input ID and author')
}

async function modelDirectory(page, triggerName, english = false, surface = page.locator('[data-input-bar]')) {
  await surface.getByRole('button', { name: triggerName, exact: true }).click()
  const trigger = page.getByRole('menuitem', { name: english ? /^Model/ : /^模型/ })
  if (page.viewportSize().width <= 760) await trigger.click()
  else { await trigger.hover(); await trigger.press('ArrowRight') }
  const dialog = page.getByRole('menu').filter({ has: page.locator('[data-model-source="platform"]') })
  await dialog.getByRole('button', { name: english ? 'Search' : '搜索', exact: true }).waitFor()
  return dialog
}

async function assertLayout(page, surface) {
  await surface.evaluate(async element => { await Promise.all(element.getAnimations({ subtree: true }).filter(animation => animation.effect?.getTiming().iterations !== Infinity).map(animation => animation.finished.catch(() => {}))) })
  assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), 'document stays inside viewport')
  assert.ok(await surface.evaluate(element => element.scrollWidth <= element.clientWidth), 'model menu has no horizontal overflow')
  const box = await surface.boundingBox(), size = page.viewportSize()
  assert.ok(box && box.x >= 0 && box.y >= 0 && box.x + box.width <= size.width + 1 && box.y + box.height <= size.height + 1, 'model menu fits viewport')
  for (const button of await surface.getByRole('button').all()) {
    const target = await button.boundingBox()
    if (target && size.width <= 760) assert.ok(target.width >= 39.9 && target.height >= 39.9, `touch target ${await button.innerText()}`)
  }
}

test('managed model selection fixes resource-owner budgets and BYOK across sharing and mobile', { timeout: 360_000 }, async context => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-cloud-model-browser-'))
  const artifacts = process.env.TERNILO_E2E_ARTIFACT_DIR ?? directory
  await mkdir(artifacts, { recursive: true })
  const oidc = await startOidcServer({ audience: 'managed-model-browser', identities: { collaborator: { subject: 'managed-collaborator', email: 'managed-collaborator@example.test', name: 'Managed collaborator' } } })
  const upstream = await modelFixture()
  let application, worker, browser, ownerPage, memberPage
  const errors = []
  const expectedValidation = []
  const mobileModelGeometry = []
  const memberDefaultWrites = []
  let expectedBudgetRejection = false
  try {
    const policy = JSON.parse(await readFile(path.join(repository, 'examples/worker-policy.json'), 'utf8'))
    policy.policy_revision = 'managed-model-browser'
    policy.minimum_workspace_free_bytes = 0
    policy.allowed_plugin_kinds = [...new Set([...policy.allowed_plugin_kinds, 'ternilo.model.host_gateway', 'ternilo.sandbox.cloud_outer', 'ternilo.files.local', 'ternilo.shell.local', 'ternilo.prompt.workspace_instructions', 'ternilo.tools.files', 'ternilo.tool.shell', 'ternilo.tool.ask_user', 'ternilo.tool.plan', 'ternilo.skills.registry', 'ternilo.skills.filesystem', 'ternilo.tools.skills', 'ternilo.subagents.in_process', 'ternilo.tools.agent_team'])]
    const policyPath = path.join(directory, 'policy.json')
    await writeFile(policyPath, JSON.stringify(policy))
    const origin = `http://127.0.0.1:${await freePort()}`
    application = await initializeServer({ directory: path.join(directory, 'server'), origin, oidc: { issuer: oidc.issuer, audience: 'managed-model-browser', client_id: 'managed-model-browser' }, workerPolicy: policyPath, managedExecutionEnabled: true })
    const token = application.owner.session.access_token
    const assetHashes = {}
    const registrationPolicy = await serverRequest(origin, '/admin/registration', { token: token })
    await serverRequest(origin, '/admin/registration', { token: token, method: 'PATCH', body: { mode: 'open', require_approval: false, revision: registrationPolicy.revision } })
    for (const asset of ['app.js', 'app.css']) {
      const response = await fetch(`${origin}/assets/${asset}`)
      assert.equal(response.status, 200)
      const actual = createHash('sha256').update(Buffer.from(await response.arrayBuffer())).digest('hex')
      const expected = createHash('sha256').update(await readFile(path.join(repository, 'web/dist/assets', asset))).digest('hex')
      assert.equal(actual, expected, `Server must embed the current ${asset}`)
      assetHashes[asset] = actual
    }
    const team = (await serverRequest(origin, '/tenants', { token, body: { slug: 'managed-models', display_name: 'Managed collaboration' } })).tenant
    const member = await registerOidcUser(origin, oidc.accessToken(), 'managed-collaborator')
    await serverRequest(origin, `/tenants/${team.tenant_id}/members/${member.user.user_id}`, { token, method: 'PUT', body: { role: 'member' } })
    const project = (await serverRequest(origin, '/projects', { token, tenantId: team.tenant_id, body: { name: 'Model budgets' } })).project
    const workspace = (await serverRequest(origin, '/workspaces', { token, tenantId: team.tenant_id, body: { project_id: project.project_id, name: 'Managed workspace', placement: 'cloud' } })).workspace
    const session = await serverRequest(origin, '/sessions', { token, tenantId: team.tenant_id, body: { workspace_id: workspace.workspace_id, session_id: 'managed-model-browser', permissions: 'workspace_write' } })
    const sessionId = session.identity.session_id
    const access = resource => serverRequest(origin, resource, { token, tenantId: team.tenant_id })
    assert.equal(session.model.provider, 'profile_default', 'empty managed session does not infer a model')
    const defaults = { context_window: 100_000, max_output_tokens: 2048, reasoning: { default_effort: 'medium', efforts: { low: 'low', medium: 'medium', high: 'high' } } }
    const profile = { id: 'managed-upstream', display_name: 'Managed upstream', base_url: upstream.baseUrl, protocol: 'openai-chat-completions', api_key_ref: null, defaults, models: [{ id: 'same-model', settings: { mode: 'inherit' } }], timeout_ms: 30_000, max_attempts: 2, retry_base_delay_ms: 100 }
    await serverRequest(origin, '/admin/models/providers', { token, body: { profile, enabled: true, api_key: 'managed-platform-secret' } })
    await serverRequest(origin, '/admin/models/publications', { token, body: { model_id: 'same-model', display_name: 'Published model', provider_id: profile.id, upstream_model: 'same-model', enabled: true } })
    const grants = []
    for (let index = 0; index < 27; index++) grants.push(await serverRequest(origin, '/admin/models/grants', { token, body: { name: `Budget ${String(index).padStart(2, '0')}`, subject: { kind: 'user', id: application.owner.session.user.user_id }, model_ids: ['same-model'], monthly_tokens: 1_000_000, max_concurrent_requests: 4, allow_resource_sharing: true } }))
    await serverRequest(origin, `/sessions/${sessionId}/sharing/user/${member.user.user_id}`, { token, tenantId: team.tenant_id, method: 'PUT', body: { view: true, submit: true, stop: true, configure: true } })
    const registration = await serverRequest(origin, '/admin/workers', { token, body: { worker_id: 'managed-browser-worker' } })
    const workerBinary = process.env.TERNILO_CLOUD_E2E_WORKER_BINARY ?? path.join(repository, 'target/debug/ternilo-worker')
    const workerConfig = path.join(directory, 'worker.json'), workspaceRoot = path.join(directory, 'workspaces')
    await execute(workerBinary, ['init', '--config', workerConfig, '--server-url', origin, '--workspace-root', workspaceRoot, '--sandbox', 'bubblewrap', '--health-listen', '127.0.0.1:0', '--poll-interval-ms', '50'], { cwd: repository, env: { ...process.env, TERNILO_WORKER_TOKEN: registration.token } })
    worker = startProcess(workerBinary, ['serve', '--config', workerConfig])
    await until(() => worker.diagnostics(), output => /Ternilo cloud worker .* ready/.test(output), 'Worker ready')
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
    const ownerContext = await browser.newContext({ viewport: { width: 1440, height: 1000 }, hasTouch: true, serviceWorkers: 'block' })
    const memberContext = await browser.newContext({ viewport: { width: 1280, height: 900 }, serviceWorkers: 'block' })
    ownerPage = await ownerContext.newPage(); memberPage = await memberContext.newPage()
    memberPage.on('request', request => { if (request.method() === 'PUT' && new URL(request.url()).pathname === '/api/v1/default-model') memberDefaultWrites.push(request.url()) })
    for (const page of [ownerPage, memberPage]) {
      page.on('pageerror', error => errors.push(error.message))
      page.on('console', message => {
        if (message.type() !== 'error') return
        if (expectedBudgetRejection && message.text() === 'Failed to load resource: the server responded with a status of 400 (Bad Request)') expectedValidation.push('browser reported the expected rejected submission')
        else errors.push(message.text())
      })
      page.on('response', response => {
        if (response.status() < 400) return
        const pathname = new URL(response.url()).pathname
        if (expectedBudgetRejection && response.status() === 400 && pathname === `/api/v1/sessions/${sessionId}/queue`) expectedValidation.push('HTTP 400 for the expected image budget validation')
        else errors.push(`HTTP ${response.status()} ${pathname}`)
      })
    }
    await loginOwner(ownerPage, application)
    const accidentalCreation = []
    ownerPage.on('request', request => { if (request.method() === 'POST' && ['/api/v1/sessions', '/api/v1/workspaces'].includes(new URL(request.url()).pathname)) accidentalCreation.push(request.url()) })
    assert.equal(await ownerPage.getByRole('combobox', { name: '切换空间', exact: true }).getAttribute('data-space-id'), application.owner.session.personal_tenant_id)
    await ownerPage.getByRole('button', { name: '我的模型', exact: true }).click()
    const firstSettings = ownerPage.locator('[data-model-access-shell]')
    await ownerPage.reload()
    await firstSettings.getByRole('heading', { name: '个人空间 · 托管新会话默认模型', exact: true }).waitFor()
    const defaultOptions = await serverRequest(origin, '/model-options', { token, tenantId: application.owner.session.personal_tenant_id })
    assert.equal(defaultOptions.current, null)
    const defaultOption = defaultOptions.options[0]
    const defaultPicker = await modelDirectory(ownerPage, '选择模型', false, firstSettings)
    assert.ok(!defaultOption.grant_name.includes('Published model'))
    await defaultPicker.getByRole('textbox', { name: '搜索模型或预算名称', exact: true }).fill('Published model')
    await defaultPicker.getByRole('button', { name: '搜索', exact: true }).click()
    await defaultPicker.locator(`[data-model-grant="${defaultOption.grant_id}"]`).getByRole('menuitem', { name: 'Published model same-model', exact: true }).click()
    await until(() => serverRequest(origin, '/model-options', { token, tenantId: application.owner.session.personal_tenant_id }), value => value.current?.selection.grant_id === defaultOption.grant_id, 'default chosen before the first workspace')
    assert.deepEqual(accidentalCreation, [], 'setting a default does not create a workspace or session')
    const firstProject = (await serverRequest(origin, '/projects', { token, tenantId: application.owner.session.personal_tenant_id, body: { name: 'First personal project' } })).project
    const firstWorkspace = (await serverRequest(origin, '/workspaces', { token, tenantId: application.owner.session.personal_tenant_id, body: { project_id: firstProject.project_id, name: 'Default-ready workspace', placement: 'cloud' } })).workspace
    const firstSession = await serverRequest(origin, '/sessions', { token, tenantId: application.owner.session.personal_tenant_id, body: { workspace_id: firstWorkspace.workspace_id, session_id: 'default-ready-session', permissions: 'workspace_write' } })
    assert.deepEqual(firstSession.model, { provider: 'platform_model', grant_id: defaultOption.grant_id, model_id: defaultOption.model.model_id })
    await firstSettings.getByRole('link', { name: '返回工作台', exact: true }).click()
    context.diagnostic('Default model configuration needs no workspace or session, and the first new session inherits the explicit budget')
    await selectSession(ownerPage, team.tenant_id, sessionId)
    assert.equal(await ownerPage.locator('[data-platform-admin]').count(), 0, 'owner starts in the ordinary workbench')
    await ownerPage.getByRole('button', { name: '用户设置', exact: true }).click()
    let userSettings = ownerPage.locator('[data-user-settings]')
    await userSettings.waitFor()
    assert.equal(new URL(ownerPage.url()).pathname, '/settings/general')
    assert.equal(await userSettings.locator('a[href^="/admin"]').count(), 0)
    assert.equal(await userSettings.getByRole('button', { name: /Worker|平台管理|空间管理/ }).count(), 0)
    assert.equal(await userSettings.getByRole('button', { name: '模型', exact: true }).count(), 0)
    assert.equal(await userSettings.locator('a[href="/models"]').count(), 0)
    await userSettings.getByRole('button', { name: '返回工作台', exact: true }).click()
    await ownerPage.getByRole('button', { name: '我的模型', exact: true }).click()
    userSettings = ownerPage.locator('[data-model-access-shell]')
    await ownerPage.reload()
    await userSettings.getByRole('heading', { name: '个人空间 · 托管新会话默认模型', exact: true }).waitFor()
    for (const viewport of [{ width: 390, height: 844 }, { width: 320, height: 740 }, { width: 844, height: 390 }]) {
      await ownerPage.setViewportSize(viewport)
      await assertLayout(ownerPage, userSettings)
      await userSettings.getByRole('link', { name: '返回工作台', exact: true }).waitFor()
      await ownerPage.screenshot({ path: path.join(artifacts, `user-settings-${viewport.width}.png`) })
    }
    await ownerPage.setViewportSize({ width: 390, height: 844 })
    await userSettings.getByRole('button', { name: '添加 Provider', exact: true }).first().click()
    await userSettings.locator('[data-provider-editor="new"]').waitFor()
    await userSettings.getByLabel('Provider ID', { exact: true }).focus()
    await ownerPage.evaluate(() => document.documentElement.style.setProperty('--ternilo-visual-viewport-height', '430px'))
    const visibleSettings = await userSettings.boundingBox()
    assert.ok(visibleSettings && visibleSettings.y >= 0 && visibleSettings.y + visibleSettings.height <= 431, 'user settings fit the visible area above a software keyboard')
    await until(() => userSettings.getByLabel('Provider ID', { exact: true }).boundingBox(), box => box && box.y >= 0 && box.y + box.height <= 431, 'focused model field scrolls above the software keyboard')
    const focusedInput = await userSettings.getByLabel('Provider ID', { exact: true }).boundingBox()
    assert.ok(focusedInput && focusedInput.y >= 0 && focusedInput.y + focusedInput.height <= 431, 'the focused model field remains reachable')
    await ownerPage.screenshot({ path: path.join(artifacts, 'user-settings-keyboard.png') })
    await ownerPage.evaluate(() => document.documentElement.style.removeProperty('--ternilo-visual-viewport-height'))
    await ownerPage.setViewportSize({ width: 1440, height: 1000 })
    await userSettings.getByRole('link', { name: '返回工作台', exact: true }).click()
    await ownerPage.getByRole('button', { name: '平台管理', exact: true }).click()
    const administration = ownerPage.locator('[data-platform-admin]')
    await administration.waitFor()
    assert.equal(await administration.locator('a[href="/models"], a[href^="/settings"], a[href="/spaces/current"]').count(), 0, 'platform administration has its own navigation')
    await administration.getByRole('link', { name: '返回工作台', exact: true }).click()
    context.diagnostic('Owner settings and platform administration are separate, with mobile model access and preserved configuration target')
    await ownerPage.locator('[data-model-onboarding][data-state="empty"]').waitFor()
    assert.match(await ownerPage.locator('[data-model-onboarding]').textContent(), /平台模型及预算/)
    await ownerPage.locator('[data-composer-input]').fill('Keep this draft before configuring a model')
    assert.equal(await ownerPage.locator('[data-input-bar]').getByRole('button', { name: '发送', exact: true }).isDisabled(), true)
    assert.equal(upstream.calls.length, 0, 'empty session has no automatic upstream calls')
    const first = await access(`/model-options?session_id=${sessionId}`)
    assert.equal(first.current, null)
    assert.equal(first.options.length, 25)
    let dialog = await modelDirectory(ownerPage, '选择模型')
    await dialog.getByRole('button', { name: '下一页', exact: true }).click()
    const second = await access(`/model-options?session_id=${sessionId}&cursor=${encodeURIComponent(first.next_cursor)}`)
    assert.equal(second.options.length, 2)
    const chosen = second.options[0]
    await dialog.locator(`[data-model-grant="${chosen.grant_id}"]`).getByRole('menuitem', { name: 'Published model same-model', exact: true }).click()
    await dialog.waitFor({ state: 'hidden' })
    await until(() => access(`/model-options?session_id=${sessionId}`), value => value.current?.selection?.grant_id === chosen.grant_id, 'explicit grant persisted')
    const currentOptions = await access(`/model-options?session_id=${sessionId}`)
    assert.equal(currentOptions.current.available, true)
    assert.ok(!currentOptions.options.some(option => option.grant_id === chosen.grant_id), 'current selection is independent of the first page')
    assert.equal(await ownerPage.locator('[data-composer-input]').inputValue(), 'Keep this draft before configuring a model')
    await ownerPage.locator('[data-composer-input]').fill('')
    context.diagnostic('Empty session and paged explicit budget selection passed')

    assert.equal((await access('/sessions')).find(item => item.identity.session_id === sessionId).model_token_limit, 32768)
    await ownerPage.getByRole('button', { name: '更多会话操作', exact: true }).click()
    await ownerPage.getByRole('menuitem', { name: '任务用量上限', exact: true }).click()
    const initialLimit = ownerPage.getByRole('dialog', { name: '任务用量上限', exact: true })
    assert.equal(await initialLimit.getByLabel('每次任务的 token 上限', { exact: true }).inputValue(), '32768', 'empty Cloud sessions expose their saved limit')
    await initialLimit.getByRole('button', { name: '取消', exact: true }).click()

    for (const viewport of [{ width: 390, height: 844 }, { width: 320, height: 740 }, { width: 844, height: 390 }]) {
      await ownerPage.setViewportSize(viewport)
      dialog = await modelDirectory(ownerPage, `Published model · ${chosen.grant_name} · medium`)
      await assertLayout(ownerPage, dialog)
      assert.equal((await access(`/model-options?session_id=${sessionId}`)).current.selection.grant_id, chosen.grant_id)
      await ownerPage.screenshot({ path: path.join(artifacts, `managed-model-picker-${viewport.width}.png`) })
      await ownerPage.keyboard.press('Escape')
      if (viewport.width > 760) await ownerPage.keyboard.press('Escape')
      await until(() => ownerPage.getByRole('menu').count(), count => count === 0, 'model menu closes before changing viewport')
    }
    await ownerPage.setViewportSize({ width: 1440, height: 1000 })
    await memberPage.goto(origin)
    await memberPage.getByRole('button', { name: '使用组织账号登录', exact: true }).click()
    await selectSession(memberPage, team.tenant_id, sessionId)
    assert.equal(await memberPage.getByRole('button', { name: '平台管理', exact: true }).count(), 0, 'ordinary users have no platform administration entrance')
    await memberPage.getByRole('button', { name: '用户设置', exact: true }).click()
    const memberSettings = memberPage.locator('[data-user-settings]')
    await memberSettings.waitFor()
    assert.equal(await memberSettings.locator('a[href^="/admin"]').count(), 0)
    await memberSettings.getByRole('button', { name: '返回工作台', exact: true }).click()
    assert.equal((await serverRequest(origin, '/model-access/catalog', { token: oidc.accessToken() })).entitlements.length, 0)
    await memberPage.locator('[data-input-bar]').getByRole('button', { name: `Published model · ${chosen.grant_name} · medium`, exact: true }).waitFor()
    await until(() => memberPage.locator('[data-model-onboarding]').count(), count => count === 0, 'shared owner model ready without collaborator grant')
    const memberPicker = await modelDirectory(memberPage, `Published model · ${chosen.grant_name} · medium`)
    await memberPicker.getByRole('textbox', { name: '搜索模型或预算名称', exact: true }).fill(chosen.grant_name)
    await memberPicker.getByRole('button', { name: '搜索', exact: true }).click()
    const memberConfigured = memberPage.waitForResponse(response => response.request().method() === 'PATCH' && new URL(response.url()).pathname === `/api/v1/sessions/${sessionId}`)
    await memberPicker.locator(`[data-model-grant="${chosen.grant_id}"]`).getByRole('menuitem', { name: 'Published model same-model', exact: true }).click()
    assert.equal((await memberConfigured).status(), 200, 'a collaborator can configure their shared session model')
    await memberPage.locator('[data-composer-input]').fill('Write the managed proof')
    await until(() => memberPage.locator('[data-input-bar]').getByRole('button', { name: '发送', exact: true }).isEnabled(), Boolean, 'shared runtime is ready to submit after returning from settings')
    const queued = memberPage.waitForResponse(response => response.request().method() === 'POST' && new URL(response.url()).pathname === `/api/v1/sessions/${sessionId}/queue`)
    await memberPage.locator('[data-composer-input]').press('Enter')
    const sharedSubmission = await queued
    assert.equal(sharedSubmission.status(), 201)
    const acceptedShared = await sharedSubmission.json()
    const sharedRunId = acceptedShared.run_id
    const sharedEvents = await until(async () => {
      const events = await access(`/sessions/${sessionId}/events`)
      const failed = events.find(event => event.run_id === sharedRunId && ['turn_failed', 'turn_cancelled'].includes(event.type))
      if (failed) throw new Error(`Shared managed task terminated: ${JSON.stringify(failed)}`)
      return events
    }, events => events.some(event => event.run_id === sharedRunId && event.type === 'turn_finished'), 'shared managed task finished')
    await until(() => access(`/tenants/${team.tenant_id}/runs/${sharedRunId}`), value => value.run.state === 'succeeded', 'shared run settlement completed')
    assertPersistedSubmitter(acceptedShared, sharedEvents, member.user)
    await assertCollaboratorIdentity(ownerPage, memberPage, 'Write the managed proof', member.user)
    const workspacePath = path.join(workspaceRoot, createHash('sha256').update(team.tenant_id).digest('hex'), createHash('sha256').update(workspace.workspace_id).digest('hex'))
    assert.equal(await readFile(path.join(workspacePath, 'managed-model-proof.txt'), 'utf8'), 'Managed model access uses the explicitly selected owner budget.\n')
    const requests = (await serverRequest(origin, '/admin/models/requests', { token })).requests
    const shared = requests.filter(request => request.origin === 'workload' && request.actor_user_id === member.user.user_id)
    assert.ok(shared.length >= 2)
    assert.ok(shared.every(request => request.resource_owner_user_id === application.owner.session.user.user_id && request.grant_id === chosen.grant_id && request.source === 'platform_grant'))
    const textRequestSizes = shared.flatMap(request => request.attempts.map(attempt => ({ request_id: request.request_id, attempt: attempt.attempt, reserved_tokens: attempt.reserved_tokens, serialized_request_bytes: attempt.reserved_tokens - 4096 - defaults.max_output_tokens })))
    context.diagnostic(`Pure-text standard profile ModelRequest sizes: ${JSON.stringify(textRequestSizes)}`)
    assert.ok(upstream.calls.some(call => call.key === 'Bearer managed-platform-secret'))
    const ownRequests = (await serverRequest(origin, '/model-access/requests', { token })).requests
    assert.ok(ownRequests.some(request => request.actor_user_id === member.user.user_id), 'budget owner can see shared use of their model grant')
    const actorRequests = (await serverRequest(origin, '/model-access/requests', { token: oidc.accessToken() })).requests
    assert.ok(actorRequests.some(request => request.workload?.session_id === sessionId))
    for (const request of actorRequests) {
      assert.ok(!Object.hasOwn(request, 'provider_id') && !Object.hasOwn(request, 'upstream_model'))
      if (request.workload) assert.ok(!Object.hasOwn(request.workload, 'worker_id') && !Object.hasOwn(request.workload, 'lease_token'))
      assert.ok(request.attempts.every(attempt => !Object.hasOwn(attempt, 'upstream_request_id') && (attempt.error_code == null || ['access_denied', 'quota_exceeded', 'model_busy', 'cancelled', 'request_expired', 'stream_interrupted', 'model_configuration_changed', 'invalid_request', 'request_conflict', 'upstream_failed'].includes(attempt.error_code))))
    }
    context.diagnostic('Real shared Worker tool task used the owner grant and recorded the collaborator actor')

    const imagePrompt = ownerPage.locator('[data-composer-input]')
    await imagePrompt.evaluate(element => {
      const encoded = 'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII='
      const bytes = Uint8Array.from(atob(encoded), character => character.charCodeAt(0))
      const transfer = new DataTransfer()
      transfer.items.add(new File([bytes], 'managed-image.png', { type: 'image/png' }))
      element.dispatchEvent(new ClipboardEvent('paste', { bubbles: true, cancelable: true, clipboardData: transfer }))
    })
    const imagePreview = ownerPage.getByRole('button', { name: '预览 managed-image.webp', exact: true })
    await imagePreview.waitFor()
    await imagePrompt.fill('Describe the attached image after increasing this task budget')
    const beforeImage = upstream.calls.length
    const beforeImageRuns = await access(`/tenants/${team.tenant_id}/runs?limit=100`)
    const beforeImageQuota = (await access(`/tenants/${team.tenant_id}/model-usage`)).quota
    expectedBudgetRejection = true
    const rejectedImage = ownerPage.waitForResponse(response => response.request().method() === 'POST' && new URL(response.url()).pathname === `/api/v1/sessions/${sessionId}/queue`)
    await imagePrompt.press('Enter')
    const rejection = await rejectedImage
    assert.equal(rejection.status(), 400)
    const rejectionBody = await rejection.json()
    assert.match(rejectionBody.error.message, /at least 102048 tokens/)
    await ownerPage.getByRole('alert').filter({ hasText: 'at least 102048 tokens' }).first().waitFor()
    assert.equal(await imagePrompt.inputValue(), 'Describe the attached image after increasing this task budget')
    assert.equal(await imagePreview.count(), 1)
    assert.equal(upstream.calls.length, beforeImage, 'a task that cannot reserve its image budget never calls the model')
    assert.deepEqual(await access(`/tenants/${team.tenant_id}/runs?limit=100`), beforeImageRuns, 'rejected image submission does not enqueue a Run')
    assert.deepEqual((await access(`/tenants/${team.tenant_id}/model-usage`)).quota, beforeImageQuota, 'rejected image submission consumes no quota')
    assert.equal(expectedValidation.filter(value => value.startsWith('HTTP')).length, 1)
    expectedBudgetRejection = false
    await dismissNotifications(ownerPage)
    await ownerPage.getByRole('button', { name: '更多会话操作', exact: true }).click()
    await ownerPage.getByRole('menuitem', { name: '任务用量上限', exact: true }).click()
    const limitDialog = ownerPage.getByRole('dialog', { name: '任务用量上限', exact: true })
    for (const viewport of [{ width: 390, height: 844 }, { width: 320, height: 740 }, { width: 844, height: 390 }]) {
      await ownerPage.setViewportSize(viewport)
      await assertLayout(ownerPage, limitDialog)
      await ownerPage.screenshot({ path: path.join(artifacts, `task-limit-${viewport.width}.png`) })
    }
    await limitDialog.getByLabel('每次任务的 token 上限', { exact: true }).fill('262144')
    await limitDialog.getByRole('button', { name: '保存', exact: true }).click()
    await limitDialog.waitFor({ state: 'hidden' })
    await until(() => access('/sessions'), value => value.find(item => item.identity.session_id === sessionId)?.model_token_limit === 262144, 'larger future task budget persisted')
    assert.equal(await imagePrompt.inputValue(), 'Describe the attached image after increasing this task budget')
    assert.equal(await imagePreview.count(), 1)
    const acceptedImage = ownerPage.waitForResponse(response => response.request().method() === 'POST' && new URL(response.url()).pathname === `/api/v1/sessions/${sessionId}/queue`)
    await imagePrompt.press('Enter')
    const imageResponse = await acceptedImage
    assert.equal(imageResponse.status(), 201)
    const imageRun = (await imageResponse.json()).run_id
    await until(() => access(`/tenants/${team.tenant_id}/runs/${imageRun}`), value => value.run.state === 'succeeded', 'same image executes after raising the task limit')
    assert.ok(upstream.calls.slice(beforeImage).some(call => call.body.messages.some(message => Array.isArray(message.content) && message.content.some(part => part.type === 'image_url'))), 'the model receives the actual image')
    await ownerPage.setViewportSize({ width: 1440, height: 1000 })
    context.diagnostic('Image submission retained its draft and attachment after an insufficient-limit rejection, then succeeded through the real Worker with a larger saved limit')

    await serverRequest(origin, `/providers?session_id=${sessionId}`, { token, tenantId: team.tenant_id, body: { ...profile, id: 'private-byok', display_name: 'Old space source must not be selected', api_key_ref: null } })
    await ownerPage.getByRole('button', { name: '我的模型', exact: true }).click()
    const accountCenter = ownerPage.locator('[data-provider-scope="account"]')
    await accountCenter.getByRole('button', { name: '添加 Provider', exact: true }).first().click()
    const accountEditor = accountCenter.locator('[data-provider-editor="new"]')
    await accountEditor.getByLabel('Provider ID', { exact: true }).fill('private-byok')
    await accountEditor.getByLabel('显示名称', { exact: true }).fill('Private account BYOK')
    await accountEditor.getByLabel('API 地址', { exact: true }).fill(upstream.baseUrl)
    await selectChoice(accountEditor.locator('[id$="-provider-protocol"]'), 'openai-chat-completions')
    await accountEditor.getByLabel('API Key', { exact: true }).fill('account-worker-secret')
    await accountEditor.locator('[id$="-provider-defaults-context"]').fill('100K')
    await accountEditor.locator('[id$="-provider-defaults-output"]').fill('2048')
    await accountEditor.getByRole('switch', { name: '启用 Provider 默认模型设置 的推理强度', exact: true }).click()
    await accountEditor.getByLabel('模型 ID 1', { exact: true }).fill('same-model')
    const accountSaved = ownerPage.waitForResponse(response => response.request().method() === 'POST' && new URL(response.url()).pathname === '/api/v1/providers')
    await accountEditor.getByRole('button', { name: '添加 Provider', exact: true }).click()
    const accountResponse = await accountSaved
    assert.equal(accountResponse.status(), 200)
    assert.equal(accountResponse.request().headers()['x-ternilo-tenant'], application.owner.session.personal_tenant_id)
    const accountProfile = await accountResponse.json()
    await accountEditor.waitFor({ state: 'hidden' })
    await serverRequest(origin, '/providers', { token, tenantId: application.owner.session.personal_tenant_id, body: { ...accountProfile, id: 'not-shared', display_name: 'Unshared private account source' } })
    const beforeOwnerSelection = await serverRequest(origin, `/model-options?session_id=${sessionId}`, { token: oidc.accessToken(), tenantId: team.tenant_id })
    assert.deepEqual(beforeOwnerSelection.providers, [], 'a shared platform session does not expose private account Providers')
    await assert.rejects(() => serverRequest(origin, `/sessions/${sessionId}`, { token: oidc.accessToken(), tenantId: team.tenant_id, method: 'PATCH', body: { model: { provider: 'named_provider', provider_id: 'private-byok', model: 'same-model' } } }), /403.*only the resource owner/)
    await ownerPage.getByRole('link', { name: '返回工作台', exact: true }).click()
    const byokInventoryLoaded = ownerPage.waitForResponse(response => response.request().method() === 'GET' && new URL(response.url()).pathname === '/api/v1/model-options')
    await ownerPage.locator('[data-input-bar]').getByRole('button', { name: `Published model · ${chosen.grant_name} · medium`, exact: true }).click()
    await byokInventoryLoaded
    await ownerPage.getByRole('menuitem', { name: /^模型/ }).click()
    await ownerPage.locator('[data-model-provider="private-byok"]').getByRole('menuitem', { name: 'same-model same-model', exact: true }).waitFor()
    await ownerPage.screenshot({ path: path.join(artifacts, 'byok-model-submenu.png') })
    const byokOption = ownerPage.locator('[data-model-provider="private-byok"]').getByRole('menuitem', { name: 'same-model same-model', exact: true })
    await byokOption.click()
    await until(() => access(`/model-options?session_id=${sessionId}`), value => value.current?.selection?.provider === 'named_provider', 'BYOK explicitly selected')
    const selectedByok = (await access(`/model-options?session_id=${sessionId}`)).current.selection
    assert.deepEqual(selectedByok, { provider: 'named_provider', provider_id: 'private-byok', model: 'same-model' })
    await ownerPage.locator('[data-input-bar]').getByRole('button', { name: 'same-model · 账号自有 · medium', exact: true }).waitFor()
    assert.match(await ownerPage.locator('[data-input-bar]').textContent(), /账号自有/)
    for (const interaction of ['keyboard', 'touch']) {
      await dismissNotifications(ownerPage)
      await ownerPage.setViewportSize(interaction === 'touch' ? { width: 390, height: 844 } : { width: 1440, height: 1000 })
      const trigger = ownerPage.locator('[data-input-bar]').getByRole('button', { name: 'same-model · 账号自有 · medium', exact: true })
      await until(() => trigger.isEnabled(), Boolean, 'model selection finishes before keyboard or touch input')
      if (interaction === 'touch') await trigger.tap(); else await trigger.press('Enter')
      const submenu = ownerPage.getByRole('menuitem', { name: /^模型/ })
      if (interaction === 'touch') await submenu.tap(); else await submenu.press('ArrowRight')
      const option = ownerPage.locator('[data-model-provider="private-byok"]').getByRole('menuitem', { name: 'same-model same-model', exact: true })
      await option.waitFor()
      if (interaction === 'touch') {
        const menu = ownerPage.getByRole('menu').filter({ has: option })
        await assertLayout(ownerPage, menu)
        const bounds = await option.boundingBox()
        assert.ok(bounds && bounds.height >= 40, 'BYOK option is a usable touch target')
        const geometry = await menu.evaluate(element => ({ menu: element.getBoundingClientRect().toJSON(), viewportWidth: window.visualViewport?.width ?? window.innerWidth }))
        assert.ok(geometry.menu.left >= 11.9 && geometry.menu.right <= geometry.viewportWidth - 11.9, 'the entire model menu keeps a visible margin on both edges')
        mobileModelGeometry.push(geometry)
      }
      await ownerPage.screenshot({ path: path.join(artifacts, `byok-model-${interaction}.png`) })
      const saved = ownerPage.waitForResponse(response => response.request().method() === 'PATCH' && new URL(response.url()).pathname === `/api/v1/sessions/${sessionId}`)
      if (interaction === 'touch') await option.tap(); else await option.press('Enter')
      const response = await saved
      assert.equal(response.status(), 200)
      assert.deepEqual(response.request().postDataJSON().model, selectedByok)
      await until(() => ownerPage.getByRole('menu').count(), count => count === 0, `${interaction} selection closes the model menu`)
    }
    await ownerPage.setViewportSize({ width: 1440, height: 1000 })
    context.diagnostic('BYOK selection works with direct mouse movement, keyboard, and mobile touch')
    await memberPage.locator('[data-input-bar]').getByRole('button', { name: 'same-model · 账号自有 · medium', exact: true }).waitFor()
    const beforeByok = upstream.calls.length
    await memberPage.locator('[data-composer-input]').fill('Write the managed proof with the owner account model')
    await until(() => memberPage.locator('[data-input-bar]').getByRole('button', { name: '发送', exact: true }).isEnabled(), Boolean, 'shared BYOK runtime is ready to submit')
    const byokQueued = memberPage.waitForResponse(response => response.request().method() === 'POST' && new URL(response.url()).pathname === `/api/v1/sessions/${sessionId}/queue`)
    await memberPage.locator('[data-composer-input]').press('Enter')
    const byokResponse = await byokQueued
    assert.equal(byokResponse.status(), 201)
    const acceptedByok = await byokResponse.json()
    const byokRun = acceptedByok.run_id
    const byokEvents = await until(() => access(`/sessions/${sessionId}/events`), events => events.some(event => event.run_id === byokRun && event.type === 'turn_finished'), 'shared BYOK task finished')
    await until(() => access(`/tenants/${team.tenant_id}/runs/${byokRun}`), value => value.run.state === 'succeeded', 'BYOK run settlement completed')
    assertPersistedSubmitter(acceptedByok, byokEvents, member.user)
    await assertCollaboratorIdentity(ownerPage, memberPage, 'Write the managed proof with the owner account model', member.user)
    const byokRequests = (await serverRequest(origin, '/admin/models/requests', { token })).requests.filter(request => request.workload?.run_id === byokRun)
    assert.ok(byokRequests.length > 0)
    assert.ok(byokRequests.every(request => request.source === 'user_provider' && request.grant_id === null && request.key_id === null && request.actor_user_id === member.user.user_id && request.resource_owner_user_id === application.owner.session.user.user_id))
    assert.ok(upstream.calls.slice(beforeByok).every(call => call.key === 'Bearer account-worker-secret'), 'the account source is not confused with same-name execution-space or platform models')
    assert.equal(await readFile(path.join(workspacePath, 'managed-model-proof.txt'), 'utf8'), 'Account model from My Models executes in a team workspace.\n')
    assert.ok(byokRequests.every(request => request.workload.model.tenant_id === application.owner.session.personal_tenant_id && request.workload.tenant_id === team.tenant_id))
    const sharedCatalog = await serverRequest(origin, `/model-options?session_id=${sessionId}`, { token: oidc.accessToken(), tenantId: team.tenant_id })
    assert.equal(sharedCatalog.providers.find(provider => provider.id === 'private-byok').base_url, '')
    assert.equal(sharedCatalog.providers.length, 1, 'only the explicitly selected account Provider is shared')
    await assert.rejects(() => serverRequest(origin, `/sessions/${sessionId}`, { token: oidc.accessToken(), tenantId: team.tenant_id, method: 'PATCH', body: { model: { provider: 'named_provider', provider_id: 'not-shared', model: 'same-model' } } }), /403.*only the resource owner/)
    assert.equal(JSON.stringify(sharedCatalog).includes('account-worker-secret'), false)
    const usage = await access(`/tenants/${team.tenant_id}/model-usage`)
    assert.ok(usage.ledger.some(entry => entry.run_id === byokRun && entry.actor_user_id === member.user.user_id && typeof entry.request_id === 'string' && entry.attempt >= 1))
    assert.equal(usage.totals.total_tokens, usage.ledger.reduce((sum, entry) => sum + (entry.accounted_tokens ?? 0), 0), 'attempts and logical requests are not billed twice')
    assert.equal(usage.quota.settled_tokens, usage.totals.total_tokens)
    assert.equal(usage.quota.active_reserved_tokens, 0)
    assert.equal(usage.quota.unknown_reserved_tokens, 0)
    context.diagnostic('Shared BYOK task and unified space usage ledger passed')

    const beforeRevocation = upstream.calls.length
    await serverRequest(origin, `/credentials/${encodeURIComponent(accountProfile.api_key_ref)}`, { token, tenantId: application.owner.session.personal_tenant_id, method: 'DELETE' })
    const revokedAccount = await access(`/model-options?session_id=${sessionId}`)
    assert.equal(revokedAccount.current.available, false)
    assert.equal(revokedAccount.current.selection.provider_id, 'private-byok')
    const beforeDeniedRuns = await access(`/tenants/${team.tenant_id}/runs?limit=100`)
    await assert.rejects(() => serverRequest(origin, `/sessions/${sessionId}/queue`, { token, tenantId: team.tenant_id, body: { content: { kind: 'prompt', input: 'Do not substitute another model after revocation' } } }), /403.*credential/)
    assert.deepEqual(await access(`/tenants/${team.tenant_id}/runs?limit=100`), beforeDeniedRuns)
    assert.equal(upstream.calls.length, beforeRevocation)
    context.diagnostic('Account source remains unavailable after key deletion despite a same-name execution-space Provider')

    dialog = await modelDirectory(ownerPage, 'same-model · 账号自有 · medium')
    await dialog.getByRole('textbox', { name: '搜索模型或预算名称', exact: true }).fill(chosen.grant_name)
    await dialog.getByRole('button', { name: '搜索', exact: true }).click()
    const restoredDefaultSaved = ownerPage.waitForResponse(response => response.request().method() === 'PUT' && new URL(response.url()).pathname === '/api/v1/default-model' && response.request().postDataJSON()?.grant_id === chosen.grant_id)
    await dialog.locator(`[data-model-grant="${chosen.grant_id}"]`).getByRole('menuitem', { name: 'Published model same-model', exact: true }).click()
    await dialog.waitFor({ state: 'hidden' })
    await until(() => access(`/model-options?session_id=${sessionId}`), value => value.current?.selection?.grant_id === chosen.grant_id, 'platform budget restored explicitly')
    assert.equal((await restoredDefaultSaved).status(), 200, 'the owner default finishes saving before the separate grant revocation')
    await serverRequest(origin, `/admin/models/grants/${chosen.grant_id}`, { token, method: 'DELETE' })
    await ownerPage.locator('[data-input-bar]').getByRole('button', { name: `Published model · ${chosen.grant_name} · medium`, exact: true }).click()
    await until(() => ownerPage.locator('[data-model-onboarding]').count(), count => count === 1, 'revoked grant blocks next submit')
    await ownerPage.keyboard.press('Escape')
    await until(() => ownerPage.locator('[role="menu"]').count(), count => count === 0, 'the model menu finishes closing before capturing the revoked state')
    const revoked = await access(`/model-options?session_id=${sessionId}`)
    assert.equal(revoked.current.available, false)
    assert.equal(revoked.current.selection.grant_id, chosen.grant_id)
    assert.ok(revoked.options.some(option => option.model.model_id === 'same-model' && option.grant_id !== chosen.grant_id))
    await ownerPage.screenshot({ path: path.join(artifacts, 'managed-model-revoked-desktop.png') })
    await ownerPage.setViewportSize({ width: 390, height: 844 })
    await ownerPage.locator('[data-app-frame][data-mobile]').waitFor()
    await until(() => ownerPage.locator('[data-app-sidebar-column]').evaluate(element => element.getBoundingClientRect().right), right => right <= 0, 'the closed mobile sidebar finishes its transition')
    assert.ok(await ownerPage.evaluate(() => document.documentElement.scrollWidth <= innerWidth))
    assert.equal(await ownerPage.locator('[data-input-bar]').getByRole('button', { name: '发送', exact: true }).isDisabled(), true)
    const tailButton = ownerPage.getByRole('button', { name: '回到底部', exact: true })
    if (await tailButton.isVisible()) {
      await until(async () => {
        const tail = await tailButton.boundingBox()
        const notice = await ownerPage.locator('[data-model-onboarding]').boundingBox()
        return tail && notice && tail.y + tail.height <= notice.y
      }, Boolean, 'the return-to-bottom button stays above the model configuration notice')
    }
    await ownerPage.screenshot({ path: path.join(artifacts, 'managed-model-revoked-mobile.png') })
    const beforeServiceInspection = upstream.calls.length
    await ownerPage.getByRole('button', { name: '更多会话操作', exact: true }).tap()
    const servicesLoaded = ownerPage.waitForResponse(response => response.request().method() === 'GET' && new URL(response.url()).pathname === `/api/v1/sessions/${sessionId}/services`)
    await ownerPage.getByRole('menuitem', { name: '后台服务', exact: true }).tap()
    const servicesResponse = await servicesLoaded
    assert.equal(servicesResponse.status(), 200, 'an idle Cloud session permits service inspection')
    assert.deepEqual(await servicesResponse.json(), [], 'idle Cloud inspection does not construct a runtime')
    const servicesDialog = ownerPage.getByRole('dialog', { name: '后台服务', exact: true })
    await servicesDialog.getByText('当前没有后台服务。', { exact: true }).waitFor()
    await servicesDialog.getByText('当前任务', { exact: true }).waitFor()
    const servicesDescription = await servicesDialog.textContent()
    assert.match(servicesDescription, /任务结束后，这些服务会一起停止/)
    assert.doesNotMatch(servicesDescription, /跨任务|继续使用工作目录|保持目录占用/, 'Cloud guidance does not promise persistent services or a directory lease across tasks')
    await assertLayout(ownerPage, servicesDialog)
    await ownerPage.screenshot({ path: path.join(artifacts, 'managed-services-idle-mobile.png') })
    await servicesDialog.getByRole('button', { name: '关闭', exact: true }).tap()
    await servicesDialog.waitFor({ state: 'hidden' })
    assert.equal(upstream.calls.length, beforeServiceInspection, 'service inspection never calls a model')
    context.diagnostic('Idle Cloud service inspection returned HTTP 200 with no services and showed task-scoped guidance on mobile without overflow')
    assert.deepEqual(errors, [])
    assert.deepEqual(memberDefaultWrites, [], 'shared Configure permission never changes the owner default model')
    await writeFile(path.join(artifacts, 'managed-model-browser-result.json'), JSON.stringify({ assetHashes, sessionId, selectedBudget: chosen.grant_name, requests: shared.map(request => ({ request_id: request.request_id, actor_user_id: request.actor_user_id, grant_id: request.grant_id, accounted_tokens: request.accounted_tokens })), upstreamCalls: upstream.calls.length, requestSizes: upstream.calls.map(call => call.bytes), textRequestSizes, mobileModelGeometry, expectedValidation, errors }, null, 2))
  } catch (error) {
    if (application) {
      const token = application.owner.session.access_token
      const tenants = (await serverRequest(application.origin, '/tenants', { token }).catch(() => ({ tenants: [] }))).tenants
      const state = await Promise.all(tenants.map(async tenant => ({
        tenant: tenant.tenant_id,
        state: await serverRequest(application.origin, '/state', { token, tenantId: tenant.tenant_id }).catch(cause => String(cause)),
        runs: await serverRequest(application.origin, `/tenants/${tenant.tenant_id}/runs`, { token }).catch(cause => String(cause)),
      })))
      await writeFile(path.join(artifacts, 'managed-runtime-failure.json'), JSON.stringify({ state, workerExit: worker?.child?.exitCode, workerSignal: worker?.child?.signalCode }, null, 2))
    }
    const failedRequestReservations = application ? await serverRequest(application.origin, '/admin/models/requests', { token: application.owner.session.access_token }).then(value => value.requests.map(request => ({ request_id: request.request_id, state: request.state, error_code: request.error_code, attempts: request.attempts.map(attempt => ({ attempt: attempt.attempt, state: attempt.state, reserved_tokens: attempt.reserved_tokens, accounted_tokens: attempt.accounted_tokens, error_code: attempt.error_code })) }))).catch(() => []) : []
    await writeFile(path.join(artifacts, 'managed-model-failure-sizes.json'), JSON.stringify({ requestSizes: upstream.calls.map(call => call.bytes), failedRequestReservations, expectedValidation, errors }, null, 2))
    if (ownerPage) await ownerPage.screenshot({ path: path.join(artifacts, 'managed-model-failure.png') }).catch(() => {})
    if (memberPage) await memberPage.screenshot({ path: path.join(artifacts, 'managed-model-member-failure.png') }).catch(() => {})
    context.diagnostic(`Server: ${application?.diagnostics() ?? ''}\nWorker: ${worker?.diagnostics() ?? ''}`)
    throw error
  } finally {
    await browser?.close()
    await stopProcess(worker)
    await stopProcess(application)
    await upstream.close(); await oidc.close()
    if (!process.env.TERNILO_E2E_KEEP_TEMP) await rm(directory, { recursive: true, force: true })
  }
})
