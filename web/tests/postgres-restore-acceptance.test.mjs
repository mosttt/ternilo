import assert from 'node:assert/strict'
import { randomBytes } from 'node:crypto'
import { chmod, mkdir, mkdtemp, readFile, rename, rm, stat, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { chromium } from 'playwright'
import { execute, freePort, serverRequest, stopProcess } from './platform-e2e-fixture.mjs'
import { localApi, until } from './model-device-fixture.mjs'
import { profile, upstream } from './account-node-provider-fixture.mjs'
import { archiveDirectory, captureLayouts, digest, extractDirectory, isolatedEnvironment, openSession, serve, verifyAssets } from './delivery-restore-fixture.mjs'

test('matched PostgreSQL dump and Server configuration retain restricted-runtime identity, Node access and model ledger', { timeout: 180_000 }, async () => {
  const serverBinary = process.env.TERNILO_E2E_SERVER_BINARY, nodeBinary = process.env.TERNILO_E2E_NODE_BINARY
  assert.ok(serverBinary && nodeBinary, 'Provide the final Server and Node builds explicitly')
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-postgres-restore-'))
  const artifacts = process.env.TERNILO_E2E_ARTIFACT_DIR ?? directory
  await mkdir(artifacts, { recursive: true })
  const environment = await isolatedEnvironment(path.join(directory, 'home'))
  const runtime = { env: environment, cwd: directory, timeout: 60000 }
  const evidence = { completed: false, checks: [], errors: [], network: [] }
  const processes = []
  const docker = process.env.TERNILO_DOCKER ?? 'docker'
  let container, model, browser
  try {
    const password = randomBytes(18).toString('hex'), ownerPassword = randomBytes(18).toString('hex'), appPassword = randomBytes(18).toString('hex')
    const port = await freePort()
    const created = await execute(docker, ['run', '--rm', '-d', '--name', `ternilo-restore-${randomBytes(6).toString('hex')}`, '-e', 'POSTGRES_PASSWORD', '-p', `127.0.0.1:${port}:5432`, process.env.TERNILO_E2E_POSTGRES_IMAGE ?? 'postgres:17'], { ...runtime, env: { ...environment, POSTGRES_PASSWORD: password } })
    container = created.stdout.trim()
    await until(async () => {
      try { await execute(docker, ['exec', container, 'pg_isready', '-h', '127.0.0.1', '-U', 'postgres'], runtime); return true } catch { return false }
    }, Boolean, 'isolated PostgreSQL ready')
    const sql = (database, command) => execute(docker, ['exec', container, 'psql', '-X', '-v', 'ON_ERROR_STOP=1', '-U', 'postgres', '-d', database, '-Atc', command], runtime)
    await sql('postgres', `CREATE ROLE ternilo_runtime NOLOGIN; CREATE ROLE restore_owner LOGIN PASSWORD '${ownerPassword}'; CREATE ROLE restore_app LOGIN PASSWORD '${appPassword}'; GRANT ternilo_runtime TO restore_app;`)
    for (const database of ['source', 'restored']) {
      await sql('postgres', `CREATE DATABASE ${database} OWNER restore_owner`)
      await sql(database, 'REVOKE CREATE ON SCHEMA public FROM PUBLIC; GRANT USAGE ON SCHEMA public TO ternilo_runtime;')
    }
    const databaseUrl = database => `postgres://restore_app:${appPassword}@127.0.0.1:${port}/${database}`
    const migrationUrl = `postgres://restore_owner:${ownerPassword}@127.0.0.1:${port}/source`
    model = await upstream('postgres-restore-proof', 'synthetic-pg-restore-key')
    const origin = `http://127.0.0.1:${await freePort()}`, nodeOrigin = `http://127.0.0.1:${await freePort()}`
    const originalData = path.join(directory, 'server'), config = path.join(originalData, 'config.json')
    const credentials = { username: 'restore-owner', password: 'synthetic-restore-password' }
    await execute(serverBinary, ['setup', '--non-interactive', '--config-dir', path.dirname(config), '--listen', new URL(origin).host, '--public-url', origin], {
      ...runtime, env: { ...environment, TERNILO_DATABASE_URL: databaseUrl('source'), TERNILO_MIGRATION_DATABASE_URL: migrationUrl,
        TERNILO_SERVER_OWNER_USERNAME: credentials.username, TERNILO_SERVER_OWNER_PASSWORD: credentials.password, TERNILO_SERVER_OWNER_EMAIL: 'restore-owner@example.test' },
    })
    const configuration = await readFile(config)
    const server = await serve(serverBinary, ['serve', '--config-dir', path.dirname(config)], environment, directory, `${origin}/readyz`, processes)
    const login = await serverRequest(origin, '/auth/login', { body: credentials })
    const request = (resource, options = {}) => serverRequest(origin, resource, { token: login.access_token, tenantId: login.personal_tenant_id, ...options })
    await request('/admin/instance', { method: 'PATCH', body: { mode: 'multi_user', revision: login.instance.revision } })
    await request('/credentials', { body: { name: 'SAME_PROVIDER_KEY', value: 'synthetic-pg-restore-key' } })
    await request('/providers', { body: profile(model.baseUrl) })
    const registration = await request('/admin/registration')
    await request('/admin/registration', { method: 'PATCH', body: { mode: 'open', require_approval: false, revision: registration.revision } })
    const memberCredentials = { username: 'restore-member', password: 'synthetic-member-password', email: 'restore-member@example.test' }
    const member = (await serverRequest(origin, '/auth/register', { body: memberCredentials })).session
    const authorization = await serverRequest(origin, '/model-device/authorize', { body: { device_name: 'Restored PostgreSQL device' } })
    const limits = { monthly_tokens: 100000, max_concurrent_requests: 2, requests_per_minute: 10, expires_at_ms: Date.now() + 3600000 }
    await request('/model-access/device-authorization', { body: { user_code: authorization.user_code, scope: { kind: 'selected', grants: [], providers: [{ provider_id: 'same', model_ids: ['same-model'] }] }, limits } })
    await new Promise(resolve => setTimeout(resolve, (authorization.interval + 1) * 1000))
    const device = await serverRequest(origin, '/model-device/token', { body: { device_code: authorization.device_code } })
    assert.equal(device.status, 'authorized')
    const deviceId = device.session.identity.device_id
    const gateway = () => fetch(`${origin}/v1/device-account/same/chat/completions`, {
      method: 'POST', headers: { authorization: `Bearer ${device.token}`, 'content-type': 'application/json' },
      body: JSON.stringify({ model: 'same-model', messages: [{ role: 'user', content: 'PostgreSQL restore ledger proof' }], max_tokens: 64, stream: false }),
    })
    const accepted = await gateway()
    assert.equal(accepted.status, 200, await accepted.clone().text())
    assert.equal((await accepted.json()).usage.total_tokens, 42)
    const usage = await request(`/model-access/devices/${deviceId}/usage`)
    assert.equal(usage.used_tokens, 42)
    const restrictedLimits = { ...limits, monthly_tokens: 42 }
    await request(`/model-access/devices/${deviceId}`, { method: 'PATCH', body: restrictedLimits })
    const enrollment = await request(`/tenants/${login.personal_tenant_id}/my-computer-enrollments`, { body: { name: 'pg-restore-node', project_id: null, ttl_seconds: 600 } })
    const enrolledComputerId = enrollment.enrollment.executor_id
    const nodeToken = (await request('/enrollments/consume', { body: { token: enrollment.enrollment.token } })).credential.token
    const nodeData = path.join(directory, 'node')
    const nodeArgs = data => ['serve', '--listen', new URL(nodeOrigin).host, '--data-dir', data, '--node-id', enrolledComputerId, '--gateway-url', `${origin.replace('http:', 'ws:')}/api/v1/executors/connect`, '--allow-insecure-gateway']
    const node = await serve(nodeBinary, nodeArgs(nodeData), { ...environment, TERNILO_LOCAL_TOKEN: nodeToken }, directory, nodeOrigin, processes)
    const local = await localApi(nodeOrigin)
    const folder = path.join(directory, 'workspace'); await mkdir(folder)
    const workspace = await local('/workspaces', { body: { path: folder } })
    const session = await local('/sessions', { body: { workspace_id: workspace.workspace_id } })
    const localId = session.identity.session_id
    await local(`/sessions/${localId}/turns`, { body: { input: '/write before.txt postgres-backup-proof' } })
    const history = await local(`/sessions/${localId}/events`)
    const state = await until(() => request('/state'), result => result.sessions.length === 1, 'Node mapping saved to PostgreSQL')
    const publicId = state.sessions[0].identity.session_id
    const roles = (await sql('source', "SELECT rolsuper,rolbypassrls FROM pg_roles WHERE rolname='restore_app'")).stdout.trim()
    assert.equal(roles, 'f|f')
    const owned = (await sql('source', "SELECT count(*) FROM pg_tables WHERE schemaname='public' AND tableowner='restore_app'")).stdout.trim()
    assert.equal(owned, '0')
    const rls = (await sql('source', "SELECT count(*) FROM pg_class WHERE relnamespace='public'::regnamespace AND relrowsecurity")).stdout.trim()
    assert.ok(Number(rls) > 0)
    await stopProcess(node); await stopProcess(server)
    assert.equal(node.child.exitCode, 0, node.diagnostics()); assert.equal(server.child.exitCode, 0, server.diagnostics())
    const backup = path.join(directory, 'server.tar.gz'), localBackup = path.join(directory, 'local.tar.gz'), dump = path.join(directory, 'database.dump')
    await archiveDirectory(originalData, backup, environment); await archiveDirectory(nodeData, localBackup, environment)
    await execute(docker, ['exec', container, 'pg_dump', '-U', 'postgres', '-d', 'source', '--format=custom', '--file=/tmp/database.dump'], runtime)
    await execute(docker, ['cp', `${container}:/tmp/database.dump`, dump], runtime)
    await chmod(dump, 0o600)
    assert.equal((await stat(dump)).mode & 0o777, 0o600)
    await execute(docker, ['exec', container, 'rm', '/tmp/database.dump'], runtime)
    await execute(docker, ['cp', dump, `${container}:/tmp/restored.dump`], runtime)
    await execute(docker, ['exec', container, 'pg_restore', '-U', 'postgres', '--exit-on-error', '--single-transaction', '-d', 'restored', '/tmp/restored.dump'], runtime)
    await rename(originalData, `${originalData}-offline`); await rename(nodeData, `${nodeData}-offline`)
    const restoredData = path.join(directory, 'restored-server'), restoredLocal = path.join(directory, 'restored-local')
    await extractDirectory(backup, restoredData, environment); await extractDirectory(localBackup, restoredLocal, environment)
    assert.deepEqual(await readFile(path.join(restoredData, 'config.json')), configuration)
    const restoredServer = await serve(serverBinary, ['serve', '--config-dir', path.dirname(path.join(restoredData, 'config.json')), '--database-url', databaseUrl('restored'), '--workspace-root', path.join(restoredData, 'workspaces')], environment, directory, `${origin}/readyz`, processes)
    const restoredIdentity = await serverRequest(origin, '/auth/login', { body: credentials })
    assert.equal(restoredIdentity.user.user_id, login.user.user_id)
    const owner = (resource, options = {}) => serverRequest(origin, resource, { token: restoredIdentity.access_token, tenantId: restoredIdentity.personal_tenant_id, ...options })
    assert.deepEqual(await owner(`/model-access/devices/${deviceId}/usage`), usage)
    assert.deepEqual((await owner('/model-access/devices?limit=50')).devices.find(value => value.device_id === deviceId).limits, restrictedLimits)
    assert.ok((await owner('/providers/discover', { body: { provider_id: 'same' } })).length > 0)
    const callsBefore = model.calls.length
    const blocked = await gateway()
    assert.equal(blocked.status, 429); assert.match(await blocked.text(), /monthly token limit/)
    assert.equal(model.calls.length, callsBefore)
    await owner(`/model-access/devices/${deviceId}`, { method: 'PATCH', body: limits })
    assert.equal((await gateway()).status, 200)
    assert.equal((await owner(`/model-access/devices/${deviceId}/usage`)).used_tokens, 84)
    assert.equal((await sql('source', 'SELECT count(*) FROM control_model_requests')).stdout.trim(), '1')
    assert.equal((await sql('restored', 'SELECT count(*) FROM control_model_requests')).stdout.trim(), '2')
    const restoredNode = await serve(nodeBinary, nodeArgs(restoredLocal), { ...environment, TERNILO_LOCAL_TOKEN: nodeToken }, directory, nodeOrigin, processes)
    await until(() => owner('/execution-targets'), value => value.executors.some(executor => executor.executor_id === enrolledComputerId && executor.connected), 'restored PostgreSQL Node credential')
    const restoredApi = await localApi(nodeOrigin)
    assert.deepEqual(await restoredApi(`/sessions/${localId}/events`), history)
    assert.ok((await owner('/state')).sessions.some(value => value.identity.session_id === publicId))
    assert.equal((await owner(`/sessions/${publicId}/events`)).length, history.length)
    const memberLogin = await serverRequest(origin, '/auth/login', { body: { username: memberCredentials.username, password: memberCredentials.password } })
    assert.equal(memberLogin.user.user_id, member.user.user_id)
    await assert.rejects(() => serverRequest(origin, `/sessions/${publicId}/events`, { token: memberLogin.access_token, tenantId: memberLogin.personal_tenant_id }), /400.*"code":"invalid_input".*"message":"session does not exist"/)
    await assert.rejects(() => serverRequest(origin, `/model-access/devices/${deviceId}/usage`, { token: memberLogin.access_token }), /403.*model device belongs to another account/)
    assert.equal((await sql('restored', "SELECT count(*) FROM pg_class WHERE relnamespace='public'::regnamespace AND relrowsecurity")).stdout.trim(), rls)
    await verifyAssets(origin); await verifyAssets(nodeOrigin)
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
    const page = await openSession(browser, origin, publicId, credentials, evidence)
    await page.getByRole('textbox', { name: '输入任务', exact: true }).fill('/write after.txt postgres-restored-task')
    await page.getByRole('button', { name: '发送', exact: true }).click()
    await until(async () => { try { return await readFile(path.join(folder, 'after.txt'), 'utf8') } catch { return '' } }, value => value === 'postgres-restored-task', 'restored PostgreSQL browser task')
    await page.getByRole('button', { name: '发送', exact: true }).waitFor()
    await captureLayouts(page, artifacts, 'postgres-restored-server')
    await page.context().close()
    assert.deepEqual(evidence.errors, [])
    evidence.checks.push({ dumpSha256: await digest(dump), sourceRlsTables: Number(rls), runtimeSuperuser: false, runtimeBypassRls: false, runtimeOwnedTables: Number(owned), retainedHistory: history.length, retainedUsage: 42, finalUsage: 84, sourceDatabaseUnchanged: true, ownerIsolation: true, restoredNodeTask: true })
    await stopProcess(restoredNode); await stopProcess(restoredServer)
    evidence.completed = true
  } finally {
    await browser?.close()
    for (const application of processes.reverse()) await stopProcess(application)
    await model?.close()
    if (container) await execute(docker, ['rm', '-f', container], runtime)
    await writeFile(path.join(artifacts, 'postgres-restore-results.json'), JSON.stringify(evidence, null, 2))
    await rm(directory, { recursive: true, force: true })
  }
})
