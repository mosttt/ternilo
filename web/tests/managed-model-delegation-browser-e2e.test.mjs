import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { chromium } from 'playwright'
import { execute, freePort, initializeServer, repository, selectSpace, serverRequest, startProcess, stopProcess } from './platform-e2e-fixture.mjs'
import { until } from './model-device-fixture.mjs'
import { profile, upstream } from './account-node-provider-fixture.mjs'

test('managed collaboration keeps model ownership separate from resources, submitters and service accounts', { timeout: 240000 }, async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-managed-delegation-'))
  const artifacts = process.env.TERNILO_E2E_ARTIFACT_DIR ? path.join(process.env.TERNILO_E2E_ARTIFACT_DIR, 'managed-model-delegation') : undefined
  if (artifacts) await mkdir(artifacts, { recursive: true })
  const processes = [], models = [], errors = []
  let browser, page, release
  const networkFailures = [], modelWrites = []
  const result = { networkFailures, modelWrites, status: 'running', checks: [], errors }
  try {
    const policy = JSON.parse(await readFile(path.join(repository, 'examples/worker-policy.json')))
    policy.minimum_workspace_free_bytes = 0
    policy.allowed_plugin_kinds = [...new Set([...policy.allowed_plugin_kinds, 'ternilo.model.host_gateway', 'ternilo.sandbox.cloud_outer', 'ternilo.files.local', 'ternilo.shell.local', 'ternilo.prompt.workspace_instructions', 'ternilo.tools.files', 'ternilo.tool.shell', 'ternilo.tool.ask_user', 'ternilo.tool.plan', 'ternilo.skills.registry', 'ternilo.skills.filesystem', 'ternilo.tools.skills', 'ternilo.subagents.in_process', 'ternilo.tools.agent_team'])]
    const policyPath = path.join(directory, 'policy.json'); await writeFile(policyPath, JSON.stringify(policy))
    const server = await initializeServer({ directory: path.join(directory, 'server'), origin: `http://127.0.0.1:${await freePort()}`,
      databaseUrl: process.env.TERNILO_E2E_DATABASE_URL, migrationDatabaseUrl: process.env.TERNILO_E2E_MIGRATION_DATABASE_URL,
      workerPolicy: policyPath, managedExecutionEnabled: true })
    processes.push(server)
    const identity = server.owner.session
    const owner = (resource, options = {}) => serverRequest(server.origin, resource, { token: identity.access_token, tenantId: identity.personal_tenant_id, ...options })
    const registration = await owner('/admin/registration')
    await owner('/admin/registration', { method: 'PATCH', body: { mode: 'open', require_approval: false, revision: registration.revision } })
    const credentials = { username: 'model-provider', email: 'model-provider@example.test', password: 'model-provider-fixture-password' }
    const { session: source } = await serverRequest(server.origin, '/auth/register', { body: credentials })
    const { tenant } = await owner('/tenants', { body: { slug: 'delegated-models', display_name: 'Delegated models' } })
    const tenantId = tenant.tenant_id
    const request = (resource, options = {}) => owner(resource, { tenantId, ...options })
    const contributor = (resource, options = {}) => serverRequest(server.origin, resource, { token: source.access_token, tenantId, ...options })
    const personal = (resource, options = {}) => contributor(resource, { tenantId: source.personal_tenant_id, ...options })
    await request(`/tenants/${tenantId}/members/${source.user.user_id}`, { method: 'PUT', body: { role: 'member' } })
    const sourceModel = await upstream('delegated', 'source-account-secret'); models.push(sourceModel)
    const ownerModel = await upstream('resource-owner', 'resource-owner-secret'); models.push(ownerModel)
    for (const [api, model, key] of [[personal, sourceModel, 'source-account-secret'], [owner, ownerModel, 'resource-owner-secret']]) {
      await api('/credentials', { body: { name: 'SAME_PROVIDER_KEY', value: key } })
      await api('/providers', { body: profile(model.baseUrl) })
    }
    await personal('/providers', { body: { ...profile(sourceModel.baseUrl), id: 'unrelated', display_name: 'Unrelated private provider' } })
    const { project } = await request('/projects', { body: { name: 'Delegation tasks' } })
    const { workspace } = await request('/workspaces', { body: { project_id: project.project_id, name: 'Shared managed workspace', placement: 'cloud' } })
    const session = await request('/sessions', { body: { workspace_id: workspace.workspace_id, permissions: 'workspace_write' } })
    const id = session.identity.session_id
    const share = `/workspaces/${workspace.workspace_id}/sharing/user/${source.user.user_id}`
    const permissions = { view: true, submit: true, stop: true, configure: true }
    await request(share, { method: 'PUT', body: permissions })
    const workerGrant = await owner('/admin/workers', { body: { worker_id: 'delegation-worker' } })
    const binary = process.env.TERNILO_CLOUD_E2E_WORKER_BINARY ?? path.join(repository, 'target/debug/ternilo-worker')
    const workerDirectory = path.join(directory, 'worker'), workspaceRoot = path.join(directory, 'workspaces')
    await execute(binary, ['init', '--config-dir', workerDirectory, '--server-url', server.origin, '--workspace-root', workspaceRoot, '--sandbox', 'bubblewrap', '--health-listen', '127.0.0.1:0', '--poll-interval-ms', '50'], { cwd: repository, env: { ...process.env, TERNILO_WORKER_TOKEN: workerGrant.token } })
    const worker = startProcess(binary, ['serve', '--config-dir', workerDirectory]); processes.push(worker)
    await until(() => worker.diagnostics(), output => /Ternilo cloud worker .* ready/.test(output), 'Worker ready')
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
    page = await browser.newPage({ viewport: { width: 1440, height: 980 }, serviceWorkers: 'block' })
    page.on('request', request => { if (['PATCH', 'PUT'].includes(request.method())) modelWrites.push({ path: new URL(request.url()).pathname, body: request.postDataJSON() }) })
    page.on('pageerror', error => errors.push(error.message))
    page.on('console', message => { if (message.type() === 'error') errors.push(message.text()) })
    page.on('response', response => { if (response.status() >= 400) { errors.push(`HTTP ${response.status()} ${new URL(response.url()).pathname}`); void response.text().then(body => networkFailures.push({ status: response.status(), path: new URL(response.url()).pathname, body })) } })
    await page.goto(server.origin)
    await page.getByLabel('用户名', { exact: true }).fill(credentials.username)
    await page.getByLabel('密码', { exact: true }).fill(credentials.password)
    await page.getByRole('button', { name: '登录', exact: true }).click()
    await selectSpace(page, tenantId)
    await page.locator(`[data-sidebar-session-row][data-session-id="${id}"]`).click()
    await page.locator('[data-input-bar] [data-model-picker]').click()
    const modelMenu = page.getByRole('menuitem', { name: /^模型/ })
    await modelMenu.hover(); await modelMenu.press('ArrowRight')
    const accountChoice = page.locator('[data-model-source="account"] [data-model-provider="same"]').getByRole('menuitem').filter({ hasText: 'same-model' })
    await accountChoice.waitFor()
    result.picker = { text: await accountChoice.innerText(), disabled: await accountChoice.getAttribute('data-disabled'), credentials: await personal('/credentials') }
    assert.equal(result.picker.disabled, null)
    const [chosen] = await Promise.all([page.waitForResponse(response => response.request().method() === 'PATCH' && new URL(response.url()).pathname.endsWith(`/sessions/${id}`)), accountChoice.press('Enter')])
    assert.equal(chosen.status(), 200)
    const selection = (await request(`/model-options?session_id=${id}`)).current.selection
    assert.deepEqual(selection, { provider: 'account_provider', owner_user_id: source.user.user_id, provider_id: 'same', model: 'same-model' })
    await assert.rejects(() => request(`/sessions/${id}`, { method: 'PATCH', body: { model: { ...selection, provider_id: 'unrelated' } } }), /403/)
    const catalog = await request(`/model-options?session_id=${id}`)
    assert.equal(catalog.current.owner_user_id, source.user.user_id)
    assert.ok(catalog.providers.every(profile => profile.id === 'same' && profile.base_url === ''))
    assert.ok(!JSON.stringify(catalog).includes('source-account-secret'))
    const folder = path.join(workspaceRoot, createHash('sha256').update(tenantId).digest('hex'), createHash('sha256').update(workspace.workspace_id).digest('hex'))
    const runTask = async (api, input, actor) => {
      const accepted = await api(`/sessions/${id}/queue`, { body: { content: { kind: 'prompt', input }, attachments: [] } })
      const events = await until(() => request(`/sessions/${id}/events`), events => events.some(event => event.run_id === accepted.run_id && ['turn_finished', 'turn_failed'].includes(event.type)), 'managed task finishes')
      assert.equal(events.some(event => event.run_id === accepted.run_id && event.type === 'turn_failed'), false, JSON.stringify(events.filter(event => event.run_id === accepted.run_id)))
      await until(() => request(`/tenants/${tenantId}/runs/${accepted.run_id}`), value => value.run.state === 'succeeded', 'managed run settles')
      assert.equal(await readFile(path.join(folder, 'provider-source.txt'), 'utf8'), 'delegated')
      const ledger = (await owner('/admin/models/requests?limit=100')).requests.filter(record => record.workload?.run_id === accepted.run_id)
      assert.ok(ledger.length >= 2)
      for (const record of ledger) {
        assert.equal(record.actor_user_id, actor)
        assert.equal(record.model_beneficiary_user_id, source.user.user_id)
        assert.equal(record.resource_owner_user_id, identity.user.user_id)
        assert.equal(record.workload.execution_owner_user_id, identity.user.user_id)
      }
      return { ...accepted, ledger }
    }
    await runTask(request, 'Use the delegated private model and write a proof.', identity.user.user_id)
    assert.equal(ownerModel.calls.length, 0, 'same-name resource-owner provider is not selected')
    result.checks.push('browser selection by a collaborator, source identity, hidden unrelated providers, Worker tool loop and separate resource/model ownership')

    const root = `/tenants/${tenantId}/service-accounts`
    const { service_account: account } = await request(root, { body: { name: 'Managed automation' } })
    const grant = await request(`${root}/${account.service_account_id}/credentials`, { body: { name: 'Run', scopes: ['resource.read', 'run.execute'], expires_at_ms: Date.now() + 600000 } })
    const access = { view: true, submit: true, stop: true, configure: false }
    await request(`${root}/${account.service_account_id}/workspaces/${workspace.workspace_id}`, { method: 'PUT', body: { permissions: access, expected_permissions: null } })
    const service = (resource, options = {}) => serverRequest(server.origin, resource, { token: grant.access_token, tenantId, ...options })
    await runTask(service, 'Use the delegated model from automation.', account.service_account_id)
    result.checks.push('service account task with distinct submitter, model provider and resource owner')

    await owner('/admin/models/providers', { body: { profile: { ...profile(sourceModel.baseUrl), id: 'platform-delegation', api_key_ref: null }, enabled: true, api_key: 'source-account-secret' } })
    await owner('/admin/models/publications', { body: { model_id: 'delegated-public', display_name: 'Delegated platform model', provider_id: 'platform-delegation', upstream_model: 'same-model', enabled: true } })
    const grantBody = { name: 'Contributor budget', subject: { kind: 'user', id: source.user.user_id }, model_ids: ['delegated-public'], monthly_tokens: 1000000, max_concurrent_requests: 4, allow_resource_sharing: false }
    const allowance = await owner('/admin/models/grants', { body: grantBody })
    await page.locator('[data-input-bar] [data-model-picker]').click()
    await modelMenu.hover(); await modelMenu.press('ArrowRight')
    const platformChoice = page.locator(`[data-model-grant="${allowance.grant_id}"]`).getByRole('menuitem')
    await platformChoice.waitFor()
    const [platformChosen] = await Promise.all([page.waitForResponse(response => response.request().method() === 'PATCH' && new URL(response.url()).pathname.endsWith(`/sessions/${id}`)), platformChoice.press('Enter')])
    assert.equal(platformChosen.status(), 200)
    const beforePlatform = sourceModel.calls.length
    await assert.rejects(() => service(`/sessions/${id}/queue`, { body: { content: { kind: 'prompt', input: 'Cannot share a private model grant.' }, attachments: [] } }), /403/)
    assert.equal(sourceModel.calls.length, beforePlatform)
    await owner(`/admin/models/grants/${allowance.grant_id}`, { method: 'PUT', body: { ...grantBody, allow_resource_sharing: true } })
    const platformTask = await runTask(service, 'Use the explicitly shared contributor budget.', account.service_account_id)
    assert.ok(platformTask.ledger.every(record => record.grant_id === allowance.grant_id && record.source === 'platform_grant'))
    await contributor(`/sessions/${id}`, { method: 'PATCH', body: { model: selection } })
    result.checks.push('contributor platform budget selection, explicit sharing requirement and independent service-account attribution')

    const hold = sourceModel.holdNext(body => body.stream && body.tools?.length && JSON.stringify(body.messages.findLast(message => message.role === 'user')).includes('revoke-model-owner'))
    release = hold.release
    const active = await service(`/sessions/${id}/queue`, { body: { content: { kind: 'prompt', input: 'revoke-model-owner' }, attachments: [] } })
    await Promise.race([hold.began, new Promise((_, reject) => setTimeout(() => reject(new Error('delegated call did not start')), 15000))])
    await request(share, { method: 'PUT', body: { ...permissions, configure: false } })
    await until(() => request(`/sessions/${id}/events`), events => events.some(event => event.run_id === active.run_id && event.type === 'turn_failed'), 'source permission revocation stops the model')
    release(); release = undefined
    const count = sourceModel.calls.length
    await assert.rejects(() => service(`/sessions/${id}/queue`, { body: { content: { kind: 'prompt', input: 'No model authorization now.' }, attachments: [] } }))
    assert.equal(sourceModel.calls.length, count)
    assert.equal((await request(`/model-options?session_id=${id}`)).current.available, false)
    assert.equal(ownerModel.calls.length, 0)
    result.checks.push('model owner configuration revocation stops active work and new calls without changing service resource access or falling back')
    await page.reload()
    await page.locator('[data-input-bar] [data-model-picker]').waitFor()
    for (const width of [1440, 390, 320]) {
      await page.setViewportSize({ width, height: 980 })
      assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth))
      if (artifacts) await page.screenshot({ path: path.join(artifacts, `managed-delegation-${width}.png`) })
    }
    assert.deepEqual(errors, [])
    result.status = 'passed'
  } catch (error) {
    result.status = 'failed'; result.error = String(error?.stack ?? error); result.diagnostics = processes.map(process => process.diagnostics())
    if (artifacts && page) await page.screenshot({ path: path.join(artifacts, 'failure.png') }).catch(() => {})
    throw error
  } finally {
    release?.(); await browser?.close()
    for (const process of processes.reverse()) await stopProcess(process)
    for (const model of models) await model.close()
    if (artifacts) await writeFile(path.join(artifacts, 'result.json'), JSON.stringify(result, null, 2))
    await rm(directory, { recursive: true, force: true })
  }
})
