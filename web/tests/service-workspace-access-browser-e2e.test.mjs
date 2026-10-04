import assert from 'node:assert/strict'
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { chromium } from 'playwright'
import { upstream, profile } from './account-node-provider-fixture.mjs'
import { execute, freePort, initializeServer, repository, serverRequest, startProcess, stopProcess, waitForHttp } from './platform-e2e-fixture.mjs'

async function until(read, predicate, message, timeout = 30000) {
  const deadline = Date.now() + timeout
  while (Date.now() < deadline) {
    const value = await read()
    if (predicate(value)) return value
    await new Promise(resolve => setTimeout(resolve, 40))
  }
  throw new Error(message)
}
async function live(origin, token, tenant) {
  const socket = new WebSocket(`${origin.replace('http:', 'ws:')}/api/v1/live`)
  const frames = []
  let closed = false
  socket.addEventListener('message', event => frames.push(JSON.parse(event.data)))
  socket.addEventListener('close', () => { closed = true })
  await new Promise((resolve, reject) => { socket.addEventListener('open', resolve, { once: true }); socket.addEventListener('error', reject, { once: true }) })
  socket.send(JSON.stringify({ type: 'hello', protocol_version: 1, bearer_token: token, tenant_id: tenant }))
  await until(() => frames, value => value.some(frame => ['ready', 'error'].includes(frame.type)), 'Live handshake completes')
  return { socket, frames, get closed() { return closed } }
}
async function status(origin, resource, scope, body) {
  const response = await fetch(`${origin}/api/v1${resource}`, { method: body ? 'POST' : 'GET', headers: { authorization: `Bearer ${scope.token}`, 'x-ternilo-tenant': scope.tenantId, ...(body ? { 'content-type': 'application/json' } : {}) }, ...(body ? { body: JSON.stringify(body) } : {}) })
  return response.status
}

