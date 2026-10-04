import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { createServer } from 'node:http'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { chromium } from 'playwright'
import { selectSpace, execute, freePort, initializeServer, repository, serverRequest, startProcess, stopProcess } from './platform-e2e-fixture.mjs'

async function until(read, ready, label) {
  const deadline = Date.now() + 45_000
  let value
  while (Date.now() < deadline) {
    value = await read()
    if (ready(value)) return value
    await new Promise(resolve => setTimeout(resolve, 100))
  }
  throw new Error(`${label}: ${JSON.stringify(value)}`)
}

async function modelFixture() {
  const requests = []
  const server = createServer(async (request, response) => {
    const chunks = []
    for await (const chunk of request) chunks.push(chunk)
    const body = JSON.parse(Buffer.concat(chunks).toString())
    requests.push(body)
    const needsTool = body.tools?.some(tool => tool.function?.name === 'write_file') && !body.messages.some(message => message.role === 'tool')
    const tool = { index: 0, id: 'managed-files-write', type: 'function', function: { name: 'write_file', arguments: JSON.stringify({ path: 'managed-proof.txt', content: 'Written inside the managed Worker.\n' }) } }
    const identity = { id: `workspace-${requests.length}`, created: 1, model: body.model }
    const content = 'Managed workspace ready.'
    if (!body.stream) {
      response.writeHead(200, { 'content-type': 'application/json' })
      response.end(JSON.stringify({ ...identity, object: 'chat.completion', choices: [{ index: 0, message: { role: 'assistant', content }, finish_reason: 'stop' }] }))
      return
    }
    const frame = value => `data: ${JSON.stringify(value)}\n\n`
    response.writeHead(200, { 'content-type': 'text/event-stream' })
    response.end(frame({ ...identity, object: 'chat.completion.chunk', choices: [{ index: 0, delta: needsTool ? { role: 'assistant', tool_calls: [tool] } : { role: 'assistant', content }, finish_reason: null }] })
      + frame({ ...identity, object: 'chat.completion.chunk', choices: [{ index: 0, delta: {}, finish_reason: needsTool ? 'tool_calls' : 'stop' }] })
      + frame({ ...identity, object: 'chat.completion.chunk', choices: [], usage: { prompt_tokens: 20, completion_tokens: 10, total_tokens: 30 } }) + 'data: [DONE]\n\n')
  })
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve))
  return { requests, baseUrl: `http://127.0.0.1:${server.address().port}/v1`, close: () => new Promise(resolve => { server.closeAllConnections(); server.close(resolve) }) }
}

