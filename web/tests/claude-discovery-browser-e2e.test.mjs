import assert from 'node:assert/strict'
import { createServer } from 'node:http'
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { chromium } from 'playwright'
import { freePort, initializeServer, repository, serverRequest, startProcess, stopProcess, waitForHttp } from './platform-e2e-fixture.mjs'
import { localApi, openModels } from './model-device-fixture.mjs'

test('Claude catalog discovery uses official headers and pagination, with safe errors in local and Server settings', { timeout: 120000 }, async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-claude-discovery-'))
  const artifacts = process.env.TERNILO_E2E_ARTIFACT_DIR
  if (artifacts) await mkdir(artifacts, { recursive: true })
  const key = 'claude-fixture-secret', privateText = 'private-upstream-detail'
  const processes = [], errors = [], calls = []
  let status = 200, browser, page, expectedError = false
  const upstream = createServer((request, response) => {
    calls.push({ url: request.url, key: request.headers['x-api-key'], version: request.headers['anthropic-version'], bearer: request.headers.authorization })
    if (status !== 200) { response.writeHead(status, { 'content-type': 'application/json' }).end(JSON.stringify({ error: { type: 'fixture_error', message: `${key} ${privateText}` } })); return }
    const next = new URL(request.url, 'http://fixture').searchParams.get('after_id')
    const model = next ? { id: 'claude-second', display_name: 'Second model', max_input_tokens: null, max_tokens: null } : {
      id: 'claude-first', display_name: 'First model', max_input_tokens: 200000, max_tokens: 8192,
      capabilities: { thinking: { supported: true, types: { adaptive: { supported: true } } }, effort: { high: { supported: true }, low: { supported: true } } },
    }
    response.writeHead(200, { 'content-type': 'application/json' }).end(JSON.stringify({ data: [model], has_more: !next, first_id: model.id, last_id: model.id }))
  })
  await new Promise(resolve => upstream.listen(0, '127.0.0.1', resolve))
  try {
    const server = await initializeServer({ directory: path.join(directory, 'server'), origin: `http://127.0.0.1:${await freePort()}`, mode: 'single_user' })
    processes.push(server)
    const owner = (resource, options = {}) => serverRequest(server.origin, resource, { token: server.owner.session.access_token, tenantId: server.owner.session.personal_tenant_id, ...options })
    const localOrigin = `http://127.0.0.1:${await freePort()}`
    const node = startProcess(process.env.TERNILO_E2E_NODE_BINARY ?? path.join(repository, 'target/debug/ternilo'), ['serve', '--data-dir', path.join(directory, 'local'), '--listen', new URL(localOrigin).host])
    processes.push(node); await waitForHttp(localOrigin, node)
    const local = await localApi(localOrigin)
    const profile = { id: 'claude', display_name: 'Claude fixture', base_url: `http://127.0.0.1:${upstream.address().port}/v1`, protocol: 'anthropic-messages', api_key_ref: 'CLAUDE_KEY',
      defaults: { context_window: 100000, max_output_tokens: 4096 }, models: [{ id: 'existing', settings: { mode: 'inherit' } }], timeout_ms: 10000, max_attempts: 1, retry_base_delay_ms: 25 }
    for (const api of [local, owner]) {
      await api('/credentials', { body: { name: 'CLAUDE_KEY', value: key } })
      await api('/providers', { body: profile })
      const found = await api('/providers/discover', { body: { provider_id: 'claude' } })
      assert.deepEqual(found.map(model => model.id), ['claude-first', 'claude-second'])
      assert.equal(found[0].settings.upstream.context_window, 200000)
      assert.equal(found[0].settings.upstream.reasoning.configuration.default_effort, 'high')
      assert.equal(found[1].settings.upstream.context_window ?? null, null)
      for (const failure of [401, 403, 404, 429, 500]) {
        status = failure
        await assert.rejects(() => api('/providers/discover', { body: { provider_id: 'claude' } }), error => {
          assert.ok(error.message.includes(`HTTP ${failure}`), error.message)
          assert.ok(!error.message.includes(key) && !error.message.includes(privateText))
          assert.ok(!error.message.includes('control-plane request failed'))
          return true
        })
      }
      status = 200
    }
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
    page = await browser.newPage({ viewport: { width: 1440, height: 980 }, serviceWorkers: 'block' })
    page.on('pageerror', error => errors.push(error.message))
    page.on('console', message => { if (message.type() === 'error' && !(expectedError && message.text().includes('503'))) errors.push(message.text()) })
    page.on('response', response => { if (response.status() >= 400 && !(expectedError && response.status() === 503 && new URL(response.url()).pathname.endsWith('/providers/discover'))) errors.push(`HTTP ${response.status()} ${new URL(response.url()).pathname}`) })
    for (const scope of ['server', 'local']) {
      if (scope === 'server') {
        await page.goto(`${server.origin}/models`)
        await page.getByLabel('用户名', { exact: true }).fill(server.owner.username)
        await page.getByLabel('密码', { exact: true }).fill(server.owner.password)
        await page.getByRole('button', { name: '登录', exact: true }).click()
        await page.locator('[data-provider-scope="account"]').getByRole('button', { name: '编辑', exact: true }).click()
      } else {
        await page.goto(localOrigin)
        const settings = await openModels(page)
        await settings.getByRole('button', { name: '编辑', exact: true }).click()
      }
      const editor = page.locator('[data-provider-editor="claude"]')
      await editor.locator('summary').filter({ hasText: '自定义设置' }).click()
      status = 401; expectedError = true
      await editor.getByRole('button', { name: '获取可用模型', exact: true }).click()
      await editor.getByText(/provider model discovery returned HTTP 401/).waitFor()
      assert.ok(!(await editor.innerText()).includes(privateText))
      if (artifacts) await page.screenshot({ path: path.join(artifacts, `${scope}-discovery-error.png`) })
      expectedError = false; status = 200
      await editor.getByRole('button', { name: '获取可用模型', exact: true }).click()
      const models = page.getByRole('dialog', { name: '选择要添加的模型' })
      await models.getByText('claude-first', { exact: true }).waitFor()
      await models.getByText('claude-second', { exact: true }).waitFor()
    }
    assert.ok(calls.some(call => call.url === '/v1/models?after_id=claude-first'))
    for (const call of calls) { assert.equal(call.key, key); assert.equal(call.version, '2023-06-01'); assert.equal(call.bearer, undefined) }
    assert.deepEqual(errors, [])
    if (artifacts) await writeFile(path.join(artifacts, 'result.json'), JSON.stringify({ status: 'passed', calls: calls.length, errors }))
  } catch (error) {
    if (artifacts && page) await page.screenshot({ path: path.join(artifacts, 'failure.png') }).catch(() => {})
    throw error
  } finally {
    await browser?.close()
    for (const process of processes.reverse()) await stopProcess(process)
    upstream.closeAllConnections(); await new Promise(resolve => upstream.close(resolve))
    await rm(directory, { recursive: true, force: true })
  }
})
