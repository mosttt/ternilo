import assert from 'node:assert/strict'
import { verifyTeamUsage } from './team-usage-fixture.mjs'
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import path from 'node:path'
import { tmpdir } from 'node:os'
import test from 'node:test'
import { chromium } from 'playwright'
import { modelFixture, verifyToolDiscovery, verifyToolBudgetAndMap } from './coordination-fixture.mjs'
import { localApi, until } from './model-device-fixture.mjs'
import { execute, freePort, initializeServer, repository, serverRequest, startProcess, stopProcess, waitForHttp, selectSpace } from './platform-e2e-fixture.mjs'

async function configureRole(page, request, sessionId, model, workspaceTitle) {
  await request(`/sessions/${sessionId}`, { method: 'PATCH', body: { model } })
  await page.reload()
  if (workspaceTitle) {
    const workspace = page.locator('[data-sidebar-workspace-button]').filter({ hasText: workspaceTitle })
    await workspace.waitFor()
    if (await workspace.getAttribute('aria-expanded') === 'false') await workspace.click()
  }
  await page.locator(`[data-sidebar-session-row][data-session-id="${sessionId}"]`).click()
  await page.goto(`${new URL(page.url()).origin}/settings/plugins`)
  const card = page.locator('[data-plugin-id="subagents"]')
  await card.locator('button[aria-expanded][aria-label]').click()
  const investigator = card.getByRole('group', { name: '调查', exact: true })
  const picker = investigator.locator('[data-model-picker]')
  await picker.click()
  await page.getByRole('menuitem', { name: /^模型/ }).click()
  await page.getByRole('menuitem').filter({ hasText: 'role-model' }).first().click()
  await until(() => picker.getAttribute('title'), value => value?.includes('medium'), 'role model defaults preview')
  await picker.click()
  await page.getByRole('menuitem', { name: /^推理强度/ }).click()
  await page.getByRole('menuitem', { name: 'low', exact: true }).click()
  await card.locator('#plugin-subagents-coordination-investigator-instructions').fill('Platform evidence fixture')
  await card.getByRole('button', { name: '保存', exact: true }).click()
  await until(() => card.locator('button[aria-expanded][aria-label]').getAttribute('aria-expanded'), value => value === 'false', 'saved role settings')
  await page.goto(new URL(page.url()).origin)
  const state = await request('/state')
  const session = state.sessions.find(item => item.identity.session_id === sessionId)
  assert.equal(session.model.model, 'lead-model')
  const role = session.profile_plugins.find(entry => entry.id === 'subagents').config.coordination.investigator
  assert.equal(role.model.model, 'role-model'); assert.equal(role.model.reasoning_effort, 'low')
}

async function runRole(request, parentId, upstream) {
  const before = upstream.requests.length
  await request(`/sessions/${parentId}/queue`, { body: { content: { kind: 'prompt', input: 'Investigate using the configured role' } } })
  const pending = await until(() => request(`/questions?session_id=${parentId}`), value => value.length, 'agent approval')
  await request(`/questions/${pending[0].question.id}/answer?session_id=${parentId}`, { body: { selected: ['Allow once'], custom: null } })
  await until(() => request(`/sessions/${parentId}/queue`), value => !value.active_run_id, 'completed role run')
  const state = await until(() => request('/state'), value => value.sessions.some(item => item.parent_session_id === parentId && item.subagent?.role === 'investigator'), 'canonical role child')
  const child = state.sessions.find(item => item.parent_session_id === parentId && item.subagent?.role === 'investigator')
  assert.equal(child.permissions, 'read_only'); assert.equal(child.model.model, 'role-model')
  assert.ok(upstream.requests.slice(before).some(body => body.model === 'role-model' && body.reasoning_effort === 'low' && body.messages.some(message => String(message.content).includes('Platform evidence fixture'))))
  return child
}

