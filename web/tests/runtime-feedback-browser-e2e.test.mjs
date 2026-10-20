import assert from 'node:assert/strict'
import { createServer } from 'node:http'
import { connect, createServer as tcpServer } from 'node:net'
import { chmod, mkdtemp, mkdir, readFile, rm, symlink, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import { pathToFileURL } from 'node:url'
import test from 'node:test'
import { chromium } from 'playwright'
import { execute, freePort, initializeServer, repository, serverRequest, startProcess, stopProcess, waitForHttp } from './platform-e2e-fixture.mjs'
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
  const pending = new Set(), requests = []
  const server = createServer(async (request, response) => {
    const chunks = []; for await (const chunk of request) chunks.push(chunk)
    const body = JSON.parse(Buffer.concat(chunks).toString())
    requests.push(body)
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
    if (prompt.includes('cache-history')) {
      for (let index = 0; index < 1200; index += 1) send({ content: `Cache segment ${index}. ` })
      return finish({ content: 'History cache proof complete.' })
    }
    if (prompt.includes('runtime-native-search')) {
      if (!tool('native-glob')) return call('native-glob', 'glob_files', { pattern: '*.txt' })
      if (!tool('native-search')) return call('native-search', 'search_files', { pattern: 'readiness-needle', file_glob: '*.txt' })
      return finish({ content: 'Native search works without rg.' })
    }
    if (prompt.includes('runtime-sandbox-denial')) {
      if (!tool('sandbox-denied')) return call('sandbox-denied', 'shell', { command: 'printf forbidden > should-not-exist.txt' })
      return finish({ content: 'Sandbox refusal recorded.' })
    }
    if (prompt.includes('runtime-shell-failure')) {
      if (!tool('shell-failed')) return call('shell-failed', 'shell', { command: "printf 'MethodInvocationNotSupportedInConstrainedLanguage\\nEmptyPipeElement\\n' >&2; exit 7" })
      return finish({ content: 'Shell failure recorded.' })
    }
    if (prompt.includes('windows-images')) return finish({ content: 'Windows image references.\n\n![Windows path](C:/Users/Move/Desktop/image%20space.png)\n\n![Windows file URL](file:///C:/Users/Move/Desktop/image%20space.png)\n\n![Outside workspace](file:///C:/Users/Move/Documents/secret.png)' })
    if (prompt.includes('delegate') && !tool('spawn')) return call('spawn', 'spawn_agent', { task: 'Inspect fixture evidence', label: 'Review fixture', role: 'investigator', background: false })
    const approvalLocale = Object.keys(approvalCopy).find(locale => prompt.includes(`approval-${locale}`))
    if (approvalLocale && !tool('shell-approval')) {
      const { summary } = approvalCopy[approvalLocale]
      const approval = approvalLocale === 'en'
        ? { action: 'Create one test file in this workspace', reason: 'to verify the approval flow.', impact: 'This runs only after you approve.' }
        : summary
      return call('shell-approval', 'shell', { command: `printf approved > approval-${approvalLocale}.txt`, full_access: true, approval })
    }
    if (prompt.includes('delivery')) {
      if (!tool('todo-start')) return call('todo-start', 'todo_write', { items: [{ step: 'Deliver files', status: 'in_progress' }] })
      if (!tool('version-one')) return call('version-one', 'write_file', { path: 'report.txt', content: 'version one' })
      if (!tool('version-two')) return call('version-two', 'write_file', { path: 'report.txt', content: 'version two' })
      if (!tool('todo-done')) return call('todo-done', 'todo_write', { items: [{ step: 'Deliver files', status: 'completed' }] })
      return finish({ content: `Delivery complete.\n\n![Fixture map](${folder}/image.png)\n\n![File URL map](${pathToFileURL(path.join(folder, 'image.png')).href})` })
    }
    finish({ content: 'Parent finished.' })
  })
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve))
  return { url: `http://127.0.0.1:${server.address().port}/v1`, requests, release: () => { for (const release of [...pending]) release() },
    close: async () => { server.closeAllConnections(); await new Promise(resolve => server.close(resolve)) } }
}

