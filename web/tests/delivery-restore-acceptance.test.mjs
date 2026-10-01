import assert from 'node:assert/strict'
import { copyFile, mkdir, mkdtemp, readFile, rename, rm, stat, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { chromium } from 'playwright'
import { execute, freePort, serverRequest, stopProcess } from './platform-e2e-fixture.mjs'
import { localApi, until } from './model-device-fixture.mjs'
import { reasoningProfile, upstream } from './account-node-provider-fixture.mjs'
import { archiveDirectory, captureLayouts, digest, extractDirectory, installPackages, isolatedEnvironment, openSession, serve, verifyAssets } from './delivery-restore-fixture.mjs'

test('unpacked Local and Server preserve identities, credentials and archives across full and SQLite snapshot restores', { timeout: 360_000 }, async () => {
  const localBinary = process.env.TERNILO_E2E_LOCAL_BINARY ?? process.env.TERNILO_E2E_NODE_BINARY
  const serverBinary = process.env.TERNILO_E2E_SERVER_BINARY
  assert.ok(localBinary && serverBinary, 'Provide final-build Local and Server binaries explicitly')
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-delivery-restore-'))
  const artifacts = process.env.TERNILO_E2E_ARTIFACT_DIR ?? directory
  await mkdir(artifacts, { recursive: true })
  const evidence = { completed: false, packages: [], checks: [], errors: [], network: [] }
  const processes = []
  let browser, model
  try {
    const environment = await isolatedEnvironment(path.join(directory, 'home'))
    const { binaries, localRoot, installation } = await installPackages(directory, {
      ternilo: localBinary, 'ternilo-server': serverBinary,
      'ternilo-plugin': process.env.TERNILO_E2E_PLUGIN_BINARY ?? path.join(path.dirname(localBinary), 'ternilo-plugin'),
    }, environment, evidence.packages)
    const runtime = { env: environment, cwd: installation, timeout: 120_000 }
    await execute('python3', [path.join(localRoot, 'sdk/python/tests/test_smoke.py')], { ...runtime, env: { ...environment, PYTHONPATH: path.join(localRoot, 'sdk/python/src'), TERNILO_BIN: binaries.ternilo } })
    await execute(process.execPath, ['--experimental-strip-types', path.join(localRoot, 'sdk/typescript/test/smoke.ts')], { ...runtime, env: { ...environment, TERNILO_BIN: binaries.ternilo } })
    evidence.checks.push('unpacked Python and TypeScript SDKs execute real file tools over RPC')
    const signingKey = path.join(directory, 'publisher-key.json'), trust = path.join(directory, 'publisher.json'), bundle = path.join(directory, 'echo-extension.json')
    const example = path.join(localRoot, 'examples/rhai-echo-extension')
    await execute(binaries['ternilo-plugin'], ['keygen', '--key-id', 'example-publisher', '--output', signingKey], runtime)
    assert.equal((await stat(signingKey)).mode & 0o777, 0o600)
    await execute(binaries['ternilo-plugin'], ['publisher', '--key', signingKey, '--source', 'https://plugins.ternilo.dev/examples', '--output', trust], runtime)
    await execute(binaries['ternilo-plugin'], ['sign', '--key', signingKey, '--manifest', path.join(example, 'manifest.json'), '--payload', path.join(example, 'extension.rhai'), '--output', bundle], runtime)
    await execute(binaries['ternilo-plugin'], ['verify', '--publisher', trust, '--bundle', bundle], runtime)
    evidence.checks.push('unpacked plugin CLI signs and verifies the packaged Rhai example')
    model = await upstream('delivery-proof', 'synthetic-delivery-secret', 'Restored model task completed.')
    const provider = reasoningProfile(model.baseUrl)
    provider.models[0].settings = { mode: 'automatic', upstream: { context_window: 65536 }, overrides: {} }
    const serverData = path.join(directory, 'server-data'), localData = path.join(directory, 'local-data')
    const serverOrigin = `http://127.0.0.1:${await freePort()}`, localOrigin = `http://127.0.0.1:${await freePort()}`
    const config = path.join(serverData, 'config.json')
    const credentials = { username: 'delivery-owner', email: 'delivery-owner@example.test', password: 'synthetic-delivery-password' }
    const login = { username: credentials.username, password: credentials.password }
    await execute(binaries['ternilo-server'], ['init', '--non-interactive', '--config-dir', path.dirname(config), '--listen', new URL(serverOrigin).host, '--public-url', serverOrigin], {
      ...runtime, env: { ...environment, TERNILO_SERVER_OWNER_USERNAME: credentials.username, TERNILO_SERVER_OWNER_EMAIL: credentials.email, TERNILO_SERVER_OWNER_PASSWORD: credentials.password },
    })
    assert.equal((await stat(config)).mode & 0o777, 0o600)
    const originalConfig = await readFile(config)
    await assert.rejects(() => execute(binaries['ternilo-server'], ['init', '--non-interactive', '--config-dir', path.dirname(config)], runtime), error => error.code === 1 && /already exists/.test(error.stderr))
    assert.deepEqual(await readFile(config), originalConfig, 'repeat initialization does not replace existing identity or master key')
    const server = await serve(binaries['ternilo-server'], ['serve', '--config-dir', path.dirname(config)], environment, installation, `${serverOrigin}/readyz`, processes)
    const account = await serverRequest(serverOrigin, '/auth/login', { body: login })
    const owner = (resource, options = {}) => serverRequest(serverOrigin, resource, { token: account.access_token, tenantId: account.personal_tenant_id, ...options })
    await owner('/credentials', { body: { name: 'SAME_PROVIDER_KEY', value: 'synthetic-delivery-secret' } })
    await owner('/providers', { body: provider })
    const enrolled = await owner(`/tenants/${account.personal_tenant_id}/my-computer-enrollments`, { body: { executor_id: 'delivery-node', project_id: null, ttl_seconds: 600 } })
    const connection = await owner('/enrollments/consume', { body: { token: enrolled.enrollment.token } })
    const nodeEnvironment = { ...environment, TERNILO_LOCAL_TOKEN: connection.credential.token }
    const nodeArgs = (origin, data, gateway) => ['serve', '--listen', new URL(origin).host, '--data-dir', data, '--node-id', 'delivery-node', '--gateway-url', `${gateway.replace('http:', 'ws:')}/api/v1/executors/connect`, '--allow-insecure-gateway']
    const node = await serve(binaries.ternilo, nodeArgs(localOrigin, localData, serverOrigin), nodeEnvironment, installation, localOrigin, processes)
    await verifyAssets(serverOrigin); await verifyAssets(localOrigin)
    const local = await localApi(localOrigin)
    await local('/credentials', { body: { name: 'SAME_PROVIDER_KEY', value: 'synthetic-delivery-secret' } })
    await local('/providers', { body: provider })
    const authorize = await local('/model-connections/authorize', { body: { server_url: serverOrigin, name: 'Restored Server authorization' } })
    const limits = { monthly_tokens: 100000, max_concurrent_requests: 2, requests_per_minute: 10, expires_at_ms: Date.now() + 3600000 }
    await owner('/model-access/device-authorization', { body: { user_code: authorize.user_code, scope: { kind: 'account' }, limits } })
    await new Promise(resolve => setTimeout(resolve, (authorize.interval + 1) * 1000))
    assert.equal((await local(`/model-connections/authorize/${authorize.attempt_id}`, { method: 'POST' })).status, 'connected')
    const modelConnections = await local('/model-connections')
    assert.deepEqual(modelConnections[0].session.identity.limits, limits)
    const deviceId = modelConnections[0].session.identity.device_id
    const workspacePath = path.join(directory, 'workspace')
    await mkdir(workspacePath)
    const workspace = await local('/workspaces', { body: { path: workspacePath } })
    const session = await local('/sessions', { body: { workspace_id: workspace.workspace_id } })
    const localId = session.identity.session_id
    const selection = { provider: 'named_provider', provider_id: provider.id, model: 'same-model', reasoning_effort: 'high' }
    await local(`/sessions/${localId}`, { method: 'PATCH', body: { model: selection } })
    await local(`/sessions/${localId}/turns`, { body: { input: 'Initial installed model task.' } })
    await until(() => local(`/sessions/${localId}/events`), events => events.some(event => event.type === 'turn_finished'), 'initial task finished')
    assert.equal(await readFile(path.join(workspacePath, 'provider-source.txt'), 'utf8'), 'delivery-proof')
    const history = await local(`/sessions/${localId}/events`)
    const archived = await local('/sessions', { body: { workspace_id: workspace.workspace_id } })
    const archivedId = archived.identity.session_id
    await local(`/sessions/${archivedId}/turns`, { body: { input: '/code "archived-installation-proof"' } })
    await local(`/sessions/${archivedId}/archive`, { method: 'POST' })
    const archiveHistory = await local(`/sessions/${archivedId}/archive-events`)
    const state = await until(() => owner('/state'), state => state.sessions.length === 1 && state.workspaces.length === 1, 'installed Node resource mapping')
    const publicId = state.sessions[0].identity.session_id
    const archivedRemote = await until(() => owner('/sessions/archived'), records => records.length === 1, 'archived Node mapping')
    const publicArchiveId = archivedRemote[0].identity.session_id
    const savedProviders = await local('/providers')
    const savedQueue = await local(`/sessions/${localId}/queue`)
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
    const initialPage = await openSession(browser, serverOrigin, publicId, credentials, evidence)
    await initialPage.locator('article[data-role="assistant"]').filter({ hasText: 'Restored model task completed.' }).waitFor()
    await captureLayouts(initialPage, artifacts, 'installed-server')
    await initialPage.context().close()
    await stopProcess(node)
    assert.equal(node.child.exitCode, 0, node.diagnostics())
    const backups = path.join(directory, 'backups')
    await mkdir(backups, { mode: 0o700 })
    const snapshot = path.join(backups, 'online.sqlite3')
    await execute(binaries['ternilo-server'], ['admin', 'backup-sqlite', '--config-dir', path.dirname(config), '--output', snapshot], runtime)
    assert.equal((await stat(snapshot)).mode & 0o777, 0o600)
    const snapshotHash = await digest(snapshot)
    await assert.rejects(() => execute(binaries['ternilo-server'], ['admin', 'backup-sqlite', '--config-dir', path.dirname(config), '--output', snapshot], runtime), error => error.code === 1)
    assert.equal(await digest(snapshot), snapshotHash)
    assert.equal((await fetch(`${serverOrigin}/readyz`)).status, 200, 'SQLite snapshot was taken with Server online')
    await stopProcess(server)
    assert.equal(server.child.exitCode, 0, server.diagnostics())
    for (const [name, source] of [['server', serverData], ['local', localData], ['workspace', workspacePath]]) await archiveDirectory(source, path.join(backups, `${name}.tar.gz`), environment)
    await writeFile(path.join(backups, 'config.json'), originalConfig, { mode: 0o600 })
    await writeFile(path.join(backups, 'node-connection.json'), JSON.stringify({ token: connection.credential.token }), { mode: 0o600 })
    await rename(serverData, `${serverData}-offline`); await rename(localData, `${localData}-offline`)
    await rm(workspacePath, { recursive: true })
    await extractDirectory(path.join(backups, 'workspace.tar.gz'), workspacePath, environment)
    evidence.checks.push('matching online snapshot and stopped full archives; protected permissions and refusal to overwrite')
    for (const mode of ['full', 'snapshot']) {
      const callsBeforeRestore = model.calls.length
      const restoredServer = path.join(directory, `${mode}-server`), restoredLocal = path.join(directory, `${mode}-local`)
      const restoredConfig = path.join(restoredServer, 'config.json')
      if (mode === 'full') await extractDirectory(path.join(backups, 'server.tar.gz'), restoredServer, environment)
      else {
        await mkdir(restoredServer, { mode: 0o700 })
        await copyFile(path.join(backups, 'config.json'), restoredConfig)
        await mkdir(path.join(restoredServer, 'data/db'), { recursive: true, mode: 0o700 })
        await copyFile(snapshot, path.join(restoredServer, 'data/db/server.sqlite3'))
      }
      assert.deepEqual(await readFile(restoredConfig), originalConfig)
      await extractDirectory(path.join(backups, 'local.tar.gz'), restoredLocal, environment)
      const restored = await serve(binaries['ternilo-server'], ['serve', '--config-dir', path.dirname(restoredConfig), '--database-url', `sqlite://${path.join(restoredServer, 'data/db/server.sqlite3')}?mode=rw`, '--workspace-root', path.join(restoredServer, 'data/workspaces')], environment, installation, `${serverOrigin}/readyz`, processes)
      const identity = await serverRequest(serverOrigin, '/auth/login', { body: login })
      assert.equal(identity.user.user_id, account.user.user_id)
      assert.equal(identity.personal_tenant_id, account.personal_tenant_id)
      const ownerRestored = (resource, options = {}) => serverRequest(serverOrigin, resource, { token: identity.access_token, tenantId: identity.personal_tenant_id, ...options })
      assert.ok((await ownerRestored('/providers/discover', { body: { provider_id: provider.id } })).length > 0, 'restored Server decrypts its Provider credential')
      assert.deepEqual((await ownerRestored('/model-access/devices?limit=50')).devices.find(device => device.device_id === deviceId).limits, limits)
      const restoredConnection = JSON.parse(await readFile(path.join(backups, 'node-connection.json'), 'utf8'))
      const restoredNode = await serve(binaries.ternilo, nodeArgs(localOrigin, restoredLocal, serverOrigin), { ...environment, TERNILO_LOCAL_TOKEN: restoredConnection.token }, installation, localOrigin, processes)
      await until(() => ownerRestored('/execution-targets'), result => result.executors.some(executor => executor.executor_id === 'delivery-node' && executor.connected), 'restored Node authentication')
      const restoredApi = await localApi(localOrigin)
      await verifyAssets(serverOrigin); await verifyAssets(localOrigin)
      assert.deepEqual(await restoredApi('/providers'), savedProviders)
      assert.deepEqual(await restoredApi('/model-connections'), modelConnections)
      assert.deepEqual(await restoredApi(`/sessions/${localId}/events`), history)
      assert.deepEqual(await restoredApi(`/sessions/${localId}/queue`), { ...savedQueue, paused: true })
      assert.deepEqual(await restoredApi(`/sessions/${archivedId}/archive-events`), archiveHistory)
      assert.equal((await restoredApi('/sessions')).find(record => record.identity.session_id === localId).model.reasoning_effort, 'high')
      assert.ok((await ownerRestored('/state')).sessions.some(record => record.identity.session_id === publicId))
      assert.equal((await ownerRestored('/sessions/archived'))[0].identity.session_id, publicArchiveId)
      assert.equal((await ownerRestored(`/sessions/${publicArchiveId}/archive-events`)).length, archiveHistory.length)
      assert.equal(model.calls.length, callsBeforeRestore, 'restoration does not resume or replay tasks')
      for (const [surface, origin, sessionId, accountCredentials] of [['server', serverOrigin, publicId, credentials], ['local', localOrigin, localId, null]]) {
        const page = await openSession(browser, origin, sessionId, accountCredentials, evidence)
        await page.locator('article[data-role="assistant"]').filter({ hasText: 'Restored model task completed.' }).first().waitFor()
        const before = model.calls.length
        const turnsBefore = (await restoredApi(`/sessions/${localId}/events`)).filter(event => event.type === 'turn_finished').length
        await page.getByRole('textbox', { name: '输入任务', exact: true }).fill(`${mode} restored ${surface} real task`)
        await page.getByRole('button', { name: '发送', exact: true }).click()
        await until(() => restoredApi(`/sessions/${localId}/events`), events => events.filter(event => event.type === 'turn_finished').length === turnsBefore + 1, 'restored task finished')
        await until(() => page.locator('article[data-role="assistant"]').filter({ hasText: 'Restored model task completed.' }).count(), count => count === turnsBefore + 1, 'browser renders the completed response')
        await page.getByRole('button', { name: '发送', exact: true }).waitFor()
        assert.ok(model.calls.length > before)
        assert.ok(model.calls.slice(before).some(call => call.reasoning_effort === 'high'))
        await captureLayouts(page, artifacts, `${mode}-${surface}`)
        await page.context().close()
      }
      assert.equal(await readFile(path.join(workspacePath, 'provider-source.txt'), 'utf8'), 'delivery-proof')
      await stopProcess(restoredNode); await stopProcess(restored)
      assert.equal(restoredNode.child.exitCode, 0, restoredNode.diagnostics())
      assert.equal(restored.child.exitCode, 0, restored.diagnostics())
      evidence.checks.push({ mode, identityRetained: true, modelsAndLimitsRetained: true, historyRetained: history.length, archivedHistoryRetained: archiveHistory.length, pendingItemsRetained: true, queuePausedOnRestart: true, noReplay: true, realTasks: 2 })
    }
    assert.deepEqual(evidence.errors, [])
    evidence.completed = true
  } finally {
    if (!evidence.completed && browser) for (const [index, context] of browser.contexts().entries()) for (const page of context.pages()) {
      await page.screenshot({ path: path.join(artifacts, `failure-${index}.png`) }).catch(() => {})
    }
    await browser?.close()
    for (const application of processes.reverse()) await stopProcess(application)
    await model?.close()
    await writeFile(path.join(artifacts, 'delivery-results.json'), JSON.stringify(evidence, null, 2))
    await rm(directory, { recursive: true, force: true })
  }
})
