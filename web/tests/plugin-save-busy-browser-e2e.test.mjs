import assert from 'node:assert/strict'
import { createServer } from 'node:http'
import { mkdir, mkdtemp, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { chromium } from 'playwright'
import { localApi, until } from './model-device-fixture.mjs'
import { freePort, initializeServer, repository, serverRequest, startProcess, stopProcess, waitForHttp } from './platform-e2e-fixture.mjs'

async function controlledModel() {
  let holdNext = true
  const pending = new Set()
  const server = createServer(async (request, response) => {
    if (request.method !== 'POST' || request.url !== '/v1/chat/completions') { response.writeHead(404).end(); return }
    const chunks = []
    for await (const chunk of request) chunks.push(chunk)
    const body = JSON.parse(Buffer.concat(chunks).toString())
    const title = body.messages.some(message => message.role === 'system' && String(message.content).startsWith('You name software-agent conversations.'))
    const identity = { id: 'plugin-save-fixture', model: body.model, created: 1 }
    const sse = value => `data: ${JSON.stringify({ ...identity, object: 'chat.completion.chunk', ...value })}\n\n`
    const finish = () => {
      if (response.destroyed) return
      const content = title ? 'Plugin settings validation' : 'The controlled task finished.'
      if (body.stream) response.end(sse({ choices: [{ index: 0, delta: { content }, finish_reason: null }] })
        + sse({ choices: [{ index: 0, delta: {}, finish_reason: 'stop' }] })
        + sse({ choices: [], usage: { prompt_tokens: 20, completion_tokens: 8, total_tokens: 28 } }) + 'data: [DONE]\n\n')
      else response.end(JSON.stringify({ ...identity, object: 'chat.completion', choices: [{ index: 0, message: { role: 'assistant', content }, finish_reason: 'stop' }], usage: { prompt_tokens: 20, completion_tokens: 8, total_tokens: 28 } }))
    }
    response.writeHead(200, { 'content-type': body.stream ? 'text/event-stream' : 'application/json' })
    if (holdNext && !title) {
      holdNext = false
      if (body.stream) response.write(sse({ choices: [{ index: 0, delta: { role: 'assistant', content: 'Task remains active. ' }, finish_reason: null }] }))
      pending.add(finish)
      response.once('close', () => pending.delete(finish))
    } else finish()
  })
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve))
  return { baseUrl: `http://127.0.0.1:${server.address().port}/v1`, pending,
    holdNext: () => { holdNext = true }, finish: () => { for (const finish of pending) finish() },
    close: () => new Promise(resolve => { for (const finish of pending) finish(); server.closeAllConnections(); server.close(resolve) }) }
}

async function reachable(button, page) {
  await button.scrollIntoViewIfNeeded()
  const bounds = await button.boundingBox(), viewport = page.viewportSize()
  assert.ok(bounds && bounds.x >= 0 && bounds.y >= 0 && bounds.x + bounds.width <= viewport.width && bounds.y + bounds.height <= viewport.height)
  if (await button.isEnabled()) assert.ok(await button.evaluate(element => {
    const bounds = element.getBoundingClientRect()
    return element.contains(document.elementFromPoint(bounds.x + bounds.width / 2, bounds.y + bounds.height / 2))
  }))
}