async function verifyWindowsImages(page, request, resource, workspaceId) {
  // Present a Windows host root; image bytes still come from the real Node's workspace API.
  const location = `**/workspaces/${workspaceId}/location`
  await page.route(location, async route => {
    const response = await route.fetch()
    assert.equal(response.status(), 200)
    await route.fulfill({ response, json: { ...await response.json(), path: '\\\\?\\C:\\Users\\Move\\Desktop' } })
  })
  const reads = []
  const record = outgoing => {
    if (new URL(outgoing.url()).pathname.endsWith(`${resource}/workspace`) && outgoing.method() === 'POST') reads.push(outgoing.postDataJSON())
  }
  page.on('request', record)
  try {
    await page.reload()
    await request(`${resource}/queue`, { body: { content: { kind: 'prompt', input: 'windows-images' } } })
    await until(() => request(`${resource}/events`), events => events.some(event => event.type === 'assistant_message' && event.response.content.startsWith('Windows image references.')), 'Windows image answer')
    const outside = {
      zh: '图片路径不在当前工作区内', en: 'Image path is outside the current workspace.', ko: '이미지 경로가 현재 작업 공간 밖에 있습니다.',
    }
    for (const [locale, message] of Object.entries(outside)) {
      await page.evaluate(locale => localStorage.setItem('ternilo.locale', locale), locale)
      await page.setViewportSize(locale === 'ko' ? { width: 320, height: 740 } : { width: 1366, height: 900 })
      await page.reload()
      for (const alt of ['Windows path', 'Windows file URL']) {
        const image = page.locator(`img[alt="${alt}"]`)
        await image.waitFor()
        await until(() => image.evaluate(img => img.complete && img.naturalWidth === 192), Boolean, 'Windows reference loads actual image bytes')
      }
      const blocked = page.getByRole('status').filter({ hasText: 'Outside workspace' })
      await blocked.filter({ hasText: message }).waitFor()
      assert.equal(await page.locator('img[alt="Outside workspace"], img[src^="file:"]').count(), 0)
      const image = page.locator('img[alt="Windows file URL"]')
      await image.scrollIntoViewIfNeeded(); await image.click()
      await page.getByRole('dialog').filter({ has: page.locator('img[alt="Windows file URL"]') }).waitFor()
      await page.keyboard.press('Escape')
      await blocked.scrollIntoViewIfNeeded()
      assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true)
      if (process.env.TERNILO_E2E_ARTIFACT_DIR) await page.screenshot({ path: path.join(process.env.TERNILO_E2E_ARTIFACT_DIR, `windows-images-${locale}.png`) })
    }
    assert.ok(reads.some(read => read.kind === 'read' && read.path === 'image space.png'))
    assert.ok(reads.every(read => read.kind !== 'read' || (read.path !== '../Documents/secret.png' && !read.path.includes('secret.png'))))
  } finally {
    page.off('request', record)
    await page.unroute(location)
  }
}

