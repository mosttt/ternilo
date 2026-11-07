import assert from 'node:assert/strict'
import { createServer } from 'node:http'
import { mkdir, mkdtemp, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { chromium } from 'playwright'
import { localApi, until } from './model-device-fixture.mjs'
import { freePort, initializeServer, repository, serverRequest, startProcess, stopProcess, waitForHttp } from './platform-e2e-fixture.mjs'

test('Server account subagents resolve web settings and keep approval bound to the parent on mobile', { timeout: 90_000 }, async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-subagent-web-'))
  const artifacts = process.env.TERNILO_E2E_ARTIFACT_DIR ?? directory
  await mkdir(artifacts, { recursive: true })
  const processes = [], errors = [], modelCalls = [], webCalls = []
  let browser, page
  const upstream = createServer(async (request, response) => {
    const chunks = []; for await (const chunk of request) chunks.push(chunk)
    const body = JSON.parse(Buffer.concat(chunks))
    if (request.url === '/v1/tools/search') {
      webCalls.push(body)
      assert.equal(request.headers.authorization, 'Bearer fixture-search-secret')
      response.writeHead(200, { 'content-type': 'application/json' }).end(JSON.stringify({ results: [{ title: 'Child evidence', url: 'https://example.com/evidence', snippet: 'Verified by the child', summary: 'Child search completed' }] }))
      return
    }
    assert.equal(request.url, '/v1/chat/completions')
    modelCalls.push(body)
    const title = body.messages.some(message => message.role === 'system' && message.content.startsWith('You name software-agent conversations'))
    const userIndex = body.messages.findLastIndex(message => message.role === 'user')
    const child = !title && body.messages[userIndex].content.includes('CHILD_WEB_TASK')
    const done = body.messages.slice(userIndex + 1).some(message => message.role === 'tool')
    const tool = title || done ? null : child ? 'web_search' : 'spawn_agent'
    if (tool) assert.ok(body.tools.some(item => item.function?.name === tool), `missing ${tool}`)
    const args = child ? { query: 'child evidence', limit: 1 } : { task: 'CHILD_WEB_TASK: search for evidence', label: '网页验证', background: false }
    const delta = tool ? { role: 'assistant', tool_calls: [{ index: 0, id: `call-${modelCalls.length}`, type: 'function', function: { name: tool, arguments: JSON.stringify(args) } }] } : { role: 'assistant', content: title ? '子任务验证' : child ? 'Child web evidence ready.' : 'Parent delegation finished.' }
    const frame = value => `data: ${JSON.stringify({ id: 'fixture', model: 'fixture', object: 'chat.completion.chunk', ...value })}\n\n`
    response.writeHead(200, { 'content-type': 'text/event-stream' }).end(frame({ choices: [{ index: 0, delta, finish_reason: null }] }) + frame({ choices: [{ index: 0, delta: {}, finish_reason: tool ? 'tool_calls' : 'stop' }] }) + frame({ choices: [], usage: { prompt_tokens: 20, completion_tokens: 10, total_tokens: 30 } }) + 'data: [DONE]\n\n')
  })
  await new Promise(resolve => upstream.listen(0, '127.0.0.1', resolve))
  try {
    const environment = { ...Object.fromEntries(Object.keys(process.env).filter(key => key.startsWith('TERNILO_')).map(key => [key, undefined])), XDG_STATE_HOME: path.join(directory, 'state') }
    const origin = `http://127.0.0.1:${await freePort()}`
    const server = await initializeServer({ directory: path.join(directory, 'server'), origin, environment }); processes.push(server)
    const tenant = server.owner.session.personal_tenant_id
    const request = (url, options = {}) => serverRequest(origin, url, { token: server.owner.session.access_token, tenantId: tenant, ...options })
    const project = (await request('/projects')).projects[0]
    const { enrollment } = await request(`/tenants/${tenant}/my-computer-enrollments`, { body: { name: 'Agent computer', project_id: project.project_id, ttl_seconds: 600 } })
    const { credential } = await request('/enrollments/consume', { body: { token: enrollment.token } })
    const nodeOrigin = `http://127.0.0.1:${await freePort()}`
    const node = startProcess(process.env.TERNILO_E2E_NODE_BINARY ?? path.join(repository, 'target/debug/ternilo'), ['serve', '--data-dir', path.join(directory, 'node'), '--listen', new URL(nodeOrigin).host, '--node-id', enrollment.executor_id, '--gateway-url', `${origin.replace('http:', 'ws:')}/api/v1/executors/connect`, '--allow-insecure-gateway'], { ...environment, TERNILO_LOCAL_TOKEN: credential.token })
    processes.push(node); await waitForHttp(nodeOrigin, node)
    const local = await localApi(nodeOrigin)
    await until(() => request('/execution-targets'), value => value.executors.some(item => item.executor_id === enrollment.executor_id && item.connected), 'connected computer')
    const base = `http://127.0.0.1:${upstream.address().port}`
    await request('/credentials', { body: { name: 'SEARCH_KEY', value: 'fixture-search-secret' } })
    await request('/providers', { body: { id: 'fixture', display_name: 'Agent model', base_url: `${base}/v1`, protocol: 'openai-chat-completions', api_key_ref: null, defaults: { context_window: 32000, max_output_tokens: 2048 }, models: [{ id: 'fixture', settings: { mode: 'inherit' } }], web_access: { search: { mode: 'deepseek', base_url: base, api_key_ref: 'SEARCH_KEY' } }, timeout_ms: 15000, max_attempts: 1, retry_base_delay_ms: 25 } })
    const folder = path.join(directory, 'workspace'); await mkdir(folder)
    const workspace = (await request('/workspaces', { body: { project_id: project.project_id, name: 'Agent workspace', placement: 'local_node', executor_id: enrollment.executor_id, path: folder } })).workspace
    const session = await request('/sessions', { body: { workspace_id: workspace.workspace_id } })
    const resource = `/sessions/${session.identity.session_id}`
    await request(resource, { method: 'PATCH', body: { model: { provider: 'account_provider', owner_user_id: server.owner.session.user.user_id, provider_id: 'fixture', model: 'fixture' }, permissions: 'workspace_write' } })
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
    page = await browser.newPage({ locale: 'zh-CN', viewport: { width: 390, height: 844 }, isMobile: true, hasTouch: true, serviceWorkers: 'block' })
    page.on('pageerror', error => errors.push(error.message)); page.on('console', message => { if (message.type() === 'error') errors.push(message.text()) }); page.on('response', response => { if (response.status() >= 400) errors.push(`HTTP ${response.status()} ${new URL(response.url()).pathname}`) })
    await page.goto(origin)
    await page.getByLabel('用户名', { exact: true }).fill(server.owner.username); await page.getByLabel('密码', { exact: true }).fill(server.owner.password); await page.getByRole('button', { name: '登录', exact: true }).click()
    await page.locator('[data-composer-input]').fill('Delegate one child to search for evidence.')
    await page.getByRole('button', { name: '发送', exact: true }).click()
    const approval = page.locator('[data-tool-approval]')
    await approval.waitFor()
    assert.ok((await approval.textContent()).includes('启动子 Agent'))
    assert.ok((await approval.textContent()).includes('CHILD_WEB_TASK'))
    const pending = (await request(`/questions?session_id=${session.identity.session_id}`))[0]
    const unrelated = await request('/sessions', { body: { workspace_id: workspace.workspace_id } })
    await assert.rejects(request(`/questions/${pending.question.id}/answer?session_id=${unrelated.identity.session_id}`, {
      body: { selected: [pending.question.options[0].label] },
    }), /pending question does not belong to this session/)
    await approval.getByRole('button', { name: '允许一次', exact: true }).tap()
    const events = await until(() => request(`${resource}/events`), events => events.some(event => ['turn_finished', 'turn_failed'].includes(event.type)), 'completed parent')
    assert.equal(events.some(event => event.type === 'turn_failed'), false, JSON.stringify(events))
    const updates = events.filter(event => event.type === 'subagent_updated')
    assert.ok(updates.some(event => event.subagent.status === 'idle'), JSON.stringify(updates))
    assert.equal(webCalls.length, 1)
    const state = await request('/state')
    const child = state.sessions.find(item => item.parent_session_id === session.identity.session_id)
    assert.ok(child, 'child is registered on Server')
    const childEvents = await request(`/sessions/${child.identity.session_id}/events`)
    assert.ok(childEvents.some(event => event.type === 'tool_call_finished' && event.name === 'web_search' && !event.output.is_error))
    assert.ok(!(JSON.stringify(events) + JSON.stringify(childEvents)).includes('fixture-search-secret'))
    assert.deepEqual((await local('/credentials')).references, [], 'search credential is not persisted on the execution computer')
    await page.getByText('Parent delegation finished.', { exact: true }).waitFor()
    await page.screenshot({ path: path.join(artifacts, 'subagent-web-mobile.png') })
    assert.deepEqual(errors, [])
  } catch (error) {
    await page?.screenshot({ path: path.join(artifacts, 'subagent-web-failure.png') }).catch(() => {})
    console.error(processes.map(process => process.diagnostics()).join('\n').slice(-6000))
    throw error
  } finally {
    await browser?.close(); await new Promise(resolve => { upstream.closeAllConnections(); upstream.close(resolve) })
    for (const process of processes.reverse()) await stopProcess(process)
    await rm(directory, { recursive: true, force: true })
  }
})
