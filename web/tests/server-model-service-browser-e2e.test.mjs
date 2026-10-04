import { selectChoice } from './browser-select-fixture.mjs'
import assert from 'node:assert/strict'
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { createServer } from 'node:http'
import path from 'node:path'
import { tmpdir } from 'node:os'
import test from 'node:test'
import { chromium } from 'playwright'
import { freePort, initializeServer, repository, registerOidcUser, serverRequest, startOidcServer, startProcess, stopProcess, waitForHttp } from './platform-e2e-fixture.mjs'

async function until(read, predicate, label) {
  const deadline = Date.now() + 30_000
  while (Date.now() < deadline) {
    const value = await read()
    if (predicate(value)) return value
    await new Promise(resolve => setTimeout(resolve, 100))
  }
  throw new Error(`Timed out: ${label}`)
}

async function upstreamFixture() {
  const key = 'browser-upstream-secret'
  const requests = []
  const streams = new Set()
  const server = createServer((request, response) => {
    if (request.headers.authorization !== `Bearer ${key}`) { response.writeHead(401); response.end(); return }
    if (request.method === 'GET' && request.url === '/v1/models') {
      response.writeHead(200, { 'content-type': 'application/json' }); response.end(JSON.stringify({ data: [{ id: 'upstream-alpha', protocol: 'openai-chat-completions' }, { id: 'upstream-beta', protocol: 'openai-responses' }] })); return
    }
    if (request.method !== 'POST' || !['/v1/chat/completions', '/v1/responses'].includes(request.url)) { response.writeHead(404); response.end(); return }
    const chunks = []
    request.on('data', chunk => chunks.push(chunk))
    request.on('end', () => {
      const body = JSON.parse(Buffer.concat(chunks).toString())
      requests.push(body)
      const content = 'Model service fixture completed the local task.'
      const identity = { id: `fixture-${requests.length}`, created: 1, model: body.model }
      if (JSON.stringify(body).includes('hold-model-stream')) {
        response.writeHead(200, { 'content-type': 'text/event-stream' })
        response.write(`data: ${JSON.stringify({ ...identity, object: 'chat.completion.chunk', choices: [{ index: 0, delta: { content: 'Waiting for revocation' }, finish_reason: null }] })}\n\n`)
        streams.add(response)
        response.on('close', () => streams.delete(response))
        return
      }
      if (request.url === '/v1/responses') {
        const result = { id: identity.id, object: 'response', model: body.model, status: 'completed', output: [{ type: 'message', role: 'assistant', content: [{ type: 'output_text', text: content }] }], usage: { input_tokens: 24, output_tokens: 8, total_tokens: 32, input_tokens_details: { cached_tokens: 6 }, output_tokens_details: { reasoning_tokens: 2 } } }
        if (body.stream) {
          response.writeHead(200, { 'content-type': 'text/event-stream' })
          response.end(`event: response.output_text.delta\ndata: ${JSON.stringify({ type: 'response.output_text.delta', output_index: 0, content_index: 0, delta: content })}\n\nevent: response.completed\ndata: ${JSON.stringify({ type: 'response.completed', response: result })}\n\n`)
        } else { response.writeHead(200, { 'content-type': 'application/json' }); response.end(JSON.stringify(result)) }
        return
      }
      const needsTool = JSON.stringify(body.messages).includes('Run a local task') && body.tools?.some(tool => tool.function?.name === 'write_file') && !body.messages.some(message => message.role === 'tool')
      const toolCalls = [{ index: 0, id: 'local-write-proof', type: 'function', function: { name: 'write_file', arguments: JSON.stringify({ path: 'model-service-proof.txt', content: 'Created by the local harness through the platform model service.\n' }) } }]
      const usage = { prompt_tokens: 24, completion_tokens: 8, total_tokens: 32, prompt_tokens_details: { cached_tokens: 6 }, completion_tokens_details: { reasoning_tokens: 2 } }
      if (!body.stream) {
        response.writeHead(200, { 'content-type': 'application/json' }); response.end(JSON.stringify({ ...identity, object: 'chat.completion', choices: [{ index: 0, message: { role: 'assistant', content }, finish_reason: 'stop' }], usage })); return
      }
      const sse = event => `data: ${JSON.stringify(event)}\n\n`
      response.writeHead(200, { 'content-type': 'text/event-stream' })
      response.end(sse({ ...identity, object: 'chat.completion.chunk', choices: [{ index: 0, delta: needsTool ? { role: 'assistant', tool_calls: toolCalls } : { role: 'assistant', content }, finish_reason: null }] })
        + sse({ ...identity, object: 'chat.completion.chunk', choices: [{ index: 0, delta: {}, finish_reason: needsTool ? 'tool_calls' : 'stop' }] })
        + sse({ ...identity, object: 'chat.completion.chunk', choices: [], usage }) + 'data: [DONE]\n\n')
    })
  })
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve))
  return { key, requests, baseUrl: `http://127.0.0.1:${server.address().port}/v1`, close: () => { for (const stream of streams) stream.destroy(); return new Promise(resolve => server.close(resolve)) } }
}