test('service workspace grants, scoped Live, real SDK execution and revocation preserve the original computer and authors', { timeout: 180000 }, async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-service-workspaces-'))
  const artifacts = process.env.TERNILO_E2E_ARTIFACT_DIR
  if (artifacts) await mkdir(artifacts, { recursive: true })
  let server, node, browser, page, model
  const sockets = []
  const report = { status: 'running', errors: [], checks: [] }
  try {
    const origin = `http://127.0.0.1:${await freePort()}`
    server = await initializeServer({ directory: path.join(directory, 'server'), origin, mode: 'single_user' })
    const identity = server.owner.session
    const owner = { token: identity.access_token, tenantId: identity.personal_tenant_id }
    const enrollment = (await serverRequest(origin, `/tenants/${owner.tenantId}/my-computer-enrollments`, { ...owner, body: { name: '自动化电脑', ttl_seconds: 600 } })).enrollment
    const credential = (await serverRequest(origin, '/enrollments/consume', { body: { token: enrollment.token } })).credential
    const localOrigin = `http://127.0.0.1:${await freePort()}`
    const folder = path.join(directory, 'Project')
    await mkdir(folder)
    node = startProcess(process.env.TERNILO_E2E_NODE_BINARY ?? path.join(repository, 'target/debug/ternilo'), ['serve', '--listen', new URL(localOrigin).host, '--data-dir', path.join(directory, 'node'), '--node-id', enrollment.executor_id, '--gateway-url', `${origin.replace('http:', 'ws:')}/api/v1/executors/connect`, '--token', credential.token, '--allow-insecure-gateway'])
    await waitForHttp(localOrigin, node)
    await until(() => serverRequest(origin, `/tenants/${owner.tenantId}/my-computers/${enrollment.executor_id}`, owner), value => value.connected, 'Node connects')
    const workspace = (await serverRequest(origin, '/workspaces', { ...owner, body: { project_id: identity.personal_project_id, name: '自动化工作区', placement: 'local_node', executor_id: enrollment.executor_id, path: folder } })).workspace
    const session = await serverRequest(origin, '/sessions', { ...owner, body: { workspace_id: workspace.workspace_id } })
    const sessionId = session.identity.session_id
    const root = `/tenants/${owner.tenantId}/service-accounts`
    const account = (await serverRequest(origin, root, { ...owner, body: { name: '构建任务' } })).service_account
    const credentials = `${root}/${account.service_account_id}/credentials`
    const issue = scopes => serverRequest(origin, credentials, { ...owner, body: { name: scopes.join('+'), scopes, expires_at_ms: Date.now() + 600000 } })
    const readGrant = await issue(['resource.read'])
    const runGrant = await issue(['resource.read', 'run.execute'])
    const onlyRun = await issue(['run.execute'])
    const readScope = { token: readGrant.access_token, tenantId: owner.tenantId }
    const runScope = { token: runGrant.access_token, tenantId: owner.tenantId }
    assert.ok([400, 403].includes(await status(origin, `/sessions/${sessionId}/history`, runScope)))
    for (const [token, tenant] of [[onlyRun.access_token, owner.tenantId], [readGrant.access_token, 'another-tenant']]) {
      const rejected = await live(origin, token, tenant); sockets.push(rejected.socket)
      assert.equal(rejected.frames[0].type, 'error')
      assert.equal(rejected.frames[0].code, 'policy_denied')
    }
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
    page = await browser.newPage({ locale: 'zh-CN', viewport: { width: 1440, height: 980 } })
    const requests = []
    page.on('pageerror', error => report.errors.push(error.message))
    page.on('console', message => { if (message.type() === 'error') report.errors.push(message.text()) })
    page.on('response', response => { if (response.status() >= 400) report.errors.push(`HTTP ${response.status()} ${new URL(response.url()).pathname}`) })
    page.on('request', request => requests.push(new URL(request.url()).pathname))
    await page.goto(`${origin}/spaces/current`)
    await page.getByLabel('用户名', { exact: true }).fill(server.owner.username)
    await page.getByLabel('密码', { exact: true }).fill(server.owner.password)
    await page.getByRole('button', { name: '登录', exact: true }).click()
    await page.getByRole('tab', { name: '服务账号', exact: true }).click()
    await page.locator(`[data-service-account="${account.service_account_id}"]`).getByRole('button', { name: '详情与凭据', exact: true }).click()
    const resourcePath = `/api/v1${root}/${account.service_account_id}/workspaces`
    assert.equal(requests.filter(value => value === resourcePath).length, 0)
    await page.locator('[data-service-workspaces] summary').click()
    const row = page.locator(`[data-service-workspace="${workspace.workspace_id}"]`)
    await row.getByText('自动化电脑', { exact: true }).waitFor()
    const choose = async label => {
      await row.getByRole('combobox', { name: '直接授权', exact: true }).click()
      await page.getByRole('option', { name: label, exact: true }).click()
      const response = page.waitForResponse(value => value.request().method() === 'PUT' && new URL(value.url()).pathname.endsWith(workspace.workspace_id))
      await row.getByRole('button', { name: '保存', exact: true }).click()
      assert.equal((await response).status(), 204)
      await until(() => row.getByRole('combobox', { name: '直接授权', exact: true }).textContent(), value => value?.includes(label), 'grant view refreshes')
    }
    await choose('只读')
    assert.equal(await status(origin, `/sessions/${sessionId}/history`, readScope), 200)
    assert.equal(await status(origin, '/sessions', runScope, { workspace_id: workspace.workspace_id }), 403)
    assert.equal((await serverRequest(origin, '/workspaces', readScope)).workspaces[0].workspace_id, workspace.workspace_id)
    const readLive = await live(origin, readScope.token, owner.tenantId); sockets.push(readLive.socket)
    readLive.socket.send(JSON.stringify({ type: 'subscribe', subscription_id: 1, session_id: sessionId, metadata: { inbox: false, stats: false, projection: false, questions: false, profile: false, agent_team: false } }))
    await until(() => readLive.frames, frames => frames.some(frame => ['event_batch', 'error'].includes(frame.type)), 'read-scoped Live receives authorized history')
    assert.deepEqual(readLive.frames.filter(frame => frame.type === 'error'), [])
    await choose('允许执行')
    const config = { origin, token: runScope.token, tenant: owner.tenantId, session: sessionId }
    const environment = { ...process.env, TERNILO_SDK_TEST_CONFIG: JSON.stringify(config) }
    const typescript = await execute(process.execPath, ['--experimental-strip-types', path.join(repository, 'sdk/typescript/test/service-live-smoke.ts')], { cwd: repository, env: environment })
    assert.match(typescript.stdout, /TypeScript service Live run verified/)
    const python = await execute(process.env.TERNILO_E2E_PYTHON ?? 'python3', [path.join(repository, 'sdk/python/tests/service_live_smoke.py')], { cwd: repository, env: { ...environment, PYTHONPATH: path.join(repository, 'sdk/python/src') } })
    assert.match(python.stdout, /Python service Live run verified/)
    assert.equal(await readFile(path.join(folder, 'service-typescript.txt'), 'utf8'), 'service TypeScript proof')
    assert.equal(await readFile(path.join(folder, 'service-python.txt'), 'utf8'), 'service Python proof')
    model = await upstream('service-model', 'service-model-private-key')
    await serverRequest(origin, '/admin/models/providers', { ...owner, body: { profile: { ...profile(model.baseUrl), id: 'service-platform', api_key_ref: null }, enabled: true, api_key: 'service-model-private-key' } })
    await serverRequest(origin, '/admin/models/publications', { ...owner, body: { model_id: 'service-model', display_name: 'Service model', provider_id: 'service-platform', upstream_model: 'same-model', enabled: true } })
    const grantBody = { name: 'Service shared model budget', subject: { kind: 'user', id: identity.user.user_id }, model_ids: ['service-model'], monthly_tokens: 1000000, max_concurrent_requests: 2, allow_resource_sharing: false }
    const modelGrant = await serverRequest(origin, '/admin/models/grants', { ...owner, body: grantBody })
    await serverRequest(origin, `/sessions/${sessionId}`, { ...owner, method: 'PATCH', body: { model: { provider: 'platform_model', grant_id: modelGrant.grant_id, model_id: 'service-model' } } })
    const modelTask = async denied => execute(process.execPath, ['--experimental-strip-types', path.join(repository, 'sdk/typescript/test/service-model-smoke.ts')], { cwd: repository, env: { ...environment, TERNILO_SDK_TEST_CONFIG: JSON.stringify({ ...config, denied }) } })
    assert.match((await modelTask(true)).stdout, /Service model denied/)
    assert.equal(model.calls.length, 0, 'workspace execution permission cannot expand model sharing')
    await serverRequest(origin, `/admin/models/grants/${modelGrant.grant_id}`, { ...owner, method: 'PUT', body: { ...grantBody, allow_resource_sharing: true } })
    assert.match((await modelTask(false)).stdout, /Service model completed/)
    assert.equal(await readFile(path.join(folder, 'provider-source.txt'), 'utf8'), 'service-model')
    const usage = await serverRequest(origin, '/model-access/requests?limit=100', owner)
    assert.ok(usage.requests.some(request => request.actor_user_id === account.service_account_id && request.resource_owner_user_id === identity.user.user_id && request.model_beneficiary_user_id === identity.user.user_id && request.accounted_tokens > 0))
    assert.ok(!JSON.stringify(await serverRequest(origin, `/sessions/${sessionId}/history?limit=1000`, runScope)).includes('service-model-private-key'))
    await serverRequest(origin, `/admin/models/grants/${modelGrant.grant_id}`, { ...owner, method: 'DELETE' })
    const callsBeforeRevocation = model.calls.length
    assert.match((await modelTask(true)).stdout, /Service model denied/)
    assert.equal(model.calls.length, callsBeforeRevocation)
    report.checks.push('real model tool loop through Server gateway; explicit sharing, service actor and owner budget attribution; revoked model never falls back')
    const history = await serverRequest(origin, `/sessions/${sessionId}/history?limit=1000`, owner)
    assert.ok(history.events.some(event => event.provenance?.author?.user_id === account.service_account_id), 'canonical history retains service authorship')
    report.checks.push('single-user explicit grants; same Node workspace; TypeScript and Python SDK run/read/write and service authorship')
    for (const width of [390, 320]) {
      await page.setViewportSize({ width, height: 980 })
      assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth))
      await row.scrollIntoViewIfNeeded()
      if (artifacts) await page.screenshot({ path: path.join(artifacts, `service-workspaces-${width}.png`), fullPage: true })
    }
    await choose('未授权')
    await until(() => readLive.frames, frames => frames.some(frame => frame.type === 'error' && frame.subscription_id === 1), 'grant revocation ends existing subscription', 3000)
    assert.ok([400, 403].includes(await status(origin, `/sessions/${sessionId}/history`, runScope)))
    assert.ok([400, 403].includes(await status(origin, `/sessions/${sessionId}/queue`, runScope, { content: { kind: 'prompt', input: '/write forbidden.txt denied' } })))
    assert.equal(await readFile(path.join(folder, 'forbidden.txt'), 'utf8').catch(() => null), null)
    assert.equal(await status(origin, `/sessions/${sessionId}/history`, owner), 200)
    await choose('只读')
    const retained = await live(origin, runScope.token, owner.tenantId); sockets.push(retained.socket)
    const revokedAt = Date.now()
    await serverRequest(origin, `${credentials}/${readGrant.credential.credential_id}`, { ...owner, method: 'DELETE' })
    await until(() => readLive.closed, Boolean, 'revoked credential closes existing Live', 2000)
    assert.equal(retained.closed, false)
    report.checks.push(`individual credential revocation closes Live within ${Date.now() - revokedAt}ms and retains other credentials`)
    await serverRequest(origin, `${root}/${account.service_account_id}`, { ...owner, method: 'PATCH', body: { name: account.name, notes: '', enabled: false, expected_revision: account.revision } })
    await until(() => retained.closed, Boolean, 'disabling service account closes remaining Live', 2000)
    assert.equal(await status(origin, `/sessions/${sessionId}/history`, runScope), 401)
    assert.deepEqual(report.errors, [])
    report.status = 'passed'
  } catch (error) {
    report.status = 'failed'
    if (artifacts && page) await page.screenshot({ path: path.join(artifacts, 'failure.png'), fullPage: true }).catch(() => {})
    throw new Error(`${error.stack}\n${server?.diagnostics() ?? ''}\n${node?.diagnostics() ?? ''}`)
  } finally {
    for (const socket of sockets) socket.close()
    if (artifacts) await writeFile(path.join(artifacts, 'result.json'), `${JSON.stringify(report, null, 2)}\n`)
    await browser?.close(); await stopProcess(node); await stopProcess(server); await model?.close()
    await rm(directory, { recursive: true, force: true })
  }
})