test('managed workspace browsing reads real Worker output without a model and keeps session-only viewers outside the directory', { timeout: 180_000 }, async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-cloud-workspace-'))
  const artifacts = process.env.TERNILO_E2E_ARTIFACT_DIR
  const model = await modelFixture()
  const processes = [], errors = []
  let browser, page
  try {
    const policy = JSON.parse(await readFile(path.join(repository, 'examples/worker-policy.json'), 'utf8'))
    policy.minimum_workspace_free_bytes = 0
    policy.allowed_plugin_kinds = [...new Set([...policy.allowed_plugin_kinds, 'ternilo.model.host_gateway', 'ternilo.sandbox.cloud_outer', 'ternilo.files.local', 'ternilo.shell.local', 'ternilo.prompt.workspace_instructions', 'ternilo.tools.files', 'ternilo.tool.shell', 'ternilo.tool.ask_user', 'ternilo.tool.plan', 'ternilo.skills.registry', 'ternilo.skills.filesystem', 'ternilo.tools.skills', 'ternilo.subagents.in_process', 'ternilo.tools.agent_team'])]
    const policyPath = path.join(directory, 'policy.json')
    await writeFile(policyPath, JSON.stringify(policy))
    const application = await initializeServer({ directory: path.join(directory, 'server'), origin: `http://127.0.0.1:${await freePort()}`, workerPolicy: policyPath, managedExecutionEnabled: true })
    processes.push(application)
    const owner = (resource, options = {}) => serverRequest(application.origin, resource, { token: application.owner.session.access_token, ...options })
    const team = (await owner('/tenants', { body: { slug: 'managed-files', display_name: 'Managed files' } })).tenant
    const access = (resource, options = {}) => owner(resource, { tenantId: team.tenant_id, ...options })
    const project = (await access('/projects')).projects[0]
    const workspace = (await access('/workspaces', { body: { project_id: project.project_id, name: 'Managed files', placement: 'cloud' } })).workspace
    const session = await access('/sessions', { body: { workspace_id: workspace.workspace_id, session_id: 'managed-files-run', permissions: 'workspace_write' } })
    const sessionPath = `/sessions/${session.identity.session_id}`
    const registration = await owner('/admin/workers', { body: { worker_id: 'workspace-browser-worker' } })
    const binary = process.env.TERNILO_CLOUD_E2E_WORKER_BINARY ?? path.join(repository, 'target/debug/ternilo-worker')
    const config = path.join(directory, 'config.json'), workspaceRoot = path.join(directory, 'workspaces')
    await execute(binary, ['init', '--config-dir', path.dirname(config), '--server-url', application.origin, '--workspace-root', workspaceRoot, '--sandbox', 'bubblewrap', '--health-listen', '127.0.0.1:0', '--poll-interval-ms', '50'], { cwd: repository, env: { ...process.env, TERNILO_WORKER_TOKEN: registration.token } })
    const worker = startProcess(binary, ['serve', '--config-dir', path.dirname(config)])
    processes.push(worker)
    await until(() => worker.diagnostics(), text => /Ternilo cloud worker .* ready/.test(text), 'Worker ready')
    const cold = await access(`${sessionPath}/workspace`)
    assert.equal(cold.can_browse, true)
    assert.deepEqual(cold.applications, [])
    await assert.rejects(access(`${sessionPath}/workspace`, { body: { kind: 'list', path: '' } }), /has not been initialized/)
    assert.equal(model.requests.length, 0)
    const profile = { id: 'workspace-fixture', display_name: 'Workspace fixture', base_url: model.baseUrl, protocol: 'openai-chat-completions', api_key_ref: null, defaults: { context_window: 100_000, max_output_tokens: 2048 }, models: [{ id: 'files-model', settings: { mode: 'inherit' } }], timeout_ms: 30000, max_attempts: 1, retry_base_delay_ms: 50 }
    await owner('/admin/models/providers', { body: { profile, enabled: true, api_key: 'workspace-fixture-key' } })
    await owner('/admin/models/publications', { body: { model_id: 'files-model', display_name: 'Files model', provider_id: profile.id, upstream_model: 'files-model', enabled: true } })
    const grant = await owner('/admin/models/grants', { body: { name: 'Files test budget', subject: { kind: 'user', id: application.owner.session.user.user_id }, model_ids: ['files-model'], monthly_tokens: 1_000_000, max_concurrent_requests: 4, allow_resource_sharing: true } })
    await access(sessionPath, { method: 'PATCH', body: { title: 'Managed file proof', model: { provider: 'platform_model', grant_id: grant.grant_id, model_id: 'files-model' } } })
    const submission = await access(`${sessionPath}/queue`, { body: { content: { kind: 'prompt', input: 'Write the managed workspace proof.' } } })
    await until(() => access(`/tenants/${team.tenant_id}/runs/${submission.run_id}`), record => record.run.state === 'succeeded' || record.run.state === 'failed', 'managed write completion').then(record => assert.equal(record.run.state, 'succeeded', JSON.stringify(record)))
    const diskPath = path.join(workspaceRoot, createHash('sha256').update(team.tenant_id).digest('hex'), createHash('sha256').update(workspace.workspace_id).digest('hex'), 'managed-proof.txt')
    assert.equal(await readFile(diskPath, 'utf8'), 'Written inside the managed Worker.\n')
    const reader = await access('/sessions', { body: { workspace_id: workspace.workspace_id, session_id: 'managed-files-reader' } })
    assert.equal(reader.model.provider, 'profile_default')
    const readerPath = `/sessions/${reader.identity.session_id}`
    const beforeRead = model.requests.length
    const listing = await access(`${readerPath}/workspace`, { body: { kind: 'list', path: '' } })
    assert.ok(listing.entries.some(entry => entry.name === 'managed-proof.txt' && entry.kind === 'file'))
    assert.equal((await access(`${readerPath}/workspace`, { body: { kind: 'read', path: 'managed-proof.txt' } })).content, 'Written inside the managed Worker.\n')
    await assert.rejects(access(`${readerPath}/workspace`, { body: { kind: 'read', path: '../config.json' } }), /400/)
    await assert.rejects(access(`${readerPath}/workspace`, { body: { kind: 'open', app_id: 'vscode' } }), /403/)
    const invitation = await owner('/admin/invitations', { body: { tenant_id: null, role: 'member', expires_in_seconds: 3600 } })
    const account = { username: 'managed-file-viewer', password: 'managed-file-viewer-password' }
    const member = await serverRequest(application.origin, '/auth/invitations/accept', { body: { token: invitation.token, email: 'managed-file-viewer@example.test', ...account } })
    await owner(`/tenants/${team.tenant_id}/members/${member.user.user_id}`, { method: 'PUT', body: { role: 'member' } })
    const peer = (resource, options = {}) => serverRequest(application.origin, resource, { token: member.access_token, tenantId: team.tenant_id, ...options })
    const viewOnly = { view: true, submit: false, stop: false, configure: false }
    await access(`${readerPath}/sharing/user/${member.user.user_id}`, { method: 'PUT', body: viewOnly })
    assert.equal((await peer(`${readerPath}/workspace`)).can_browse, false)
    await assert.rejects(peer(`${readerPath}/workspace`, { body: { kind: 'read', path: 'managed-proof.txt' } }), /403/)
    await access(`/workspaces/${workspace.workspace_id}/sharing/user/${member.user.user_id}`, { method: 'PUT', body: viewOnly })
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
    page = await browser.newPage({ locale: 'zh-CN', viewport: { width: 1440, height: 900 } })
    page.on('pageerror', error => errors.push(error.message))
    page.on('console', message => { if (message.type() === 'error') errors.push(message.text()) })
    page.on('response', response => { if (response.status() >= 400) errors.push(`${response.status()} ${new URL(response.url()).pathname}`) })
    for (const asset of ['app.js', 'app.css']) {
      const actual = Buffer.from(await (await fetch(`${application.origin}/assets/${asset}`)).arrayBuffer())
      assert.equal(createHash('sha256').update(actual).digest('hex'), createHash('sha256').update(await readFile(path.join(repository, 'web/dist/assets', asset))).digest('hex'))
    }
    await page.goto(application.origin)
    await page.getByLabel('用户名', { exact: true }).fill(account.username)
    await page.getByLabel('密码', { exact: true }).fill(account.password)
    await page.getByRole('button', { name: '登录', exact: true }).click()
    await selectSpace(page, team.tenant_id)
    await page.locator(`[data-sidebar-session-row][data-session-id="${reader.identity.session_id}"] [data-sidebar-session-button]`).click()
    const toggle = page.locator('[data-workspace-toggle]')
    await until(() => toggle.isEnabled(), Boolean, 'shared workspace available')
    assert.equal(await page.locator('[data-workspace-open-app]').count(), 0)
    await toggle.click()
    const panel = page.locator('[data-workspace-panel]')
    await panel.locator('[data-workspace-entry="managed-proof.txt"]').click()
    await panel.locator('[data-workspace-preview="managed-proof.txt"] pre').filter({ hasText: 'Written inside the managed Worker.' }).waitFor()
    await page.reload()
    await panel.locator('[data-workspace-preview="managed-proof.txt"] pre').waitFor()
    await page.setViewportSize({ width: 390, height: 844 })
    await until(() => panel.getAttribute('role'), role => role === 'dialog', 'mobile file panel')
    assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth))
    if (artifacts) { await mkdir(artifacts, { recursive: true }); await page.screenshot({ path: path.join(artifacts, 'cloud-workspace-mobile.png'), animations: 'disabled' }) }
    await access(`/workspaces/${workspace.workspace_id}/sharing/user/${member.user.user_id}`, { method: 'DELETE' })
    await panel.getByText('需要工作区查看权限；仅共享会话不会开放目录。', { exact: true }).waitFor()
    await assert.rejects(peer(`${readerPath}/workspace`, { body: { kind: 'read', path: 'managed-proof.txt' } }), /403/)
    assert.equal(model.requests.length, beforeRead, 'file browsing must not invoke a model')
    assert.deepEqual(errors, [])
  } catch (error) {
    if (page && artifacts) { await mkdir(artifacts, { recursive: true }); await page.screenshot({ path: path.join(artifacts, 'cloud-workspace-failure.png') }).catch(() => {}) }
    throw new Error(`${error.stack}\n${processes.map(process => process.diagnostics()).join('\n')}`)
  } finally {
    await browser?.close()
    for (const process of processes.reverse()) await stopProcess(process)
    await model.close()
    await rm(directory, { recursive: true, force: true })
  }
})