async function assertLayout(page, locator) {
  // Viewport emulation can return before the application's resize handler runs.
  await page.waitForFunction(() => Number.parseFloat(document.documentElement.style.getPropertyValue('--ternilo-visual-viewport-height'))
    === Math.round(window.visualViewport?.height ?? window.innerHeight))
  await locator.evaluate(async element => {
    await Promise.all(element.getAnimations({ subtree: true }).filter(animation => animation.effect?.getTiming().iterations !== Infinity).map(animation => animation.finished.catch(() => {})))
  })
  assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), 'page stays within the viewport')
  assert.ok(await locator.evaluate(element => element.scrollWidth <= element.clientWidth), 'surface has no horizontal overflow')
  const bounds = await locator.boundingBox()
  assert.ok(bounds && bounds.x >= 0 && bounds.y >= 0 && bounds.x + bounds.width <= page.viewportSize().width + 1 && bounds.y + bounds.height <= page.viewportSize().height + 1, `surface remains inside the viewport: ${JSON.stringify({ bounds, viewport: page.viewportSize() })}`)
  for (const button of await locator.getByRole('button').all()) {
    const box = await button.boundingBox()
    if (box) assert.ok(box.height >= 39.9 && box.width >= 39.9, 'buttons have usable touch targets')
  }
}

