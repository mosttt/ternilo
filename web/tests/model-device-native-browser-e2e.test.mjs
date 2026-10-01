import assert from 'node:assert/strict'
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { chromium } from 'playwright'
import { approveConnection, localApi, until } from './model-device-fixture.mjs'
import { nativeFixture, settleNativeViewport } from './native-model-fixture.mjs'
import { freePort, initializeServer, repository, serverRequest, startProcess, stopProcess, waitForHttp } from './platform-e2e-fixture.mjs'

for (const protocol of ['google-gemini', 'anthropic-messages']) {
  test(`connected Server exposes ${protocol} platform and private models to an unenrolled local client`, { timeout: 180_000 }, async () => {
    const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-native-device-'))
    const artifacts = process.env.TERNILO_E2E_ARTIFACT_DIR ?? directory
    await mkdir(artifacts, { recursive: true })
    const processes = [], errors = []
    const environment = { XDG_STATE_HOME: path.join(directory, 'state') }
    const upstream = await nativeFixture(protocol)
    let browser, local, approval
    try {
      const server = await initializeServer({ directory: path.join(directory, 'server'), origin: `http://127.0.0.1:${await freePort()}`, mode: 'single_user', environment })
      processes.push(server)
      const owner = (resource, options = {}) => serverRequest(server.origin, resource, { token: server.owner.session.access_token, tenantId: server.owner.session.personal_tenant_id, ...options })
      const profile = { id: 'native-provider', display_name: 'Native Provider', base_url: upstream.baseUrl, protocol, api_key_ref: null,
        defaults: { context_window: 64000, max_output_tokens: 8192 }, models: [{ id: 'native-fixture', settings: { mode: 'inherit' } }], timeout_ms: 10000, max_attempts: 1, retry_base_delay_ms: 1 }
      await owner('/admin/models/providers', { body: { profile, enabled: true, api_key: upstream.apiKey } })
      await owner('/admin/models/publications', { body: { model_id: 'public-native', display_name: 'Native platform model', provider_id: profile.id, upstream_model: 'native-fixture', enabled: true } })
      const grant = await owner('/admin/models/grants', { body: { name: 'Native budget', subject: { kind: 'user', id: server.owner.session.user.user_id }, model_ids: ['public-native'], monthly_tokens: 100000, max_concurrent_requests: 2, allow_resource_sharing: false } })
      await owner('/credentials', { body: { name: 'PRIVATE_NATIVE_KEY', value: upstream.apiKey } })
      await owner('/providers', { body: { ...profile, api_key_ref: 'PRIVATE_NATIVE_KEY' } })
      const origin = `http://127.0.0.1:${await freePort()}`, data = path.join(directory, 'local-data')
      const app = startProcess(process.env.TERNILO_E2E_NODE_BINARY ?? path.join(repository, 'target/debug/ternilo'), ['serve', '--listen', new URL(origin).host, '--data-dir', data], environment)
      processes.push(app); await waitForHttp(origin, app)
      const request = await localApi(origin)
      const folder = path.join(directory, 'workspace'); await mkdir(folder)
      await writeFile(path.join(folder, 'fixture.txt'), 'Native fixture file')
      const workspace = await request('/workspaces', { body: { path: folder } })
      const session = await request('/sessions', { body: { workspace_id: workspace.workspace_id } })
      const sessionId = session.identity.session_id
      await request(`/sessions/${sessionId}`, { method: 'PATCH', body: { title: 'Native connected models' } })
      browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
      local = await browser.newPage({ viewport: { width: 1280, height: 900 }, hasTouch: true, serviceWorkers: 'block' })
      approval = await browser.newPage({ viewport: { width: 320, height: 844 }, hasTouch: true, serviceWorkers: 'block' })
      for (const page of [local, approval]) {
        page.on('pageerror', error => errors.push(error.message))
        page.on('console', message => { if (message.type() === 'error') errors.push(message.text()) })
        page.on('response', response => { if (response.status() >= 400) errors.push(`HTTP ${response.status()} ${new URL(response.url()).pathname}`) })
      }
      await local.goto(origin)
      await approveConnection(local, approval, server, 'Native models', grant.grant_id, profile.id)
      const connections = await request('/model-connections')
      assert.equal(connections.length, 1)
      const deviceId = connections[0].session.identity.device_id
      const providers = await request('/providers')
      assert.equal(providers.length, 2, 'both native-protocol catalogs become selectable local Providers')
      assert.ok(providers.every(provider => provider.protocol === protocol))
      for (const [index, source] of ['platform', 'account'].entries()) {
        const selected = providers.find(provider => provider.base_url.includes(source === 'platform' ? '/device/' : '/device-account/'))
        assert.ok(selected)
        await local.locator('[data-input-bar] [data-model-picker]').click()
        await local.getByRole('menuitem', { name: /^模型/ }).click()
        await local.locator(`[data-model-provider="${selected.id}"]`).getByRole('menuitem').first().click()
        await local.getByRole('textbox', { name: '输入任务', exact: true }).fill(`请用 ${source} 模型读取 fixture.txt。`)
        await local.getByRole('button', { name: '发送', exact: true }).click()
        const events = await until(() => request(`/sessions/${sessionId}/events`), events => events.filter(event => ['turn_finished', 'turn_failed'].includes(event.type)).length === index + 1, `${source} native model task`)
        assert.equal(events.some(event => event.type === 'turn_failed'), false, JSON.stringify({ events, failures: upstream.failures }))
        assert.equal(events.filter(event => event.type === 'tool_call_finished' && event.name === 'read_file').length, index + 1)
        assert.equal(events.filter(event => event.type === 'provider_usage_started').length, 0, 'Server gateway calls do not also enter direct-device observations')
        await local.getByText(upstream.answer, { exact: true }).last().waitFor()
        assert.equal(upstream.signatureChecks.length, index + 1)
        await local.setViewportSize({ width: index ? 390 : 1280, height: 900 })
        await settleNativeViewport(local)
        await local.getByText(upstream.answer, { exact: true }).last().scrollIntoViewIfNeeded()
        await local.screenshot({ path: path.join(artifacts, `${protocol}-device-${source}.png`) })
        assert.equal(await local.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true)
      }
      assert.equal(upstream.requests.length, 4)
      assert.deepEqual(upstream.failures, [])
      const ledger = (await owner('/model-access/requests?limit=50')).requests
      assert.equal(ledger.length, 4)
      assert.equal(ledger.filter(record => record.source === 'platform_grant' && record.grant_id === grant.grant_id).length, 2)
      assert.equal(ledger.filter(record => record.source === 'user_provider').length, 2)
      assert.ok(ledger.every(record => record.origin === 'client_device' && record.key_id === deviceId && record.accounted_tokens === 65))
      assert.equal((await owner('/execution-targets')).executors.length, 0)
      assert.equal((await readFile(path.join(data, 'secrets/model-connections.json'), 'utf8')).includes(upstream.apiKey), false)
      assert.deepEqual(errors, [])
    } catch (error) {
      await local?.screenshot({ path: path.join(artifacts, `${protocol}-device-failure.png`) }).catch(() => {})
      error.message += `\n${JSON.stringify(upstream.failures)}\n${processes.map(process => process.diagnostics()).join('\n')}`
      throw error
    } finally {
      await browser?.close()
      for (const process of processes.reverse()) await stopProcess(process)
      await upstream.close()
      await rm(directory, { recursive: true, force: true })
    }
  })
}