for (const placement of ['local', 'server-node']) {
  test(`plugin saving explains running tasks, preserves edits and returns promptly (${placement})`, { timeout: 120_000 }, async () => {
    const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-plugin-save-busy-'))
    const artifacts = process.env.TERNILO_E2E_ARTIFACT_DIR ?? directory
    await mkdir(artifacts, { recursive: true })
    const processes = [], errors = { page: [], console: [], http: [] }
    let browser, page, upstream, readInbox
    try {
      upstream = await controlledModel()
      const environment = { ...Object.fromEntries(Object.keys(process.env).filter(key => key.startsWith('TERNILO_')).map(key => [key, undefined])), XDG_STATE_HOME: path.join(directory, 'state') }
      const nodeOrigin = `http://127.0.0.1:${await freePort()}`
      let origin = nodeOrigin, server, request, project, executor
      const binary = process.env.TERNILO_E2E_NODE_BINARY ?? path.join(repository, 'target/debug/ternilo')
      const args = ['serve', '--listen', new URL(nodeOrigin).host, '--data-dir', path.join(directory, 'node')]
      if (placement === 'server-node') {
        origin = `http://127.0.0.1:${await freePort()}`
        server = await initializeServer({ directory: path.join(directory, 'server'), origin, environment })
        processes.push(server)
        const tenant = server.owner.session.personal_tenant_id
        request = (url, options = {}) => serverRequest(origin, url, { token: server.owner.session.access_token, tenantId: tenant, ...options })
        project = (await request('/projects')).projects[0]
        const enrollment = (await request(`/tenants/${tenant}/my-computer-enrollments`, { body: { name: 'Plugin save computer', project_id: project.project_id, ttl_seconds: 600 } })).enrollment
        const credential = (await request('/enrollments/consume', { body: { token: enrollment.token } })).credential
        executor = enrollment.executor_id
        args.push('--node-id', executor, '--gateway-url', `${origin.replace('http:', 'ws:')}/api/v1/executors/connect`, '--allow-insecure-gateway')
        environment.TERNILO_LOCAL_TOKEN = credential.token
      }
      const node = startProcess(binary, args, environment)
      processes.push(node); await waitForHttp(nodeOrigin, node)
      const nodeRequest = await localApi(nodeOrigin)
      await nodeRequest('/default-model', { method: 'PUT', body: { provider: 'open_ai_compatible', base_url: upstream.baseUrl, model: 'plugin-save-model', api_key_env: null, timeout_ms: 60_000, max_attempts: 1, retry_base_delay_ms: 10 } })
      const folder = path.join(directory, 'workspace'); await mkdir(folder)
      if (server) await until(() => request('/execution-targets'), value => value.executors.some(item => item.executor_id === executor && item.connected), 'Node connection')
      else request = nodeRequest
      const workspace = server
        ? (await request('/workspaces', { body: { project_id: project.project_id, name: 'Plugin save workspace', placement: 'local_node', executor_id: executor, path: folder } })).workspace
        : await request('/workspaces', { body: { path: folder } })
      const session = await request('/sessions', { body: { workspace_id: workspace.workspace_id } })
      const resource = `/sessions/${session.identity.session_id}`
      readInbox = () => request(`${resource}/queue`)
      browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
      page = await browser.newPage({ locale: 'zh-CN', viewport: { width: 1440, height: 900 }, hasTouch: true, serviceWorkers: 'block' })
      page.on('pageerror', error => errors.page.push(error.message))
      page.on('console', message => { if (message.type() === 'error') errors.console.push(message.text()) })
      page.on('response', response => { if (response.status() >= 400) errors.http.push({ url: response.url(), status: response.status() }) })
      await page.goto(origin)
      if (server) {
        await page.getByLabel('用户名', { exact: true }).fill(server.owner.username)
        await page.getByLabel('密码', { exact: true }).fill(server.owner.password)
        await page.getByRole('button', { name: '登录', exact: true }).click()
      }
      await page.locator('[data-composer-input]').waitFor()
      await request(`${resource}/queue`, { body: { content: { kind: 'prompt', input: 'Hold this controlled plugin save task' } } })
      await until(() => request(`${resource}/queue`), value => value.active_run_id && upstream.pending.size, 'active controlled task')
      if (server) await page.goto(`${origin}/settings/plugins`)
      else {
        await page.getByRole('button', { name: '设置', exact: true }).click()
        await page.getByRole('dialog').getByRole('button', { name: '插件', exact: true }).click()
      }
      const scope = page.locator('[data-settings-surface]')
      const busy = scope.locator('[data-plugin-settings-busy]')
      await busy.waitFor()
      assert.match(await busy.textContent(), /当前会话正在运行/)
      await page.getByRole('tab', { name: '插件配置', exact: true }).click()
      const card = scope.locator('[data-plugin-id="web-fetch"]')
      await card.getByRole('button', { name: '展开: web-fetch', exact: true }).click()
      const field = card.locator('#plugin-web-fetch-timeout_ms')
      await field.fill('45000')
      const save = card.getByRole('button', { name: '保存', exact: true })
      assert.equal(await save.isEnabled(), false)
      assert.equal(await card.locator('> div [role="switch"]').first().isEnabled(), false)
      assert.equal(await field.isEnabled(), true)
      const guardStarted = Date.now()
      let deadline
      try {
        await assert.rejects(Promise.race([
          request(resource, { method: 'PATCH', body: { profile_plugins: [] } }),
          new Promise((_, reject) => { deadline = setTimeout(() => reject(new Error('plugin save did not return promptly')), 3000) }),
        ]), /409.*[Ss]ession is busy/)
      } finally { clearTimeout(deadline) }
      const guardResponseMs = Date.now() - guardStarted
      assert.ok((await request(`${resource}/queue`)).active_run_id, 'rejected save must keep the running task addressable')
      assert.deepEqual((await request('/state')).sessions.find(item => item.identity.session_id === session.identity.session_id).profile_plugins, [])
      await reachable(save, page)
      await page.screenshot({ path: path.join(artifacts, `plugin-save-busy-${placement}-desktop.png`) })
      upstream.finish()
      await busy.waitFor({ state: 'detached' })
      await until(() => save.isEnabled(), value => value, 'save after task completion')
      assert.equal(await field.inputValue(), '45000')
      await save.click()
      await until(() => request('/state'), state => state.sessions.find(item => item.identity.session_id === session.identity.session_id)?.profile_plugins.some(entry => entry.id === 'web-fetch' && entry.config.timeout_ms === 45000), 'saved configuration')
      await card.getByRole('button', { name: '展开: web-fetch', exact: true }).waitFor()
      upstream.holdNext()
      await request(`${resource}/queue`, { body: { content: { kind: 'prompt', input: 'Hold the second controlled plugin save task' } } })
      await busy.waitFor()
      await card.getByRole('button', { name: '展开: web-fetch', exact: true }).click()
      await field.fill('60000')
      await page.setViewportSize({ width: 320, height: 560 })
      await reachable(save, page)
      assert.equal(await save.isEnabled(), false)
      await page.screenshot({ path: path.join(artifacts, `plugin-save-busy-${placement}-mobile-form.png`) })
      const stop = busy.getByRole('button', { name: '停止当前任务', exact: true })
      await reachable(stop, page)
      await page.screenshot({ path: path.join(artifacts, `plugin-save-busy-${placement}-mobile-stop.png`) })
      await stop.tap()
      await busy.waitFor({ state: 'detached' })
      await until(() => request(`${resource}/queue`), value => !value.active_run_id, 'stopped task')
      assert.equal(await field.inputValue(), '60000')
      assert.equal(await save.isEnabled(), true)
      await reachable(save, page); await save.tap()
      await until(() => request('/state'), state => state.sessions.find(item => item.identity.session_id === session.identity.session_id)?.profile_plugins.some(entry => entry.id === 'web-fetch' && entry.config.timeout_ms === 60000), 'saved after stopping')
      assert.deepEqual(errors, { page: [], console: [], http: [] })
      console.log(JSON.stringify({ placement, guardResponseMs, runningTaskPreserved: true, editsPreserved: true, saveAfterFinish: true, stopFromSettings: true, mobileSaveAfterStop: true, errors }))
    } catch (error) {
      await page?.screenshot({ path: path.join(artifacts, `plugin-save-busy-${placement}-failure.png`), fullPage: true }).catch(() => {})
      console.error(JSON.stringify({ errors, inbox: await readInbox?.().catch(() => null), pendingModelRequests: upstream?.pending.size })); console.error(processes.map(process => process.diagnostics()).join('\n').slice(-8000))
      throw error
    } finally {
      await browser?.close(); await upstream?.close()
      for (const process of processes.reverse()) await stopProcess(process)
      await rm(directory, { recursive: true, force: true })
    }
  })
}