test('platform model access works without workers or machine enrollment and stays usable on mobile', { timeout: 360_000 }, async context => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-model-service-browser-'))
  const artifacts = process.env.TERNILO_E2E_ARTIFACT_DIR ?? directory
  await mkdir(artifacts, { recursive: true })
  const identities = Object.fromEntries(Array.from({ length: 27 }, (_, index) => { const id = String(index).padStart(2, '0'); return [id, { subject: `model-member-${id}`, email: `model-member-${id}@example.test`, name: `Model member ${id}` }] }))
  const oidc = await startOidcServer({ audience: 'model-browser', identities, initialIdentity: '00' })
  const upstream = await upstreamFixture()
  let application, local, browser
  const errors = []
  try {
    const origin = `http://127.0.0.1:${await freePort()}`
    application = await initializeServer({ directory: path.join(directory, 'server'), origin, oidc: { issuer: oidc.issuer, audience: 'model-browser', client_id: 'model-browser' }, managedExecutionEnabled: false })
    const ownerToken = application.owner.session.access_token
    const registrationPolicy = await serverRequest(origin, '/admin/registration', { token: ownerToken })
    await serverRequest(origin, '/admin/registration', { token: ownerToken, method: 'PATCH', body: { mode: 'open', require_approval: false, revision: registrationPolicy.revision } })
    const members = []
    for (const id of Object.keys(identities).sort()) members.push(await registerOidcUser(origin, oidc.accessToken(id), `model-member-${id}`))
    const member = members[0]
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
    const ownerContext = await browser.newContext({ locale: 'zh-CN', viewport: { width: 1440, height: 1000 }, serviceWorkers: 'block' })
    const memberContext = await browser.newContext({ locale: 'zh-CN', viewport: { width: 1280, height: 900 }, serviceWorkers: 'block', permissions: ['clipboard-read', 'clipboard-write'] })
    const owner = await ownerContext.newPage()
    const user = await memberContext.newPage()
    for (const page of [owner, user]) {
      page.on('pageerror', error => errors.push(`page: ${error.message}`))
      page.on('console', message => { if (message.type() === 'error') errors.push(`console: ${message.text()}`) })
      page.on('response', response => { if (response.status() >= 400) errors.push(`HTTP ${response.status()}: ${new URL(response.url()).pathname}`) })
    }
    await owner.goto(`${origin}/admin/models`)
    await owner.getByLabel('用户名', { exact: true }).fill(application.owner.username)
    await owner.getByLabel('密码', { exact: true }).fill(application.owner.password)
    await owner.getByRole('button', { name: '登录', exact: true }).click()
    await owner.locator('[data-model-administration]').waitFor()
    await owner.getByRole('tab', { name: '上游接入', exact: true }).click()
    await owner.getByRole('button', { name: '添加上游', exact: true }).click()
    let dialog = owner.getByRole('dialog', { name: '添加上游', exact: true })
    await dialog.getByLabel('Provider ID', { exact: true }).fill('model-browser')
    await dialog.getByLabel('显示名称', { exact: true }).fill('Browser upstream')
    await dialog.getByLabel('API 地址', { exact: true }).fill(upstream.baseUrl)
    await selectChoice(dialog.getByLabel('API 协议', { exact: true }), 'openai-chat-completions')
    await dialog.getByLabel('API Key', { exact: true }).fill(upstream.key)
    await dialog.getByRole('button', { name: '获取可用模型', exact: true }).click()
    const discovered = owner.getByRole('dialog', { name: '选择要添加的模型', exact: true })
    await discovered.getByRole('button', { name: '应用所选', exact: true }).click()
    await dialog.locator('summary').filter({ hasText: '请求与重试' }).click()
    await dialog.locator('[id$="-provider-attempts"]').fill('3')
    assert.equal(await dialog.locator('[id$="-provider-attempts"]').inputValue(), '3')
    assert.match(await dialog.textContent(), /\/v1 请求只调用上游一次/)
    const providerCreated = owner.waitForResponse(response => response.request().method() === 'POST' && new URL(response.url()).pathname === '/api/v1/admin/models/providers')
    await dialog.getByRole('button', { name: '添加 Provider', exact: true }).click()
    const provider = await (await providerCreated).json()
    assert.equal(provider.has_api_key, true)
    assert.equal(provider.profile.max_attempts, 3, 'managed retry configuration is saved independently from the single-attempt public gateway')
    assert.ok(!JSON.stringify(provider).includes(upstream.key), 'provider reads do not return the upstream key')
    await dialog.waitFor({ state: 'hidden' })
    await owner.locator('[data-model-provider="model-browser"]').getByRole('button', { name: '编辑', exact: true }).click()
    dialog = owner.getByRole('dialog', { name: '编辑上游', exact: true })
    assert.equal(await dialog.getByLabel('API Key', { exact: true }).inputValue(), '')
    const preserved = owner.waitForRequest(request => request.method() === 'PUT' && new URL(request.url()).pathname === '/api/v1/admin/models/providers/model-browser')
    await dialog.getByRole('button', { name: '保存', exact: true }).click()
    assert.equal((await preserved).postDataJSON().api_key, undefined, 'blank input preserves the saved key')
    await dialog.waitFor({ state: 'hidden' })
    await owner.getByRole('tab', { name: '平台模型', exact: true }).click()
    await owner.getByRole('button', { name: '发布模型', exact: true }).click()
    dialog = owner.getByRole('dialog', { name: '发布模型', exact: true })
    await dialog.locator('#publication-id').fill('platform-alpha')
    await dialog.locator('#publication-name').fill('Platform Alpha')
    await dialog.getByRole('button', { name: /Browser upstream.*model-browser/ }).click()
    await selectChoice(dialog.locator('#publication-upstream-model'), 'upstream-alpha')
    await dialog.getByRole('button', { name: '保存', exact: true }).click()
    await dialog.waitFor({ state: 'hidden' })
    await owner.locator('[data-published-model="platform-alpha"]').waitFor()
    await serverRequest(origin, '/admin/models/providers', { token: ownerToken, body: { profile: { ...provider.profile, id: 'response-browser', display_name: 'Responses upstream', protocol: 'openai-responses', models: [{ id: 'upstream-beta', settings: { mode: 'inherit' } }] }, enabled: true, api_key: upstream.key } })
    await owner.getByRole('button', { name: '发布模型', exact: true }).click()
    dialog = owner.getByRole('dialog', { name: '发布模型', exact: true })
    await dialog.locator('#publication-id').fill('platform-beta')
    await dialog.locator('#publication-name').fill('Platform Beta')
    await dialog.getByRole('button', { name: /Responses upstream.*response-browser/ }).click()
    await selectChoice(dialog.locator('#publication-upstream-model'), 'upstream-beta')
    await dialog.getByRole('button', { name: '保存', exact: true }).click()
    await dialog.waitFor({ state: 'hidden' })
    await owner.locator('[data-published-model="platform-beta"]').waitFor()
    context.diagnostic('Published Chat Completions and Responses models through administration')
    await owner.getByRole('tab', { name: '授权与额度', exact: true }).click()
    await owner.getByRole('button', { name: '新建用户组', exact: true }).click()
    dialog = owner.getByRole('dialog', { name: '新建用户组', exact: true })
    await dialog.locator('#model-group-name').fill('Model subscribers')
    const groupCreated = owner.waitForResponse(response => response.request().method() === 'POST' && new URL(response.url()).pathname === '/api/v1/admin/models/groups')
    await dialog.getByRole('button', { name: '保存', exact: true }).click()
    const group = await (await groupCreated).json()
    dialog = owner.getByRole('dialog', { name: '管理用户组', exact: true })
    await dialog.getByRole('button', { name: '添加账号', exact: true }).click()
    await until(() => dialog.locator('[data-model-group-member]').count(), count => count === 25, 'first platform-account candidate page')
    const firstCandidates = await dialog.locator('[data-model-group-member]').evaluateAll(rows => rows.map(row => row.dataset.modelGroupMember))
    await dialog.getByRole('button', { name: '下一页', exact: true }).click()
    await until(() => dialog.locator('[data-model-group-member]').count(), count => count === 3, 'second platform-account candidate page')
    const lastCandidates = await dialog.locator('[data-model-group-member]').evaluateAll(rows => rows.map(row => row.dataset.modelGroupMember))
    assert.equal(new Set([...firstCandidates, ...lastCandidates]).size, 28)
    await dialog.getByRole('textbox', { name: '搜索平台账号', exact: true }).fill(member.user.username)
    const searchedMember = owner.waitForResponse(response => response.request().method() === 'GET' && new URL(response.url()).pathname.endsWith('/candidates') && new URL(response.url()).searchParams.get('query') === member.user.username)
    await dialog.getByRole('button', { name: '搜索', exact: true }).click()
    assert.equal((await searchedMember).status(), 200)
    const memberRow = dialog.locator(`[data-model-group-member="${member.user.user_id}"]`)
    await memberRow.getByRole('button', { name: '加入', exact: true }).click()
    await memberRow.getByRole('button', { name: '已加入', exact: true }).waitFor()
    await until(() => dialog.getByRole('button', { name: '返回组员列表', exact: true }).isEnabled(), Boolean, 'group member addition settled')
    await dialog.locator('[data-slot="dialog-close"]').click()
    await dialog.waitFor({ state: 'hidden' })
    const memberSpaces = await serverRequest(origin, '/tenants', { token: oidc.accessToken() })
    assert.equal(memberSpaces.tenants.length, 1, 'platform model membership does not add team membership')
    assert.equal(memberSpaces.tenants[0].kind, 'personal')
    context.diagnostic('Platform account paging and model-group membership preserved personal spaces')
    await owner.getByRole('button', { name: '分配模型与额度', exact: true }).click()
    dialog = owner.getByRole('dialog', { name: '分配模型与额度', exact: true })
    await dialog.locator('#model-grant-name').fill('Subscriber shared budget')
    await selectChoice(dialog.locator('#model-grant-kind'), 'group')
    await dialog.getByRole('button', { name: /Model subscribers/ }).click()
    await dialog.getByRole('button', { name: /Platform Alpha.*platform-alpha/ }).click()
    await dialog.getByRole('button', { name: /Platform Beta.*platform-beta/ }).click()
    await dialog.locator('#model-grant-tokens').fill('1000000')
    await dialog.locator('#model-grant-concurrency').fill('2')
    assert.match(await dialog.textContent(), /整组共享额度/)
    await dialog.getByRole('button', { name: '保存', exact: true }).click()
    await dialog.waitFor({ state: 'hidden' })
    await user.goto(`${origin}/models`)
    await user.getByRole('button', { name: '使用组织账号登录', exact: true }).click()
    await user.waitForURL(`${origin}/models`)
    await user.getByRole('button', { name: '平台授权', exact: true }).click()
    await user.locator('[data-model-entitlement]').waitFor()
    assert.equal(await user.locator('a[href="/admin/models"]').count(), 0)
    assert.match(await user.locator('[data-model-access]').textContent(), /OpenAI Chat Completions/)
    await user.getByRole('button', { name: '创建接入密钥', exact: true }).click()
    dialog = user.getByRole('dialog', { name: '创建接入密钥', exact: true })
    await dialog.locator('#model-key-name').fill('My standalone laptop')
    await dialog.locator('#model-key-tokens').fill('500000')
    const keyCreated = user.waitForResponse(response => response.request().method() === 'POST' && new URL(response.url()).pathname === '/api/v1/model-access/keys')
    await dialog.getByRole('button', { name: '创建接入密钥', exact: true }).click()
    const created = await (await keyCreated).json()
    dialog = user.getByRole('dialog', { name: '接入密钥已创建', exact: true })
    await dialog.getByRole('button', { name: '复制 API Key', exact: true }).click()
    assert.equal(await user.evaluate(() => navigator.clipboard.readText()), created.token)
    assert.match(await dialog.locator('[data-model-key-models]').textContent(), /platform-alpha/)
    assert.match(await dialog.locator('[data-model-key-models]').textContent(), /OpenAI Chat Completions/)
    for (const viewport of [{ width: 390, height: 844 }, { width: 320, height: 740 }, { width: 844, height: 390 }]) {
      await user.setViewportSize(viewport)
      await assertLayout(user, dialog)
      await user.screenshot({ path: path.join(artifacts, `model-key-${viewport.width}.png`), mask: [user.locator('[data-model-secret]')] })
    }
    await dialog.locator('[data-model-key-created]').getByRole('button', { name: '关闭', exact: true }).click()
    assert.equal(await user.locator('[data-model-secret]').count(), 0)
    context.diagnostic('One-time key copying and three mobile viewports passed')
    await user.setViewportSize({ width: 1280, height: 900 })
    const headers = { authorization: `Bearer ${created.token}`, 'content-type': 'application/json' }
    const available = await fetch(`${origin}/v1/models`, { headers })
    assert.equal(available.status, 200)
    assert.deepEqual((await available.json()).data.map(model => model.id).sort(), ['platform-alpha', 'platform-beta'])
    const completion = await fetch(`${origin}/v1/chat/completions`, { method: 'POST', headers, body: JSON.stringify({ model: 'platform-alpha', messages: [{ role: 'user', content: 'Public model request' }], max_tokens: 128, stream: false }) })
    assert.equal(completion.status, 200, await completion.clone().text())
    assert.match((await completion.json()).choices[0].message.content, /Model service fixture/)
    const responseModel = await fetch(`${origin}/v1/responses`, { method: 'POST', headers, body: JSON.stringify({ model: 'platform-beta', input: 'Responses model request', max_output_tokens: 128, stream: false }) })
    assert.equal(responseModel.status, 200, await responseModel.clone().text())
    assert.equal((await responseModel.json()).status, 'completed')
    const privileged = await fetch(`${origin}/api/v1/admin/models/providers`, { headers })
    assert.equal(privileged.status, 401, 'model-only credentials cannot use administration')
    const localOrigin = `http://127.0.0.1:${await freePort()}`
    local = startProcess(path.join(repository, 'target/debug/ternilo'), ['serve', '--listen', new URL(localOrigin).host, '--data-dir', path.join(directory, 'local')])
    await waitForHttp(localOrigin, local)
    const html = await (await fetch(localOrigin)).text()
    const boot = html.match(/window\.__TERNILO_BOOT__\s*=\s*(\{[^<]+\});/)
    assert.ok(boot)
    const localToken = JSON.parse(boot[1]).apiToken
    const workspacePath = path.join(directory, 'workspace')
    await mkdir(workspacePath)
    const workspace = await serverRequest(localOrigin, '/workspaces', { token: localToken, body: { path: workspacePath } })
    await serverRequest(localOrigin, '/credentials', { token: localToken, body: { name: 'MODEL_SERVICE_KEY', value: created.token } })
    await serverRequest(localOrigin, '/providers', { token: localToken, body: { ...provider.profile, id: 'platform', display_name: 'Platform models', base_url: `${origin}/v1`, api_key_ref: 'MODEL_SERVICE_KEY', models: [{ id: 'platform-alpha', settings: { mode: 'inherit' } }] } })
    for (const [protocol, expected] of [['openai-chat-completions', 'platform-alpha'], ['openai-responses', 'platform-beta']]) {
      const models = await serverRequest(localOrigin, '/providers/discover', { token: localToken, body: { base_url: `${origin}/v1`, protocol, api_key: created.token } })
      assert.deepEqual(models.map(model => model.id), [expected], 'Local discovery filters the public catalog by the selected protocol')
    }
    const session = await serverRequest(localOrigin, '/sessions', { token: localToken, body: { workspace_id: workspace.workspace_id } })
    await serverRequest(localOrigin, `/sessions/${session.identity.session_id}`, { token: localToken, method: 'PATCH', body: { model: { provider: 'named_provider', provider_id: 'platform', model: 'platform-alpha' } } })
    await serverRequest(localOrigin, `/sessions/${session.identity.session_id}/queue`, { token: localToken, body: { content: { kind: 'prompt', input: 'Run a local task with the shared model' } } })
    await until(() => serverRequest(localOrigin, `/sessions/${session.identity.session_id}/queue`, { token: localToken }), queue => !queue.active_run_id && !queue.items.some(item => item.placement === 'queued'), 'local model request finished')
    assert.ok(upstream.requests.length >= 2)
    assert.ok(upstream.requests.every(request => ['upstream-alpha', 'upstream-beta'].includes(request.model)))
    assert.equal(await readFile(path.join(workspacePath, 'model-service-proof.txt'), 'utf8'), 'Created by the local harness through the platform model service.\n')
    const localPage = await (await browser.newContext({ locale: 'zh-CN', viewport: { width: 1280, height: 900 }, serviceWorkers: 'block' })).newPage()
    localPage.on('pageerror', error => errors.push(`local page: ${error.message}`))
    localPage.on('console', message => { if (message.type() === 'error') errors.push(`local console: ${message.text()}`) })
    localPage.on('response', response => { if (response.status() >= 400) errors.push(`local HTTP ${response.status()}: ${new URL(response.url()).pathname}`) })
    await localPage.goto(localOrigin)
    await localPage.locator(`[data-sidebar-session-row][data-session-id="${session.identity.session_id}"]`).click()
    await localPage.getByText('Model service fixture completed the local task.', { exact: true }).last().waitFor()
    await localPage.screenshot({ path: path.join(artifacts, 'model-local-file-task.png') })

    context.diagnostic('Both public protocols and a real standalone Local file operation passed')
    await user.getByRole('link', { name: '用量', exact: true }).click()
    await user.locator('[data-model-request]').first().waitFor()
    const usage = await serverRequest(origin, '/model-access/usage', { token: oidc.accessToken() })
    assert.ok(usage.used_tokens >= 64 && usage.request_count >= 2)
    await user.screenshot({ path: path.join(artifacts, 'model-usage-desktop.png') })
    const groupStream = await fetch(`${origin}/v1/chat/completions`, { method: 'POST', headers, body: JSON.stringify({ model: 'platform-alpha', messages: [{ role: 'user', content: 'hold-model-stream group' }], max_tokens: 128, stream: true }) })
    assert.equal(groupStream.status, 200)
    const groupStreamBody = groupStream.text()
    await owner.locator(`[data-model-group="${group.group_id}"]`).getByRole('button', { name: '管理用户组', exact: true }).click()
    dialog = owner.getByRole('dialog', { name: '管理用户组', exact: true })
    await dialog.locator(`[data-model-group-member="${member.user.user_id}"]`).getByRole('button', { name: '移除', exact: true }).click()
    await dialog.locator(`[data-model-group-member="${member.user.user_id}"]`).waitFor({ state: 'detached' })
    await until(() => dialog.getByRole('button', { name: '添加账号', exact: true }).isEnabled(), Boolean, 'group member removal settled')
    await dialog.locator('[data-slot="dialog-close"]').click()
    await user.getByRole('link', { name: '模型', exact: true }).click()
    await user.getByRole('button', { name: '平台授权', exact: true }).click()
    await user.getByText('目前没有可用的模型授权。请联系平台管理员分配模型与额度。', { exact: true }).waitFor()
    assert.match(await groupStreamBody, /error/, 'group removal terminates an active model stream')
    const denied = await fetch(`${origin}/v1/chat/completions`, { method: 'POST', headers, body: JSON.stringify({ model: 'platform-alpha', messages: [{ role: 'user', content: 'Denied after group removal' }] }) })
    assert.equal(denied.status, 403)
    await user.getByRole('link', { name: '授权与接入', exact: true }).click()
    const keyRow = user.locator(`[data-model-key="${created.key.key_id}"]`)
    assert.match(await keyRow.textContent(), /未撤销/)
    assert.match(await keyRow.textContent(), /Subscriber shared budget/)
    await serverRequest(origin, `/admin/models/groups/${group.group_id}/members/${member.user.user_id}`, { token: ownerToken, method: 'PUT' })
    const keyStream = await fetch(`${origin}/v1/chat/completions`, { method: 'POST', headers, body: JSON.stringify({ model: 'platform-alpha', messages: [{ role: 'user', content: 'hold-model-stream key' }], max_tokens: 128, stream: true }) })
    assert.equal(keyStream.status, 200)
    const keyStreamBody = keyStream.text()
    await keyRow.getByRole('button', { name: '撤销', exact: true }).click()
    dialog = user.getByRole('dialog', { name: '撤销接入密钥？', exact: true })
    await dialog.getByRole('button', { name: '撤销', exact: true }).click()
    await dialog.waitFor({ state: 'hidden' })
    await until(() => keyRow.textContent(), text => text.includes('已撤销'), 'key revoked')
    assert.match(await keyStreamBody, /error/, 'key revocation terminates an active model stream')
    context.diagnostic('Group removal and key revocation terminated active streams')
    const finalUsage = await serverRequest(origin, '/model-access/usage', { token: oidc.accessToken() })
    assert.equal(finalUsage.active_requests, 0)
    assert.equal(finalUsage.unknown_requests, 2)
    assert.ok(finalUsage.reserved_tokens > 0, 'unknown upstream usage retains its conservative reservation')
    await user.getByRole('link', { name: '用量', exact: true }).click()
    await until(() => user.locator('[data-model-request]').filter({ hasText: '用量待核对' }).count(), count => count === 2, 'cancelled calls retain unknown usage in the UI')
    await user.screenshot({ path: path.join(artifacts, 'model-usage-unknown.png') })
    assert.equal(await user.getByRole('button', { name: '核对用量', exact: true }).count(), 0)
    const pendingRequests = (await serverRequest(origin, '/admin/models/requests', { token: ownerToken })).requests
    const unknownRequest = pendingRequests.find(request => request.attempted && request.accounted_tokens === null && request.state !== 'pending')
    const unknownAttempt = unknownRequest.attempts.find(attempt => attempt.attempted && attempt.accounted_tokens === null)
    const reconcilePath = `/admin/models/requests/${unknownRequest.request_id}/attempts/${unknownAttempt.attempt}/reconcile`
    const reconcileInput = { expected_settled_at_ms: unknownAttempt.settled_at_ms,
      usage: { input_tokens: 70, output_tokens: 30, cached_input_tokens: null, cache_write_tokens: null, reasoning_tokens: null },
      reference: 'upstream-report/browser-confirmed-call', note: 'Confirmed missing usage against the upstream report.' }
    await assert.rejects(() => serverRequest(origin, reconcilePath, { token: oidc.accessToken(), body: reconcileInput }), /403/)
    await owner.getByRole('tab', { name: '用量', exact: true }).click()
    const usageRow = owner.locator(`[data-model-request="${unknownRequest.request_id}"]`)
    await usageRow.locator('summary').click()
    await usageRow.getByRole('button', { name: '核对用量', exact: true }).click()
    dialog = owner.getByRole('dialog', { name: '管理员核对用量', exact: true })
    await dialog.getByLabel('输入', { exact: true }).fill('70')
    await dialog.getByLabel('输出', { exact: true }).fill('30')
    await dialog.getByLabel('核对依据', { exact: true }).fill(reconcileInput.reference)
    await dialog.getByLabel('核对说明', { exact: true }).fill(reconcileInput.note)
    for (const viewport of [{ width: 390, height: 844 }, { width: 320, height: 740 }]) {
      await owner.setViewportSize(viewport)
      await assertLayout(owner, dialog)
      await owner.screenshot({ path: path.join(artifacts, `model-reconcile-${viewport.width}.png`) })
    }
    const reconciled = owner.waitForResponse(response => response.request().method() === 'POST' && new URL(response.url()).pathname === `/api/v1${reconcilePath}`)
    await dialog.getByRole('button', { name: '确认用量并记录核对', exact: true }).click()
    const reconciledResponse = await reconciled
    assert.equal(reconciledResponse.status(), 200)
    const result = await reconciledResponse.json()
    assert.equal(result.attempt.accounted_tokens, 100)
    assert.equal(result.attempt.state, unknownAttempt.state)
    await dialog.locator('[data-usage-reconciliation]').waitFor()
    assert.ok((await dialog.textContent()).includes(reconcileInput.reference))
    await dialog.getByRole('button', { name: '关闭', exact: true }).last().click()
    const repeated = await serverRequest(origin, reconcilePath, { token: ownerToken, body: reconcileInput })
    assert.equal(repeated.reconciliation.reconciled_at_ms, result.reconciliation.reconciled_at_ms)
    await assert.rejects(() => serverRequest(origin, reconcilePath, { token: ownerToken, body: { ...reconcileInput, note: 'Conflicting correction' } }), /409/)
    const recordsPath = `/admin/models/requests/${unknownRequest.request_id}/reconciliations`
    assert.equal((await serverRequest(origin, recordsPath, { token: ownerToken })).length, 1)
    const reconciledUsage = await serverRequest(origin, '/model-access/usage', { token: oidc.accessToken() })
    assert.equal(reconciledUsage.used_tokens, finalUsage.used_tokens + 100)
    assert.equal(reconciledUsage.unknown_requests, 1)
    assert.equal(reconciledUsage.reserved_tokens, finalUsage.reserved_tokens - unknownAttempt.reserved_tokens)
    await owner.reload()
    await owner.getByRole('tab', { name: '用量', exact: true }).click()
    await owner.locator(`[data-model-request="${unknownRequest.request_id}"]`).getByRole('button', { name: '核对记录', exact: true }).click()
    dialog = owner.getByRole('dialog', { name: '核对记录', exact: true })
    await dialog.locator('[data-usage-reconciliation]').waitFor()
    assert.ok((await dialog.textContent()).includes(reconcileInput.reference))
    await dialog.getByRole('button', { name: '关闭', exact: true }).last().click()
    context.diagnostic('Administrative reconciliation is durable, idempotent, permission checked and reflected in the original account budget')


    const identity = await serverRequest(origin, '/auth/session', { token: ownerToken })
    assert.equal(identity.instance.managed_execution_enabled, false)
    const machines = await serverRequest(origin, `/tenants/${member.personal_tenant_id}/my-computers`, { token: oidc.accessToken() })
    assert.equal(machines.executors.length, 0)
    await user.evaluate(() => localStorage.setItem('ternilo.locale', 'en'))
    await user.reload()
    await user.getByRole('heading', { name: 'Usage', exact: true }).waitFor()
    await user.getByRole('link', { name: 'Access & connections', exact: true }).click()
    await user.locator(`[data-model-key="${created.key.key_id}"]`).waitFor()
    for (const viewport of [{ width: 390, height: 844 }, { width: 320, height: 740 }, { width: 844, height: 390 }]) {
      await user.setViewportSize(viewport)
      assert.ok(await user.evaluate(() => document.documentElement.scrollWidth <= innerWidth))
      await user.screenshot({ path: path.join(artifacts, `model-keys-en-${viewport.width}.png`) })
    }
    const account = (await serverRequest(origin, `/admin/accounts?query=${encodeURIComponent(members[1].user.user_id)}`, { token: ownerToken })).accounts[0]
    await serverRequest(origin, `/admin/accounts/${account.user_id}/role`, { token: ownerToken, method: 'PATCH', body: { role: 'auditor', role_revision: account.role_revision } })
    oidc.selectIdentity('01')
    const auditContext = await browser.newContext({ locale: 'zh-CN', viewport: { width: 1280, height: 900 }, serviceWorkers: 'block' })
    const auditor = await auditContext.newPage()
    auditor.on('pageerror', error => errors.push(`auditor: ${error.message}`))
    await auditor.goto(`${origin}/admin/models`)
    await auditor.getByRole('button', { name: '使用组织账号登录', exact: true }).click()
    await auditor.waitForURL(`${origin}/admin/models`)
    await auditor.locator('[data-published-model]').first().waitFor()
    assert.equal(await auditor.getByRole('button', { name: '发布模型', exact: true }).count(), 0)
    await auditor.getByRole('tab', { name: '上游接入', exact: true }).click()
    await auditor.locator('[data-model-provider]').first().waitFor()
    assert.equal(await auditor.getByRole('button', { name: '编辑', exact: true }).count(), 0)
    await auditor.getByRole('tab', { name: '用量', exact: true }).click()
    await auditor.locator('[data-model-request]').first().waitFor()
    assert.equal(await auditor.getByRole('button', { name: '核对用量', exact: true }).count(), 0)
    await assert.rejects(() => serverRequest(origin, reconcilePath, { token: oidc.accessToken('01'), body: reconcileInput }), /403/)

    assert.deepEqual(errors, [])
    await writeFile(path.join(artifacts, 'model-service-result.json'), JSON.stringify({ requests: upstream.requests.length, used_tokens: reconciledUsage.used_tokens, unknown_requests: reconciledUsage.unknown_requests, reserved_tokens: reconciledUsage.reserved_tokens, workers_enabled: false, enrolled_machines: 0, viewports: [390, 320, 844], errors }, null, 2))
  } catch (error) {
    if (browser) {
      for (const [contextIndex, context] of browser.contexts().entries()) {
        for (const [pageIndex, page] of context.pages().entries()) {
          await page.screenshot({ path: path.join(artifacts, `failure-${contextIndex}-${pageIndex}.png`), mask: [page.locator('[data-model-secret]')] }).catch(() => {})
        }
      }
    }
    throw error
  } finally {
    if (browser) await browser.close()
    if (local) await stopProcess(local)
    if (application) await stopProcess(application)
    await upstream.close()
    await oidc.close()
    if (artifacts !== directory) await rm(directory, { recursive: true, force: true })
  }
})