test('Server role models execute through Node and Worker with independent reasoning settings', { timeout: 240_000 }, async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-coordination-platform-'))
  const processes = [], errors = []
  let browser, upstream, page
  try {
    upstream = await modelFixture()
    const policy = JSON.parse(await readFile(path.join(repository, 'examples/worker-policy.json'), 'utf8'))
    policy.minimum_workspace_free_bytes = 0
    policy.allowed_plugin_kinds = [...new Set([...policy.allowed_plugin_kinds, 'ternilo.model.host_gateway', 'ternilo.sandbox.cloud_outer', 'ternilo.files.local', 'ternilo.shell.local', 'ternilo.prompt.workspace_instructions', 'ternilo.tools.files', 'ternilo.tool.shell', 'ternilo.tool.ask_user', 'ternilo.tool.plan', 'ternilo.skills.registry', 'ternilo.skills.filesystem', 'ternilo.tools.skills', 'ternilo.subagents.in_process', 'ternilo.tools.agent_team'])]
    const policyPath = path.join(directory, 'policy.json'); await writeFile(policyPath, JSON.stringify(policy))
    const server = await initializeServer({ directory: path.join(directory, 'server'), origin: `http://127.0.0.1:${await freePort()}`, workerPolicy: policyPath, managedExecutionEnabled: true })
    processes.push(server)
    const token = server.owner.session.access_token
    let tenantId = server.owner.session.personal_tenant_id
    const request = (url, options = {}) => serverRequest(server.origin, url, { token, tenantId, ...options })
    const defaults = { context_window: 128_000, max_output_tokens: 4096, reasoning: { default_effort: 'medium', efforts: { low: 'low', medium: 'medium', high: 'high' } } }
    await request('/providers', { body: { id: 'coordination-fixture', display_name: 'Coordination Fixture', base_url: upstream.baseUrl, protocol: 'openai-chat-completions', api_key_ref: null, defaults, models: ['lead-model', 'role-model'].map(id => ({ id, display_name: id, settings: { mode: 'inherit' } })), timeout_ms: 30_000, max_attempts: 1, retry_base_delay_ms: 10 } })
    const model = { provider: 'account_provider', owner_user_id: server.owner.session.user.user_id, provider_id: 'coordination-fixture', model: 'lead-model' }
    const team = (await request('/tenants', { body: { slug: 'coordination', display_name: 'Coordination' } })).tenant
    tenantId = team.tenant_id
    const enrolled = await request(`/tenants/${tenantId}/my-computer-enrollments`, { body: { name: 'coordination-node', project_id: null, ttl_seconds: 600 } })
    const credential = await request('/enrollments/consume', { body: { token: enrolled.enrollment.token } })
    const nodeOrigin = `http://127.0.0.1:${await freePort()}`
    const node = startProcess(process.env.TERNILO_E2E_NODE_BINARY ?? path.join(repository, 'target/debug/ternilo'), ['serve', '--listen', new URL(nodeOrigin).host, '--data-dir', path.join(directory, 'node'), '--node-id', enrolled.enrollment.executor_id, '--gateway-url', `${server.origin.replace('http:', 'ws:')}/api/v1/executors/connect`, '--allow-insecure-gateway'], { TERNILO_LOCAL_TOKEN: credential.credential.token, XDG_STATE_HOME: path.join(directory, 'state') })
    processes.push(node); await waitForHttp(nodeOrigin, node)
    const local = await localApi(nodeOrigin)
    const folder = path.join(directory, 'workspace'); await mkdir(folder)
    const workspace = await local('/workspaces', { body: { path: folder } })
    await local('/sessions', { body: { workspace_id: workspace.workspace_id } })
    const mapped = await until(() => request('/state'), value => value.sessions.length === 1, 'mapped Node session')
    const nodeId = mapped.sessions[0].identity.session_id
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
    page = await browser.newPage({ locale: 'zh-CN', viewport: { width: 1366, height: 900 }, serviceWorkers: 'block' })
    page.on('pageerror', error => errors.push(error.message))
    page.on('console', message => { if (message.type() === 'error') errors.push(message.text()) })
    page.on('response', response => { if (response.status() >= 400) errors.push(`${response.status()} ${response.url()}`) })
    await page.goto(server.origin)
    await page.getByLabel('用户名', { exact: true }).fill(server.owner.username)
    await page.getByLabel('密码', { exact: true }).fill(server.owner.password)
    await page.getByRole('button', { name: '登录', exact: true }).click()
    await selectSpace(page, tenantId)
    await configureRole(page, request, nodeId, model)
    for (const id of [nodeId]) {
      const current = (await request('/state')).sessions.find(session => session.identity.session_id === id)
      await request(`/sessions/${id}`, { method: 'PATCH', body: { profile_plugins: [...current.profile_plugins, { id: 'agent-loop', kind: 'ternilo.agent.react', enabled: true, config: { observe_reasoning: true, prompt_cache: true } }] } })
    }
    const nodeChild = await runRole(request, nodeId, upstream)
    const project = (await request('/projects', { body: { name: 'Coordination' } })).project
    const managed = (await request('/workspaces', { body: { project_id: project.project_id, name: 'Managed coordination', placement: 'cloud' } })).workspace
    const cloud = await request('/sessions', { body: { workspace_id: managed.workspace_id, session_id: 'coordination-cloud', permissions: 'workspace_write' } })
    const registration = await request('/admin/workers', { body: { worker_id: 'coordination-worker' } })
    const workerBinary = process.env.TERNILO_CLOUD_E2E_WORKER_BINARY ?? path.join(repository, 'target/debug/ternilo-worker')
    const workerDirectory = path.join(directory, 'worker')
    await execute(workerBinary, ['init', '--config-dir', workerDirectory, '--server-url', server.origin, '--workspace-root', path.join(directory, 'managed-workspaces'), '--sandbox', 'bubblewrap', '--health-listen', '127.0.0.1:0', '--poll-interval-ms', '50'], { cwd: repository, env: { ...process.env, TERNILO_WORKER_TOKEN: registration.token } })
    const worker = startProcess(workerBinary, ['serve', '--config-dir', workerDirectory]); processes.push(worker)
    await until(() => worker.diagnostics(), output => /Ternilo cloud worker .* ready/.test(output), 'Worker ready')
    await configureRole(page, request, cloud.identity.session_id, model, 'Managed coordination')
    const configuredCloud = (await request('/state')).sessions.find(session => session.identity.session_id === cloud.identity.session_id)
    await request(`/sessions/${cloud.identity.session_id}`, { method: 'PATCH', body: { profile_plugins: [...configuredCloud.profile_plugins, { id: 'agent-loop', kind: 'ternilo.agent.react', enabled: true, config: { observe_reasoning: true, prompt_cache: true } }] } })
    const cloudChild = await runRole(request, cloud.identity.session_id, upstream)
    for (const sessionId of [nodeId, cloud.identity.session_id]) {
      const current = (await request('/state')).sessions.find(item => item.identity.session_id === sessionId)
      const plugins = current.profile_plugins.filter(entry => entry.id !== 'tool-registry')
      plugins.push({ id: 'tool-registry', kind: 'ternilo.tools.registry', enabled: true, config: { schema_loading: { enabled: true, max_loaded: 4 } } })
      await request(`/sessions/${sessionId}`, { method: 'PATCH', body: { profile_plugins: plugins } })
      await verifyToolDiscovery(request, sessionId, upstream)
      await page.reload()
      await page.locator(`[data-sidebar-session-row][data-session-id="${sessionId}"]`).click()
      await page.evaluate(() => localStorage.removeItem('ternilo.usage-estimate-prices.v1'))
      await verifyTeamUsage(page, request, sessionId, 'zh', sessionId === nodeId ? 'node' : 'worker')
      await verifyToolBudgetAndMap(request, sessionId, upstream)
      const recorded = await request(`/sessions/${sessionId}/events`)
      assert.ok(recorded.some(event => event.type === 'reasoning_effort_observed'))
    }
    assert.deepEqual(errors, [])
    console.log(JSON.stringify({ nodeChild: nodeChild.subagent.role, workerChild: cloudChild.subagent.role, roleReasoning: 'low', errors }))
  } catch (error) {
    for (const process of processes) console.error(process.diagnostics().slice(-7000))
    if (page && !page.isClosed()) console.error((await page.locator('body').innerText()).slice(-8000))
    throw error
  } finally {
    await browser?.close()
    for (const process of processes.reverse()) await stopProcess(process)
    await upstream?.close(); await rm(directory, { recursive: true, force: true })
  }
})
