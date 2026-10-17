import assert from 'node:assert/strict'
import { createServer } from 'node:http'
import { connect, createServer as tcpServer } from 'node:net'
import { mkdtemp, mkdir, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { chromium } from 'playwright'
import { freePort, initializeServer, repository, serverRequest, startProcess, stopProcess, waitForHttp } from './platform-e2e-fixture.mjs'
import { localApi, until } from './model-device-fixture.mjs'

async function gatewayProxy(origin) {
  const target = new URL(origin), sockets = new Set()
  let enabled = true
  const server = tcpServer(client => {
    if (!enabled) { client.destroy(); return }
    const upstream = connect(Number(target.port), target.hostname)
    sockets.add(client).add(upstream)
    client.on('error', () => upstream.destroy()); upstream.on('error', () => client.destroy())
    client.on('close', () => { sockets.delete(client); upstream.destroy() })
    upstream.on('close', () => { sockets.delete(upstream); client.destroy() })
    client.pipe(upstream).pipe(client)
  })
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve))
  const available = value => { enabled = value; if (!value) for (const socket of sockets) socket.destroy() }
  return { url: `ws://127.0.0.1:${server.address().port}/api/v1/executors/connect`, available,
    close: async () => { available(false); await new Promise(resolve => server.close(resolve)) } }
}

async function slowModel(folder) {
  const pending = new Set()
  const server = createServer(async (request, response) => {
    const chunks = []; for await (const chunk of request) chunks.push(chunk)
    const body = JSON.parse(Buffer.concat(chunks).toString())
    const latestUser = body.messages.findLastIndex(message => message.role === 'user')
    const current = body.messages.slice(latestUser), prompt = JSON.stringify(current[0])
    const tool = id => current.some(message => message.tool_call_id === id)
    if (!body.stream) {
      response.writeHead(200, { 'content-type': 'application/json' })
      response.end(JSON.stringify({ id: 'title', model: body.model, choices: [{ index: 0, message: { role: 'assistant', content: 'Feedback fixture' }, finish_reason: 'stop' }] })); return
    }
    response.writeHead(200, { 'content-type': 'text/event-stream' }); response.flushHeaders()
    const send = delta => response.write(`data: ${JSON.stringify({ choices: [{ index: 0, delta, finish_reason: null }] })}\n\n`)
    const finish = (delta, calls = false) => {
      send(delta); response.end(`data: ${JSON.stringify({ choices: [{ index: 0, delta: {}, finish_reason: calls ? 'tool_calls' : 'stop' }] })}\n\ndata: [DONE]\n\n`)
    }
    const call = (id, name, args) => finish({ tool_calls: [{ index: 0, id, type: 'function', function: { name, arguments: JSON.stringify(args) } }] }, true)
    if (body.messages.some(message => message.role === 'system' && String(message.content).includes('You name software-agent conversations'))) return finish({ content: 'Feedback fixture' })
    if (body.model === 'child' || prompt.includes('hold-')) {
      send({ reasoning_content: 'Inspecting fixture evidence.' })
      const release = () => { pending.delete(release); finish({ content: 'Fixture finished.' }) }
      pending.add(release); response.on('close', () => pending.delete(release)); return
    }
    if (prompt.includes('delegate') && !tool('spawn')) return call('spawn', 'spawn_agent', { task: 'Inspect fixture evidence', label: 'Review fixture', role: 'investigator', background: false })
    if (prompt.includes('delivery')) {
      if (!tool('todo-start')) return call('todo-start', 'todo_write', { items: [{ step: 'Deliver files', status: 'in_progress' }] })
      if (!tool('version-one')) return call('version-one', 'write_file', { path: 'report.txt', content: 'version one' })
      if (!tool('version-two')) return call('version-two', 'write_file', { path: 'report.txt', content: 'version two' })
      if (!tool('todo-done')) return call('todo-done', 'todo_write', { items: [{ step: 'Deliver files', status: 'completed' }] })
      return finish({ content: `Delivery complete.\n\n![Fixture map](${folder}/image.png)` })
    }
    finish({ content: 'Parent finished.' })
  })
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve))
  return { url: `http://127.0.0.1:${server.address().port}/v1`, release: () => { for (const release of [...pending]) release() },
    close: async () => { server.closeAllConnections(); await new Promise(resolve => server.close(resolve)) } }
}