async function verifyRuntimeReadiness({ page, request, local, resource, localSession, folder, runtimePath, bwrap }) {
  const localResource = `/sessions/${localSession.identity.session_id}`
  const localInfo = await local(`${localResource}/workspace`)
  const remoteInfo = await request(`${resource}/workspace`)
  assert.equal(localInfo.sandbox.status, 'unusable')
  assert.equal(remoteInfo.sandbox.status, 'unusable')
  assert.match(remoteInfo.sandbox.detail, /fixture sandbox unavailable/)
  await local(localResource, { method: 'PATCH', body: { permissions: 'workspace_write' } })
  for (const [input, answer] of [
    ['runtime-native-search', 'Native search works without rg.'],
    ['runtime-sandbox-denial', 'Sandbox refusal recorded.'],
  ]) {
    await request(`${resource}/queue`, { body: { content: { kind: 'prompt', input } } })
    await until(() => request(`${resource}/events`), events => events.some(event => event.type === 'assistant_message' && event.response.content === answer), answer)
  }
  const events = await request(`${resource}/events`)
  for (const id of ['native-glob', 'native-search']) {
    const result = events.find(event => event.type === 'tool_call_finished' && event.call_id === id)
    assert.ok(result, `actual ${id} result`)
    assert.equal(result.output.is_error, false)
    assert.match(result.output.content, /search-proof.txt/)
  }
  const denied = events.find(event => event.type === 'tool_call_finished' && event.call_id === 'sandbox-denied')
  assert.equal(denied.output.is_error, true)
  assert.equal(JSON.parse(denied.output.content).exit_code, 42)
  await assert.rejects(readFile(path.join(folder, 'should-not-exist.txt')), { code: 'ENOENT' })
  for (const [locale, title, retry] of [
    ['zh', '命令沙箱不可用', '重新检查'],
    ['en', 'Command sandbox unavailable', 'Check again'],
    ['ko', '명령 샌드박스를 사용할 수 없음', '다시 확인'],
  ]) {
    await page.evaluate(locale => localStorage.setItem('ternilo.locale', locale), locale)
    await page.setViewportSize(locale === 'ko' ? { width: 320, height: 740 } : { width: 1366, height: 900 })
    await page.reload()
    const warning = page.locator('[data-sandbox-status="unusable"]')
    await warning.getByText(title, { exact: true }).waitFor()
    await warning.locator('summary').click()
    await warning.getByText(/fixture sandbox unavailable/).waitFor()
    const button = warning.getByRole('button', { name: retry, exact: true })
    await button.scrollIntoViewIfNeeded()
    assert.equal(await button.isEnabled(), true)
    assert.equal(await warning.evaluate(el => el.scrollWidth > el.clientWidth + 1), false)
    if (process.env.TERNILO_E2E_ARTIFACT_DIR) {
      await mkdir(process.env.TERNILO_E2E_ARTIFACT_DIR, { recursive: true })
      await page.screenshot({ path: path.join(process.env.TERNILO_E2E_ARTIFACT_DIR, `sandbox-${locale}.png`) })
    }
  }
  await rm(path.join(runtimePath, 'bwrap')); await symlink(bwrap, path.join(runtimePath, 'bwrap'))
  await page.locator('[data-sandbox-status]').getByRole('button', { name: '다시 확인', exact: true }).click()
  await page.locator('[data-sandbox-status]').waitFor({ state: 'detached' })
  assert.equal((await request(`${resource}/workspace`)).sandbox.status, 'ready')
  await request(`${resource}/queue`, { body: { content: { kind: 'prompt', input: 'runtime-shell-failure' } } })
  await until(() => request(`${resource}/events`), events => events.some(event => event.type === 'assistant_message' && event.response.content === 'Shell failure recorded.'), 'failed shell output')
  for (const [locale, hint] of [['zh', '受限语言模式'], ['en', 'ConstrainedLanguage'], ['ko', '제한된 언어 모드']]) {
    await page.evaluate(locale => localStorage.setItem('ternilo.locale', locale), locale); await page.reload()
    const tool = page.locator('[data-tool-call-id="shell-failed"]')
    await tool.locator('[data-tool-call-toggle]').click()
    assert.equal(await tool.getAttribute('data-state'), 'error')
    await tool.locator('[data-shell-feedback]').getByText(hint, { exact: false }).waitFor()
    assert.match(await tool.locator('[aria-label="stderr"]').textContent(), /EmptyPipeElement/)
    if (process.env.TERNILO_E2E_ARTIFACT_DIR) await page.screenshot({ path: path.join(process.env.TERNILO_E2E_ARTIFACT_DIR, `shell-error-${locale}.png`) })
  }
  await local(localResource, { method: 'PATCH', body: { permissions: 'full_access' } })
  await page.evaluate(() => localStorage.setItem('ternilo.locale', 'zh'))
  await page.setViewportSize({ width: 1366, height: 900 }); await page.reload()
}

const labels = {
  zh: { permission: '会话权限', busy: '当前轮次正在运行', read: '只读', full: '完整访问', confirm: '允许完整访问', team: '打开 Agent Team', open: '打开独立会话', offline: '电脑离线，运行状态待确认', parent: '上级目录' },
  en: { permission: 'Session permission', busy: 'Wait for the current turn', read: 'Read only', full: 'Full access', confirm: 'Allow full access', team: 'Open Agent Team', open: 'Open independent session', offline: 'Computer offline. Run status is unconfirmed.', parent: 'Parent folder' },
  ko: { permission: '세션 권한', busy: '현재 실행이 끝나거나', read: '읽기 전용', full: '전체 접근', confirm: '전체 접근 허용', team: '에이전트 팀 열기', open: '독립 세션 열기', offline: '컴퓨터가 오프라인입니다. 실행 상태를 확인할 수 없습니다.', parent: '상위 폴더' },
}

