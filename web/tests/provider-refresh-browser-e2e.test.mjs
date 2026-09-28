import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { chromium } from 'playwright'
import { computerModels } from './account-node-provider-fixture.mjs'
import { localApi, openModels, until } from './model-device-fixture.mjs'
import { freePort, initializeServer, repository, serverRequest, startProcess, stopProcess, waitForHttp } from './platform-e2e-fixture.mjs'

test('Local provider changes refresh in an open Server computer catalog without losing drafts', { timeout: 180000 }, async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-provider-refresh-'))
  const artifacts = process.env.TERNILO_E2E_ARTIFACT_DIR ?? directory
  await mkdir(artifacts, { recursive: true })
  const processes = [], errors = [], network = [], layouts = []
  let browser
  try {
    const server = await initializeServer({ directory: path.join(directory, 'server'), origin: `http://127.0.0.1:${await freePort()}`, mode: 'single_user', environment: { XDG_STATE_HOME: path.join(directory, 'state') } })
    processes.push(server)
    const tenantId = server.owner.session.personal_tenant_id
    const request = (resource, options = {}) => serverRequest(server.origin, resource, { token: server.owner.session.access_token, tenantId, ...options })
    const enrolled = await request(`/tenants/${tenantId}/my-computer-enrollments`, { body: { executor_id: 'refresh-node', project_id: null, ttl_seconds: 600 } })
    const credential = await request('/enrollments/consume', { body: { token: enrolled.enrollment.token } })
    const origin = `http://127.0.0.1:${await freePort()}`
    const node = startProcess(process.env.TERNILO_E2E_NODE_BINARY ?? path.join(repository, 'target/debug/ternilo'), ['serve', '--listen', new URL(origin).host, '--data-dir', path.join(directory, 'node'), '--node-id', 'refresh-node', '--gateway-url', `${server.origin.replace('http:', 'ws:')}/api/v1/executors/connect`, '--allow-insecure-gateway'], { TERNILO_LOCAL_TOKEN: credential.credential.token, XDG_STATE_HOME: path.join(directory, 'local-state') })
    processes.push(node)
    await waitForHttp(origin, node)
    const local = await localApi(origin)
    const profile = { id: 'initial', display_name: 'Initial Provider', base_url: 'http://127.0.0.1:1/v1', protocol: 'openai-responses', api_key_ref: null, defaults: { context_window: 128000, max_output_tokens: 16000 }, models: [{ id: 'initial-model', settings: { mode: 'inherit' } }], timeout_ms: 1000, max_attempts: 1, retry_base_delay_ms: 10 }
    await local('/providers', { body: profile })
    const authorization = await local('/model-connections/authorize', { body: { server_url: server.origin, name: 'Empty Server authorization' } })
    await request('/model-access/device-authorization', { body: { user_code: authorization.user_code, scope: { kind: 'account' } } })
    await new Promise(resolve => setTimeout(resolve, (authorization.interval + 1) * 1000))
    const connection = await local(`/model-connections/authorize/${authorization.attempt_id}`, { method: 'POST' })
    assert.equal(connection.status, 'connected')
    await until(() => request('/model-computers'), values => values.some(value => value.connected), 'Node connected')
    for (const target of [origin, server.origin]) {
      for (const asset of ['app.js', 'app.css']) {
        const response = await fetch(`${target}/assets/${asset}`)
        assert.equal(response.status, 200)
        assert.equal(createHash('sha256').update(Buffer.from(await response.arrayBuffer())).digest('hex'), createHash('sha256').update(await readFile(path.join(repository, 'web/dist/assets', asset))).digest('hex'))
      }
    }
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
    const serverPage = await browser.newPage({ viewport: { width: 1366, height: 900 }, hasTouch: true, serviceWorkers: 'block' })
    const localPage = await browser.newPage({ viewport: { width: 1366, height: 900 }, hasTouch: true, serviceWorkers: 'block' })
    for (const page of [serverPage, localPage]) {
      page.setDefaultTimeout(15000)
      page.on('pageerror', error => errors.push(error.message))
      page.on('console', message => { if (message.type() === 'error') errors.push(message.text()) })
      page.on('response', response => {
        network.push({ url: response.url(), status: response.status(), method: response.request().method() })
        if (response.status() >= 400) errors.push(`${response.status()} ${response.url()}`)
      })
      page.on('requestfailed', failed => { if (failed.failure()?.errorText !== 'net::ERR_ABORTED') errors.push(`${failed.url()}: ${failed.failure()?.errorText}`) })
    }
    await serverPage.goto(`${server.origin}/models`)
    await serverPage.getByLabel('用户名', { exact: true }).fill(server.owner.username)
    await serverPage.getByLabel('密码', { exact: true }).fill(server.owner.password)
    await serverPage.getByRole('button', { name: '登录', exact: true }).click()
    const scope = await computerModels(serverPage, tenantId, 'refresh-node')
    await scope.getByText('Initial Provider', { exact: true }).waitFor()
    await scope.getByRole('button', { name: '编辑', exact: true }).click()
    const serverEditor = scope.locator('[data-provider-editor="initial"]')
    await serverEditor.locator('summary').filter({ hasText: '自定义设置' }).click()
    await serverEditor.getByLabel('显示名称', { exact: true }).fill('Unsaved Server draft')
    await localPage.goto(origin)
    const settings = await openModels(localPage)
    for (const width of [1366, 390, 320]) {
      await serverPage.setViewportSize({ width, height: 900 })
      await localPage.setViewportSize({ width, height: 900 })
      const providerId = `added-${width}`
      const name = `Added locally ${width}`
      await settings.getByRole('button', { name: '添加 Provider', exact: true }).click()
      const editor = settings.locator('[data-provider-editor="new"]')
      await editor.getByLabel('Provider ID', { exact: true }).fill(providerId)
      await editor.getByLabel('显示名称', { exact: true }).fill(name)
      await editor.getByLabel('API 地址', { exact: true }).fill('http://127.0.0.1:1/v1')
      await editor.getByLabel('模型 ID 1', { exact: true }).fill(`model-${width}`)
      await editor.getByRole('button', { name: '添加 Provider', exact: true }).click()
      await editor.waitFor({ state: 'hidden' })
      assert.ok((await request('/providers?executor_id=refresh-node')).some(value => value.id === providerId), 'Server can already read the provider from Node')
      assert.equal(await scope.getByText(name, { exact: true }).count(), 0, 'existing browser inventory is cached before explicit refresh')
      const refreshed = serverPage.waitForResponse(response => response.request().method() === 'GET' && new URL(response.url()).pathname === '/api/v1/providers' && new URL(response.url()).searchParams.get('executor_id') === 'refresh-node')
      await serverPage.locator('[data-computer-models]').getByRole('button', { name: '刷新', exact: true }).click()
      assert.equal((await refreshed).status(), 200)
      await scope.getByText(name, { exact: true }).waitFor()
      assert.equal(await serverEditor.getByLabel('显示名称', { exact: true }).inputValue(), 'Unsaved Server draft')
      const gap = await settings.evaluate(root => {
        const connection = root.querySelector('[data-model-connections]').getBoundingClientRect()
        const inventory = root.querySelector('[data-provider-inventory]').getBoundingClientRect()
        return inventory.top - connection.bottom
      })
      assert.ok(gap >= 20, `connection and provider cards are separated: ${gap}`)
      assert.ok(await settings.evaluate(element => element.scrollWidth <= element.clientWidth))
      assert.ok(await serverPage.evaluate(() => document.documentElement.scrollWidth <= innerWidth))
      await settings.locator('[data-model-connections]').scrollIntoViewIfNeeded()
      await localPage.screenshot({ path: path.join(artifacts, `local-spacing-${width}.png`), fullPage: true })
      await scope.getByText(name, { exact: true }).scrollIntoViewIfNeeded()
      await serverPage.screenshot({ path: path.join(artifacts, `server-refresh-${width}.png`), fullPage: true })
      layouts.push({ width, gap, providerId, draftPreserved: true })
    }
    await local('/providers', { body: { ...profile, id: 'direct-refresh', display_name: 'Direct refresh result' } })
    await scope.getByRole('button', { name: '刷新 Provider', exact: true }).click()
    await scope.getByText('Direct refresh result', { exact: true }).waitFor()
    assert.equal(await serverEditor.getByLabel('显示名称', { exact: true }).inputValue(), 'Unsaved Server draft')
    await serverPage.getByRole('button', { name: '收起模型', exact: true }).click()
    await local('/providers', { body: { ...profile, id: 'collapsed', display_name: 'Added while collapsed' } })
    const computers = serverPage.waitForResponse(response => new URL(response.url()).pathname === '/api/v1/model-computers')
    await serverPage.getByRole('button', { name: '刷新', exact: true }).click()
    await computers
    await serverPage.getByRole('button', { name: '查看模型', exact: true }).click()
    await scope.getByText('Added while collapsed', { exact: true }).waitFor()
    assert.equal((await local('/model-connections'))[0].session.grants.length, 0, 'local Provider additions do not become Server grants')
    assert.deepEqual(errors, [])
  } finally {
    await writeFile(path.join(artifacts, 'provider-refresh-results.json'), JSON.stringify({ layouts, errors, network }, null, 2))
    await browser?.close()
    for (const process of processes.reverse()) await stopProcess(process)
    await rm(directory, { recursive: true, force: true })
  }
})