const labels = {
  zh: { permission: '会话权限', busy: '当前轮次正在运行', read: '只读', full: '完整访问', confirm: '允许完整访问', team: '打开 Agent Team', open: '打开独立会话', offline: '电脑离线，运行状态待确认', parent: '上级目录' },
  en: { permission: 'Session permission', busy: 'Wait for the current turn', read: 'Read only', full: 'Full access', confirm: 'Allow full access', team: 'Open Agent Team', open: 'Open independent session', offline: 'Computer offline. Run status is unconfirmed.', parent: 'Parent folder' },
  ko: { permission: '세션 권한', busy: '현재 실행이 끝나거나', read: '읽기 전용', full: '전체 접근', confirm: '전체 접근 허용', team: '에이전트 팀 열기', open: '독립 세션 열기', offline: '컴퓨터가 오프라인입니다. 실행 상태를 확인할 수 없습니다.', parent: '상위 폴더' },
}

test('real Node permissions, child navigation, offline feedback and delivered files', { timeout: 300_000 }, async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-runtime-feedback-'))
  const processes = [], errors = []
  let browser, model, proxy, page
  try {
    const folder = path.join(directory, 'workspace'); await mkdir(folder)
    const home = path.join(directory, 'home'); await mkdir(home)
    await writeFile(path.join(folder, 'image.png'), await readFile(path.join(repository, 'web/public/assets/icon-192.png')))
    model = await slowModel(folder)
    const server = await initializeServer({ directory: path.join(directory, 'server'), origin: `http://127.0.0.1:${await freePort()}` }); processes.push(server)
    const token = server.owner.session.access_token, tenantId = server.owner.session.personal_tenant_id
    const request = (url, options = {}) => serverRequest(server.origin, url, { token, tenantId, ...options })
    const enrolled = await request(`/tenants/${tenantId}/my-computer-enrollments`, { body: { name: 'Feedback node', project_id: null, ttl_seconds: 600 } })
    const credential = await request('/enrollments/consume', { body: { token: enrolled.enrollment.token } })
    proxy = await gatewayProxy(server.origin)
    const origin = `http://127.0.0.1:${await freePort()}`
    const node = startProcess(process.env.TERNILO_E2E_NODE_BINARY ?? path.join(repository, 'target/debug/ternilo'), ['serve', '--listen', new URL(origin).host, '--data-dir', path.join(directory, 'node'), '--node-id', enrolled.enrollment.executor_id, '--gateway-url', proxy.url, '--allow-insecure-gateway'], { HOME: home, TERNILO_LOCAL_TOKEN: credential.credential.token, XDG_STATE_HOME: path.join(directory, 'state') }); processes.push(node)
    await waitForHttp(origin, node)
    const local = await localApi(origin)
    await local('/providers', { body: { id: 'fixture', display_name: 'Fixture', base_url: model.url, protocol: 'openai-chat-completions', api_key_ref: null,
      defaults: { context_window: 128000, max_output_tokens: 4096 }, models: ['lead', 'child'].map(id => ({ id, settings: { mode: 'inherit' } })), timeout_ms: 0, max_attempts: 1, retry_base_delay_ms: 10 } })
    const workspace = await local('/workspaces', { body: { path: folder } })
    const localSession = await local('/sessions', { body: { workspace_id: workspace.workspace_id, permissions: 'full_access' } })
    await local(`/sessions/${localSession.identity.session_id}`, { method: 'PATCH', body: { model: { provider: 'named_provider', provider_id: 'fixture', model: 'lead' }, profile_plugins: [{ id: 'subagents', kind: 'ternilo.subagents.in_process', enabled: true, config: { coordination: { mode: 'assisted', investigator: { model: { provider: 'named_provider', provider_id: 'fixture', model: 'child' } } } } }] } })
    const state = await until(() => request('/state'), state => state.sessions.length === 1, 'mapped parent')
    const parentId = state.sessions[0].identity.session_id
    const resource = `/sessions/${parentId}`
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
    page = await browser.newPage({ locale: 'zh-CN', viewport: { width: 1366, height: 900 }, serviceWorkers: 'block' })
    page.on('pageerror', error => errors.push(error.message))
    page.on('console', message => { if (message.type() === 'error') errors.push(message.text()) })
    page.on('response', response => { if (response.status() >= 400) errors.push(`${response.status()} ${new URL(response.url()).pathname}`) })
    await page.goto(server.origin)
    await page.getByLabel('用户名', { exact: true }).fill(server.owner.username)
    await page.getByLabel('密码', { exact: true }).fill(server.owner.password)
    await page.getByRole('button', { name: '登录', exact: true }).click()
    await page.locator(`[data-sidebar-session-row][data-session-id="${parentId}"]`).click()
    await page.getByRole('button', { name: '添加工作区', exact: true }).click()
    const placement = page.getByRole('dialog', { name: '打开工作区', exact: true })
    await placement.getByRole('button', { name: '选择文件夹', exact: true }).click()
    const picker = page.getByRole('dialog', { name: '选择 Feedback node 上的工作文件夹', exact: true })
    await until(() => picker.locator('[data-directory-path]').textContent(), value => value === home, 'visible home path')
    await picker.getByRole('button', { name: '上级目录', exact: true }).click()
    await until(() => picker.locator('[data-directory-path]').textContent(), value => value === directory, 'navigate above home')
    await page.keyboard.press('Escape'); await page.keyboard.press('Escape')
    for (const [locale, text] of Object.entries(labels)) {
      await page.evaluate(locale => localStorage.setItem('ternilo.locale', locale), locale); await page.reload()
      await request(`${resource}/queue`, { body: { content: { kind: 'prompt', input: `hold-${locale}` } } })
      await page.locator('[data-execution-phase="running"]').waitFor()
      const control = page.locator('[data-input-bar] button').filter({ has: page.locator('svg.lucide-shield') })
      await control.click()
      await page.getByRole('menu').getByText(text.busy, { exact: false }).waitFor()
      assert.equal(await page.getByRole('menuitemradio', { name: text.read, exact: true }).getAttribute('data-disabled'), '')
      await page.keyboard.press('Escape')
      const rejected = await fetch(`${server.origin}/api/v1${resource}`, { method: 'PATCH', headers: { authorization: `Bearer ${token}`, 'x-ternilo-tenant': tenantId, 'content-type': 'application/json' }, body: JSON.stringify({ permissions: 'read_only' }), signal: AbortSignal.timeout(3000) })
      assert.equal(rejected.status, 409)
      const history = await request(`${resource}/history?limit=20`); assert.ok(history.events.length)
      await page.reload(); await page.locator('[data-execution-phase="running"]').waitFor()
      proxy.available(false)
      await page.getByText(text.offline, { exact: true }).waitFor()
      assert.equal(await page.locator('[data-execution-phase="running"]').count(), 0)
      assert.equal(await page.locator('[data-reasoning-row][data-state="running"]').count(), 0)
      const unknown = await page.locator('[data-execution-phase="unknown"]').innerText()
      await new Promise(resolve => setTimeout(resolve, 1100))
      assert.equal(await page.locator('[data-execution-phase="unknown"]').innerText(), unknown)
      if (process.env.TERNILO_E2E_ARTIFACT_DIR) { await mkdir(process.env.TERNILO_E2E_ARTIFACT_DIR, { recursive: true }); await page.screenshot({ path: path.join(process.env.TERNILO_E2E_ARTIFACT_DIR, `offline-${locale}.png`) }) }
      proxy.available(true)
      await page.locator('[data-execution-phase="running"]').waitFor({ timeout: 30000 })
      model.release()
      await until(() => request(`${resource}/queue`), queue => !queue.active_run_id, 'idle parent')
      await page.locator('[data-execution-phase]').waitFor({ state: 'detached' })
      await control.click(); await page.getByRole('menuitemradio', { name: text.read, exact: true }).click()
      await until(() => request('/state'), state => state.sessions.find(item => item.identity.session_id === parentId)?.permissions === 'read_only', 'saved idle permissions')
      await control.click(); await page.getByRole('menuitemradio', { name: text.full, exact: true }).click()
      await page.getByRole('dialog').getByRole('button', { name: text.confirm, exact: true }).click()
      await page.getByRole('dialog').waitFor({ state: 'detached' })
    }
    await page.evaluate(() => localStorage.setItem('ternilo.locale', 'zh')); await page.reload()
    await request(`${resource}/queue`, { body: { content: { kind: 'prompt', input: 'delegate' } } })
    await page.locator('[data-subagent-activity]').getByText('正在启动子 Agent…', { exact: true }).waitFor()
    await page.getByRole('button', { name: '允许一次', exact: true }).click()
    await page.locator('[data-subagent-activity]').getByRole('button', { name: 'Review fixture', exact: true }).waitFor()
    const withChild = await until(() => request('/state'), state => state.sessions.some(item => item.parent_session_id === parentId), 'mapped child')
    const child = withChild.sessions.find(item => item.parent_session_id === parentId)
    const events = await request(`${resource}/events`)
    assert.equal(events.findLast(event => event.type === 'subagent_updated').subagent.session_id, child.identity.session_id)
    await page.getByRole('button', { name: labels.zh.team, exact: true }).first().click()
    await page.locator('[data-agent-team-panel]').getByRole('button', { name: labels.zh.open, exact: true }).click()
    await until(() => page.locator('[data-sidebar-session-row][aria-selected="true"]').getAttribute('data-session-id'), id => id === child.identity.session_id, 'opened Team child')
    await page.locator(`[data-sidebar-session-row][data-session-id="${parentId}"]`).click()
    await page.locator('[data-subagent-activity]').getByRole('button', { name: 'Review fixture', exact: true }).click()
    await page.locator(`[data-sidebar-session-row][data-session-id="${parentId}"]`).click()
    model.release()
    await until(() => request(`${resource}/queue`), queue => !queue.active_run_id, 'completed delegation')
    await request(`${resource}/queue`, { body: { content: { kind: 'prompt', input: 'delivery' } } })
    await until(async () => {
      const pending = await request(`/questions?session_id=${parentId}`)
      for (const item of pending) {
        assert.ok(['todo_write', 'write_file'].includes(item.question.tool_approval?.tool_name), JSON.stringify(item.question))
        await request(`/questions/${item.question.id}/answer?session_id=${parentId}`, { body: { selected: ['Allow once'], custom: null } })
      }
      return request(`${resource}/events`)
    }, events => events.some(event => event.type === 'assistant_message' && event.response.content.startsWith('Delivery complete.')), 'completed delivery')
    await page.getByText('Delivery complete.', { exact: true }).waitFor()
    const image = page.locator('img[alt="Fixture map"]')
    await image.waitFor()
    await until(() => image.evaluate(img => img.complete && img.naturalWidth > 0), Boolean, 'actual rendered workspace image')
    await page.locator('[data-projection-dock]').waitFor({ state: 'detached' })
    assert.equal((await request(`${resource}/projection`)).values.todos.items[0].status, 'completed')
    for (const locale of Object.keys(labels)) {
      await page.evaluate(locale => localStorage.setItem('ternilo.locale', locale), locale)
      await page.setViewportSize(locale === 'ko' ? { width: 390, height: 844 } : { width: 1366, height: 900 })
      await page.reload()
      await page.locator('img[alt="Fixture map"]').waitFor()
      await until(() => page.locator('img[alt="Fixture map"]').evaluate(img => img.complete && img.naturalWidth > 0), Boolean, 'rendered image after reload')
      await page.locator('img[alt="Fixture map"]').scrollIntoViewIfNeeded()
      await page.locator('img[alt="Fixture map"]').click()
      await page.getByRole('dialog').filter({ has: page.locator('img[alt="Fixture map"]') }).waitFor()
      await page.keyboard.press('Escape')
      if (process.env.TERNILO_E2E_ARTIFACT_DIR) await page.screenshot({ path: path.join(process.env.TERNILO_E2E_ARTIFACT_DIR, `delivery-${locale}.png`) })
    }
    await page.evaluate(() => localStorage.setItem('ternilo.locale', 'zh'))
    await page.setViewportSize({ width: 1366, height: 900 })
    await page.goto(`${server.origin}/files?session_id=${encodeURIComponent(parentId)}&kind=generated`)
    await page.locator('[data-file-results] [data-file-id]').first().waitFor()
    const fileRows = page.locator('[data-file-results] [data-file-id]')
    assert.equal(await fileRows.count(), 1)
    assert.equal(await fileRows.first().getByRole('button').first().innerText(), 'report.txt')
    const versions = page.locator('[data-file-versions]'); await versions.locator('summary').click()
    assert.equal(await versions.locator('li').count(), 2)
    await versions.locator('li').last().locator('button').first().click()
    await page.locator('[data-file-preview]').getByText('version one', { exact: true }).waitFor()
    await page.getByRole('button', { name: '关闭预览', exact: true }).click()
    const download = page.waitForEvent('download')
    await versions.locator('li').last().locator('button').last().click()
    const saved = await download
    assert.equal(await readFile(await saved.path(), 'utf8'), 'version one')
    if (process.env.TERNILO_E2E_ARTIFACT_DIR) await page.screenshot({ path: path.join(process.env.TERNILO_E2E_ARTIFACT_DIR, 'file-versions.png') })
    assert.deepEqual(errors, [])
  } catch (error) {
    for (const process of processes) console.error(process.diagnostics().slice(-5000))
    if (page && !page.isClosed()) {
      console.error((await page.locator('body').innerText()).slice(-7000))
      if (await page.locator('[data-role="assistant"]').count()) console.error('Assistant DOM:', await page.locator('[data-role="assistant"]').last().evaluate(element => element.outerHTML))
      console.error('Browser errors:', errors)
    }
    throw error
  } finally {
    model?.release(); await browser?.close(); await proxy?.close()
    for (const process of processes.reverse()) await stopProcess(process)
    await model?.close(); await rm(directory, { recursive: true, force: true })
  }
})
