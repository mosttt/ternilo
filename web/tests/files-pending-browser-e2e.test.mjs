import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { createServer } from 'node:http'
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { chromium } from 'playwright'
import { freePort, initializeServer, repository, serverRequest, startProcess, stopProcess, waitForHttp } from './platform-e2e-fixture.mjs'

async function until(read, predicate, label) {
  const deadline = Date.now() + 20_000
  let value
  while (Date.now() < deadline) {
    value = await read()
    if (predicate(value)) return value
    await new Promise(resolve => setTimeout(resolve, 60))
  }
  throw new Error(`${label}: ${JSON.stringify(value)}`)
}

async function heldModel() {
  const responses = new Set()
  let released = false
  let requests = 0
  const finish = response => {
    if (response.destroyed || response.writableEnded) return
    response.writeHead(200, { 'content-type': 'text/event-stream' })
    response.end(`data: ${JSON.stringify({ type: 'response.completed', response: { status: 'completed', output: [{ type: 'message', role: 'assistant', content: [{ type: 'output_text', text: 'Upload retained.' }] }], usage: { input_tokens: 1, output_tokens: 1 } } })}\n\n`)
  }
  const server = createServer((request, response) => {
    request.resume()
    request.on('end', () => {
      requests++
      if (released) finish(response)
      else responses.add(response)
    })
    response.on('close', () => responses.delete(response))
  })
  await new Promise((resolve, reject) => { server.once('error', reject); server.listen(0, '127.0.0.1', resolve) })
  return {
    url: `http://127.0.0.1:${server.address().port}/v1`,
    requests: () => requests,
    release: () => { released = true; for (const response of responses) finish(response); responses.clear() },
    close: () => new Promise(resolve => { server.closeAllConnections(); server.close(resolve) }),
  }
}

async function localToken(origin) {
  const html = await (await fetch(origin)).text()
  return JSON.parse(html.match(/window\.__TERNILO_BOOT__\s*=\s*(\{[^<]+\});/)[1]).apiToken
}

async function verifyAssets(origin) {
  const hashes = {}
  for (const name of ['app.js', 'app.css']) {
    const response = await fetch(`${origin}/assets/${name}`)
    assert.equal(response.status, 200)
    const digest = bytes => createHash('sha256').update(bytes).digest('hex')
    hashes[name] = digest(Buffer.from(await response.arrayBuffer()))
    assert.equal(hashes[name], digest(await readFile(path.join(repository, 'web/dist/assets', name))))
  }
  return hashes
}

async function download(page, name) {
  const row = page.locator('[data-file-id]').filter({ hasText: name })
  const received = page.waitForEvent('download')
  await row.getByRole('button', { name: `下载 ${name}`, exact: true }).click()
  const saved = await received
  assert.equal(await saved.failure(), null)
  return readFile(await saved.path())
}