const approvalCopy = {
  zh: { summary: '为验证授权流程，将在当前工作区创建一个测试文件；只有你同意后才会执行。', allow: '允许一次', deny: '拒绝', command: '查看完整命令' },
  en: { summary: 'Create one test file in this workspace to verify the approval flow. This runs only after you approve.', allow: 'Allow once', deny: 'Deny', command: 'View full command' },
  ko: { summary: '승인 절차를 확인하기 위해 현재 작업 공간에 테스트 파일 하나를 만듭니다. 승인한 뒤에만 실행됩니다.', allow: '한 번 허용', deny: '거부', command: '전체 명령 보기' },
}

test('real Node permissions, child navigation, offline feedback and delivered files', { timeout: 300_000 }, async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-runtime-feedback-'))
  const processes = [], errors = []
  let browser, model, proxy, page
  try {
    const folder = path.join(directory, 'workspace'); await mkdir(folder)
    const home = path.join(directory, 'home'); await mkdir(home)
    const imageBytes = await readFile(path.join(repository, 'web/public/assets/icon-192.png'))
    await writeFile(path.join(folder, 'image.png'), imageBytes)
    await writeFile(path.join(folder, 'image space.png'), imageBytes)
    model = await slowModel(folder)
    const server = await initializeServer({ directory: path.join(directory, 'server'), origin: `http://127.0.0.1:${await freePort()}` }); processes.push(server)
    const token = server.owner.session.access_token, tenantId = server.owner.session.personal_tenant_id
    const request = (url, options = {}) => serverRequest(server.origin, url, { token, tenantId, ...options })
    const enrolled = await request(`/tenants/${tenantId}/my-computer-enrollments`, { body: { name: 'Feedback node', project_id: null, ttl_seconds: 600 } })
    const credential = await request('/enrollments/consume', { body: { token: enrolled.enrollment.token } })
    proxy = await gatewayProxy(server.origin)
    const origin = `http://127.0.0.1:${await freePort()}`
    const runtimePath = path.join(directory, 'runtime-path'); await mkdir(runtimePath)
    const bwrap = (await execute('/bin/sh', ['-c', 'command -v bwrap'])).stdout.trim()
    assert.ok(path.isAbsolute(bwrap), 'sandbox recovery uses the installed enforcing backend')
    await symlink('/bin/bash', path.join(runtimePath, 'bash'))
    await writeFile(path.join(runtimePath, 'bwrap'), '#!/bin/sh\nprintf "fixture sandbox unavailable" >&2\nexit 42\n')
    await chmod(path.join(runtimePath, 'bwrap'), 0o755)
    await writeFile(path.join(folder, 'search-proof.txt'), '中文 readiness-needle\n')
    const node = startProcess(process.env.TERNILO_E2E_NODE_BINARY ?? path.join(repository, 'target/debug/ternilo'), ['serve', '--listen', new URL(origin).host, '--data-dir', path.join(directory, 'node'), '--node-id', enrolled.enrollment.executor_id, '--gateway-url', proxy.url, '--allow-insecure-gateway'], { PATH: runtimePath, HOME: home, TERNILO_LOCAL_TOKEN: credential.credential.token, XDG_STATE_HOME: path.join(directory, 'state') }); processes.push(node)
    await waitForHttp(origin, node)
    for (const address of [server.origin, origin]) {
      const served = await fetch(`${address}/assets/app.js`)
      assert.equal(served.status, 200)
      assert.ok(Buffer.from(await served.arrayBuffer()).equals(await readFile(path.join(repository, 'web/dist/assets/app.js'))), `the fixture at ${address} serves the current UI`)
    }
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
    let holdMetadata = false, interruptMetadata
    let historyGate = null, cacheBackfillGate = null, cacheHeadReads = 0
    await page.routeWebSocket('**/api/v1/live', socket => {
      const upstream = socket.connectToServer()
      socket.onMessage(message => upstream.send(message))
      upstream.onMessage(message => {
        const frame = JSON.parse(String(message))
        if (holdMetadata && frame.type === 'session_metadata' && frame.session_id === parentId) {
          holdMetadata = false
          interruptMetadata = () => {
            // Reproduce the known initial-metadata failure after history has arrived.
            socket.send(JSON.stringify({ type: 'error', subscription_id: frame.subscription_id,
              code: 'unavailable', message: 'selected Ternilo node is offline' }))
            proxy.available(false)
          }
        } else socket.send(message)
      })
    })
    await page.route(`**/sessions/${parentId}/history?*`, async route => {
      if (historyGate) await historyGate
      if (new URL(route.request().url()).searchParams.has('before_seq')) {
        if (cacheBackfillGate) await cacheBackfillGate
      } else cacheHeadReads += 1
      await route.continue()
    })
    page.on('pageerror', error => errors.push(error.message))
    page.on('console', message => { if (message.type() === 'error') errors.push(message.text()) })
    page.on('response', response => { if (response.status() >= 400) errors.push(`${response.status()} ${new URL(response.url()).pathname}`) })
    await page.goto(server.origin)
    await page.getByLabel('用户名', { exact: true }).fill(server.owner.username)
    await page.getByLabel('密码', { exact: true }).fill(server.owner.password)
    await page.getByRole('button', { name: '登录', exact: true }).click()
    await page.locator(`[data-sidebar-session-row][data-session-id="${parentId}"]`).click()
    await verifyRuntimeReadiness({ page, request, local, resource, localSession, folder, runtimePath, bwrap })
    await page.getByRole('button', { name: '添加工作区', exact: true }).click()
    const placement = page.getByRole('dialog', { name: '打开工作区', exact: true })
    await placement.getByRole('button', { name: '选择文件夹', exact: true }).click()
    const picker = page.getByRole('dialog', { name: '选择 Feedback node 上的工作文件夹', exact: true })
    await until(() => picker.locator('[data-directory-path]').textContent(), value => value === home, 'visible home path')
    await picker.getByRole('button', { name: '上级目录', exact: true }).click()
    await until(() => picker.locator('[data-directory-path]').textContent(), value => value === directory, 'navigate above home')
    await page.keyboard.press('Escape'); await page.keyboard.press('Escape')
    for (const [locale, text] of Object.entries(labels)) {
      await page.setViewportSize(locale === 'ko' ? { width: 390, height: 844 } : { width: 1366, height: 900 })
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
      const catalogs = ['commands', 'skills'].map(kind => page.waitForResponse(response =>
        new URL(response.url()).pathname === `/api/v1/sessions/${parentId}/${kind}` && response.status() === 200))
      holdMetadata = true; interruptMetadata = undefined
      await page.reload()
      for (const response of await Promise.all(catalogs)) await response.finished()
      await page.locator('[data-session-state="ready"]').waitFor()
      await page.locator('[data-history-state="backfilling"]').waitFor({ state: 'detached' })
      await until(async () => Boolean(interruptMetadata), Boolean, 'held initial metadata')
      interruptMetadata()
      await page.locator('[data-history-state="error"]').waitFor()
      await page.locator('[data-node-offline]').waitFor()
      const retainedPrompt = page.locator('[data-role="user"]').getByText(`hold-${locale}`, { exact: true })
      assert.equal(await retainedPrompt.count(), 1, 'a metadata error must not hide accepted history')
      assert.equal(await page.locator('[data-execution-phase="running"]').count(), 0)
      let resumeHistory
      historyGate = new Promise(resolve => { resumeHistory = resolve })
      if (process.env.TERNILO_E2E_ARTIFACT_DIR) {
        await mkdir(process.env.TERNILO_E2E_ARTIFACT_DIR, { recursive: true })
        await page.screenshot({ path: path.join(process.env.TERNILO_E2E_ARTIFACT_DIR, `interrupted-history-${locale}.png`) })
        await page.locator('[data-history-state="error"]').screenshot({ path: path.join(process.env.TERNILO_E2E_ARTIFACT_DIR, `history-warning-${locale}.png`) })
      }
      proxy.available(true)
      await page.locator('[data-history-state="backfilling"]').waitFor()
      assert.equal(await retainedPrompt.count(), 1, 'automatic retry keeps the readable history in place')
      resumeHistory(); historyGate = null
      await page.locator('[data-history-state="error"], [data-history-state="backfilling"]').waitFor({ state: 'detached' })
      await page.locator('[data-execution-phase="running"]').waitFor()
      assert.equal(await retainedPrompt.count(), 1, 'fresh history replaces the old baseline without duplicates')
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
    await request(resource, { method: 'PATCH', body: { permissions: 'workspace_write' } })
    for (const [locale, copy] of Object.entries(approvalCopy)) {
      await page.evaluate(locale => localStorage.setItem('ternilo.locale', locale), locale)
      await page.setViewportSize(locale === 'ko' ? { width: 390, height: 844 } : { width: 1366, height: 900 })
      await page.reload()
      const card = page.locator('[data-tool-approval]')
      const enqueue = () => request(`${resource}/queue`, { body: { content: { kind: 'prompt', input: `approval-${locale}` } } })
      await enqueue()
      await card.getByText(copy.summary, { exact: true }).waitFor()
      assert.equal(await card.locator('[data-approval-explanation] dl').count(), 0)
      await assert.rejects(readFile(path.join(folder, `approval-${locale}.txt`)), { code: 'ENOENT' })
      const history = page.locator('[data-question-kind="tool-approval"][data-state="pending"]')
      assert.equal(await history.getByText(copy.summary, { exact: true }).count(), 1)
      const command = card.locator('[data-approval-command]')
      assert.equal(await command.locator('pre').isVisible(), false)
      await command.locator('summary').click()
      assert.equal(await command.locator('code').textContent(), `printf approved > approval-${locale}.txt`)
      await command.locator('summary').click()
      assert.ok(await card.getByRole('button', { name: copy.allow, exact: true }).isVisible())
      assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true)
      if (process.env.TERNILO_E2E_ARTIFACT_DIR) await page.screenshot({ path: path.join(process.env.TERNILO_E2E_ARTIFACT_DIR, `approval-${locale}.png`) })
      if (locale === 'zh') {
        await card.getByRole('button', { name: copy.deny, exact: true }).click()
        await until(() => request(`${resource}/queue`), queue => !queue.active_run_id, 'denied shell finishes')
        await assert.rejects(readFile(path.join(folder, `approval-${locale}.txt`)), { code: 'ENOENT' })
        await enqueue()
        await card.getByText(copy.summary, { exact: true }).waitFor()
      }
      await card.getByRole('button', { name: copy.allow, exact: true }).click()
      await until(() => request(`${resource}/queue`), queue => !queue.active_run_id, 'approved shell finishes')
      assert.equal(await readFile(path.join(folder, `approval-${locale}.txt`), 'utf8'), 'approved')
    }
    await request(resource, { method: 'PATCH', body: { permissions: 'full_access' } })
    await page.setViewportSize({ width: 1366, height: 900 })
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
    const fileUrlImage = page.locator('img[alt="File URL map"]')
    await fileUrlImage.waitFor()
    await until(() => fileUrlImage.evaluate(img => img.complete && img.naturalWidth > 0), Boolean, 'Node-redacted file URL renders an image')
    const delivered = model.requests.find(body => body.stream && body.messages.some(message => message.role === 'user' && String(message.content).includes('delivery')))
    assert.ok(delivered)
    const system = delivered.messages.filter(message => message.role === 'system').map(message => String(message.content)).join('\n')
    assert.match(system, /Markdown image.*!\[Image\]\(output\/image\.png\)/s)
    assert.match(system, /path must stay inside this workspace/)
    assert.match(system, /Displaying an image reference does not let you inspect its pixels/)
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
    await verifyWindowsImages(page, request, resource, state.sessions[0].workspace_id)
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
    // Switching away during a slow earlier-page fetch must retain the already readable tail.
    await page.goto(server.origin)
    const second = await request('/sessions', { body: { workspace_id: state.sessions[0].workspace_id } })
    await request(`${resource}/queue`, { body: { content: { kind: 'prompt', input: 'cache-history' } } })
    await until(() => request(`${resource}/queue`), queue => !queue.active_run_id, 'cache history generated')
    let releaseBackfill
    cacheBackfillGate = new Promise(resolve => { releaseBackfill = resolve })
    await page.reload()
    await page.getByText(/History cache proof complete/).last().waitFor()
    await page.locator('[data-history-state="backfilling"]').waitFor()
    const headReads = cacheHeadReads
    await page.locator(`[data-sidebar-session-row][data-session-id="${second.identity.session_id}"]`).click()
    await page.locator('[data-session-state="empty"]').waitFor()
    await page.locator(`[data-sidebar-session-row][data-session-id="${parentId}"]`).click()
    await page.getByText(/History cache proof complete/).last().waitFor({ timeout: 2000 })
    assert.equal(await page.locator('[data-session-state="loading"]').count(), 0)
    assert.equal(cacheHeadReads, headReads, 'returning to the partially loaded session does not reload its newest history page')
    releaseBackfill(); cacheBackfillGate = null
    await page.locator('[data-history-state="backfilling"]').waitFor({ state: 'detached' })
    if (process.env.TERNILO_E2E_ARTIFACT_DIR) await page.screenshot({ path: path.join(process.env.TERNILO_E2E_ARTIFACT_DIR, 'history-cache-return.png') })
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