for (const placement of ['local', 'cloud']) {
  test(`${placement} accepted uploads remain available while queued, removed, archived and restarted`, { timeout: 180_000 }, async t => {
    const directory = await mkdtemp(path.join(tmpdir(), `ternilo-pending-files-${placement}-`))
    const artifacts = path.join(process.env.TERNILO_E2E_ARTIFACT_DIR ?? directory, placement)
    await mkdir(artifacts, { recursive: true })
    const origin = `http://127.0.0.1:${await freePort()}`
    const model = await heldModel()
    const dataDirectory = path.join(directory, 'data')
    let app, browser, page, token, tenantId, configPath
    const errors = { page: [], console: [], network: [] }
    let hashes
    try {
      if (placement === 'cloud') {
        app = await initializeServer({ directory: path.join(directory, 'server'), origin, managedExecutionEnabled: true })
        token = app.owner.session.access_token
        tenantId = app.owner.session.personal_tenant_id
        configPath = app.configPath
      } else {
        app = startProcess(path.join(repository, 'target/debug/ternilo'), ['serve', '--listen', new URL(origin).host, '--data-dir', dataDirectory])
        await waitForHttp(origin, app)
        token = await localToken(origin)
      }
      hashes = await verifyAssets(origin)
      const api = (resource, body, method) => serverRequest(origin, resource, { token, tenantId, body, method })
      await api('/credentials', { name: 'TERNILO_PROVIDER_PENDING_API_KEY', value: 'pending-file-fixture' })
      await api('/providers', {
        id: 'pending-files', display_name: 'Pending files', base_url: model.url, protocol: 'openai-responses', api_key_ref: 'TERNILO_PROVIDER_PENDING_API_KEY',
        defaults: { context_window: 128000, max_output_tokens: 8192 }, models: [{ id: 'fixture', settings: { mode: 'inherit' } }],
        timeout_ms: 120000, max_attempts: 1, retry_base_delay_ms: 50,
      })
      const workspacePath = path.join(directory, 'workspace')
      await mkdir(workspacePath)
      const workspace = placement === 'local' ? await api('/workspaces', { path: workspacePath })
        : (await api('/workspaces', { project_id: app.owner.session.personal_project_id, name: 'Pending uploads', placement: 'cloud' })).workspace
      const session = await api('/sessions', { workspace_id: workspace.workspace_id, permissions: 'workspace_write' })
      const sessionId = session.identity.session_id
      await api(`/sessions/${sessionId}`, { title: 'Queued uploads', model: { provider: 'named_provider', provider_id: 'pending-files', model: 'fixture' } }, 'PATCH')
      await api(`/sessions/${sessionId}/queue`, { content: { kind: 'prompt', input: 'Keep this first task active.' } })
      if (placement === 'local') await until(model.requests, count => count === 1, 'first model request is held')

      browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
      page = await (await browser.newContext({ locale: 'zh-CN', viewport: { width: 1280, height: 900 }, serviceWorkers: 'block' })).newPage()
      page.on('pageerror', error => errors.page.push(error.message))
      page.on('console', message => { if (message.type() === 'error') errors.console.push(message.text()) })
      page.on('response', response => { if (response.status() >= 400) errors.network.push(`${response.status()} ${response.url()}`) })
      page.on('requestfailed', request => { if (request.failure()?.errorText !== 'net::ERR_ABORTED') errors.network.push(`${request.url()}: ${request.failure()?.errorText}`) })
      await page.goto(origin)
      if (placement === 'cloud') {
        await page.getByLabel('用户名', { exact: true }).fill(app.owner.username)
        await page.getByLabel('密码', { exact: true }).fill(app.owner.password)
        await page.getByRole('button', { name: '登录', exact: true }).click()
        await page.getByRole('dialog').waitFor({ state: 'hidden' })
      }
      const bytes = Buffer.from('\ufeffQueued before execution.\r\n排队附件 📄\r\n')
      await page.getByRole('textbox', { name: '输入任务', exact: true }).waitFor()
      await page.locator('input[type="file"]').setInputFiles({ name: 'pending-upload.txt', mimeType: 'text/plain', buffer: bytes })
      await page.getByRole('button', { name: '移除 pending-upload.txt', exact: true }).waitFor()
      await page.getByRole('textbox', { name: '输入任务', exact: true }).fill('Read this file after the first task.')
      const acceptedResponse = page.waitForResponse(response => new URL(response.url()).pathname === `/api/v1/sessions/${sessionId}/queue` && response.request().method() === 'POST')
      await page.getByRole('textbox', { name: '输入任务', exact: true }).press('Enter')
      assert.equal((await acceptedResponse).status(), 201)
      const accepted = await (await acceptedResponse).json()
      const removed = await api(`/sessions/${sessionId}/queue`, {
        content: { kind: 'prompt', input: 'This queue entry will be removed.' },
        attachments: [{ name: 'removed-upload.txt', media_type: 'text/plain', content: bytes.toString('utf8') }],
      })
      const list = () => api(`/files?session_id=${encodeURIComponent(sessionId)}`)
      const inventory = await until(list, result => result.items.length === 2, 'accepted uploads are visible before execution')
      assert.ok(inventory.items.every(file => file.event_seq === null && file.kind === 'upload'))
      const identity = items => items.map(({ id, occurred_at_ms }) => ({ id, occurred_at_ms })).sort((a, b) => a.id.localeCompare(b.id))
      const before = identity(inventory.items)
      assert.ok(before.some(file => file.id.includes(accepted.id)))
      assert.ok(before.some(file => file.id.includes(removed.id)))
      await page.goto(`${origin}/files?session_id=${encodeURIComponent(sessionId)}`)
      await until(() => page.locator('[data-file-id]').count(), count => count === 2, 'Files renders both pending uploads')
      assert.deepEqual(await download(page, 'pending-upload.txt'), bytes)
      await api(`/sessions/${sessionId}/queue/${encodeURIComponent(removed.id)}`, undefined, 'DELETE')
      assert.deepEqual(identity((await list()).items), before, 'removing a queued task retains its accepted upload')
      await page.getByRole('button', { name: '刷新', exact: true }).click()
      assert.deepEqual(await download(page, 'removed-upload.txt'), bytes)
      if (placement === 'local') {
        model.release()
        await until(() => api(`/sessions/${sessionId}/queue`), queue => !queue.active_run_id && queue.items.length === 0, 'remaining messages finish')
        const consumed = await until(list, result => result.items.some(file => file.id.includes(accepted.id) && file.event_seq !== null), 'accepted upload is associated with its message')
        assert.deepEqual(identity(consumed.items), before, 'consumption preserves file identity and pagination time')
      } else {
        assert.equal(model.requests(), 0, 'no Worker or model execution is needed to retain uploaded files')
      }
      await api(`/sessions/${sessionId}/archive`, {}, 'POST')
      const archived = await list()
      assert.ok(archived.items.every(file => file.session_archived))
      assert.deepEqual(identity(archived.items), before)
      await page.reload()
      await page.setViewportSize({ width: 390, height: 844 })
      await until(() => page.locator('[data-file-id]').count(), count => count === 2, 'archived uploads remain visible on mobile')
      assert.deepEqual(await download(page, 'removed-upload.txt'), bytes)
      assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true)
      await page.screenshot({ path: path.join(artifacts, 'pending-files-mobile.png') })

      await page.goto('about:blank')
      await stopProcess(app)
      app = placement === 'local'
        ? startProcess(path.join(repository, 'target/debug/ternilo'), ['serve', '--listen', new URL(origin).host, '--data-dir', dataDirectory])
        : startProcess(path.join(repository, 'target/debug/ternilo-server'), ['serve', '--config-dir', path.dirname(configPath)])
      await waitForHttp(placement === 'local' ? origin : `${origin}/readyz`, app)
      if (placement === 'local') token = await localToken(origin)
      await page.goto(`${origin}/files?session_id=${encodeURIComponent(sessionId)}`)
      await until(() => page.locator('[data-file-id]').count(), count => count === 2, 'accepted files survive restart')
      assert.deepEqual(identity((await list()).items), before)
      assert.deepEqual(await download(page, 'pending-upload.txt'), bytes)
      await page.goto(`${origin}/files`)
      await api(`/sessions/${sessionId}`, undefined, 'DELETE')
      assert.deepEqual((await api('/files')).items, [])
      assert.deepEqual(errors, { page: [], console: [], network: [] })
    } catch (error) {
      await page?.screenshot({ path: path.join(artifacts, 'failure.png'), fullPage: true }).catch(() => {})
      t.diagnostic(app?.diagnostics() ?? '')
      throw error
    } finally {
      await writeFile(path.join(artifacts, 'observations.json'), `${JSON.stringify({ hashes, errors }, null, 2)}\n`)
      model.release()
      await browser?.close()
      await stopProcess(app)
      await model.close()
      await rm(directory, { recursive: true, force: true })
    }
  })
}
