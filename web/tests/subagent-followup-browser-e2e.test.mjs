import assert from 'node:assert/strict'
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { chromium } from 'playwright'
import { localApi, until } from './model-device-fixture.mjs'
import { freePort, initializeServer, repository, serverRequest, startProcess, stopProcess, waitForHttp } from './platform-e2e-fixture.mjs'

test('one parent turn reuses a child without duplicate runs or stale sidebar activity; conversation plans stay distinct from shared tasks', { timeout: 120_000 }, async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-child-followup-'))
  const artifacts = process.env.TERNILO_E2E_ARTIFACT_DIR ?? directory
  await mkdir(artifacts, { recursive: true })
  const processes = [], errors = [], network = []
  let browser, page
  try {
    const environment = { XDG_STATE_HOME: path.join(directory, 'state') }
    const server = await initializeServer({ directory: path.join(directory, 'server'), origin: `http://127.0.0.1:${await freePort()}`, environment })
    processes.push(server)
    const tenantId = server.owner.session.personal_tenant_id
    const owner = (resource, options = {}) => serverRequest(server.origin, resource, { token: server.owner.session.access_token, tenantId, ...options })
    const enrolled = await owner(`/tenants/${tenantId}/my-computer-enrollments`, { body: { name: 'Follow-up computer', project_id: null, ttl_seconds: 600 } })
    const credential = await owner('/enrollments/consume', { body: { token: enrolled.enrollment.token } })
    const origin = `http://127.0.0.1:${await freePort()}`
    const node = startProcess(process.env.TERNILO_E2E_NODE_BINARY ?? path.join(repository, 'target/debug/ternilo'), [
      'serve', '--listen', new URL(origin).host, '--data-dir', path.join(directory, 'node'), '--node-id', enrolled.enrollment.executor_id,
      '--gateway-url', `${server.origin.replace('http:', 'ws:')}/api/v1/executors/connect`, '--allow-insecure-gateway',
    ], { ...environment, TERNILO_LOCAL_TOKEN: credential.credential.token })
    processes.push(node); await waitForHttp(origin, node)
    const local = await localApi(origin)
    const folder = path.join(directory, 'workspace'); await mkdir(folder)
    const workspace = await local('/workspaces', { body: { path: folder } })
    const root = await local('/sessions', { body: { workspace_id: workspace.workspace_id } })
    const state = await until(() => owner('/state'), value => value.sessions.length === 1, 'parent mapped')
    const parentId = state.sessions[0].identity.session_id
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
    page = await browser.newPage({ locale: 'zh-CN', viewport: { width: 1280, height: 900 }, serviceWorkers: 'block' })
    page.on('pageerror', error => errors.push(error.message))
    page.on('console', message => { if (message.type() === 'error') errors.push(message.text()) })
    page.on('response', response => { if (response.url().includes('/api/v1/')) network.push({ path: new URL(response.url()).pathname, status: response.status() }); if (response.status() >= 400) errors.push(`${response.status()} ${new URL(response.url()).pathname}`) })
    await page.goto(server.origin)
    await page.getByLabel('用户名', { exact: true }).fill(server.owner.username)
    await page.getByLabel('密码', { exact: true }).fill(server.owner.password)
    await page.getByRole('button', { name: '登录', exact: true }).click()
    await page.locator(`[data-sidebar-session-row][data-session-id="${parentId}"]`).click()
    await page.setViewportSize({ width: 390, height: 844 })
    const script = 'tools::update_plan(#{ items: [#{step: "Verify child reuse", status: "in_progress"}] }); '
      + 'let child = tools::spawn_agent(#{ task: "/code 1", label: "Reusable child", background: false }); '
      + 'tools::send_agent_message(#{ subagent_id: child.subagent_id, message: "/code 2" }); '
      + 'tools::wait_agent(#{ subagent_id: child.subagent_id }); '
      + 'tools::send_agent_message(#{ subagent_id: child.subagent_id, message: "/code 3" }); '
      + 'let result = tools::wait_agent(#{ subagent_id: child.subagent_id }); '
      + 'tools::update_plan(#{ items: [#{step: "Verify child reuse", status: "completed"}] }); result'
    const accepted = await owner(`/sessions/${parentId}/queue`, { body: { content: { kind: 'prompt', input: `/code ${script}` } } })
    const answered = new Set()
    for (let index = 0; index < 3; index++) {
      const pending = await until(() => owner(`/questions?session_id=${parentId}`), questions => questions.some(item => !answered.has(item.question.id)), 'next child approval')
      const question = pending.find(item => !answered.has(item.question.id)).question
      await page.locator(`[data-tool-approval][aria-labelledby="approval-${question.id}"]`).getByRole('button', { name: '允许一次', exact: true }).click()
      answered.add(question.id)
    }
    const events = await until(() => owner(`/sessions/${parentId}/events`), value => value.some(event => event.run_id === accepted.run_id && ['turn_finished', 'turn_failed'].includes(event.type)), 'parent finished')
    assert.equal(events.some(event => event.type === 'turn_failed'), false)
    const snapshots = events.filter(event => event.type === 'subagent_updated').map(event => event.subagent)
    assert.equal(snapshots.at(-1).status, 'idle')
    assert.ok(!snapshots.some(child => child.error), JSON.stringify(snapshots))
    const child = (await local('/sessions')).find(session => session.parent_session_id === root.identity.session_id && session.subagent)
    assert.ok(child)
    const history = await local(`/sessions/${child.identity.session_id}/events`)
    const inputs = history.filter(event => event.type === 'user_message')
    assert.equal(inputs.length, 3)
    assert.equal(new Set(inputs.map(event => event.run_id)).size, 3)
    assert.ok(inputs.every(event => event.source?.kind === 'subagent' && event.source.parent_run_id === accepted.run_id))
    assert.equal(history.filter(event => event.type === 'turn_finished').length, 3)
    const mapped = await until(() => owner('/state'), value => value.sessions.some(session => session.parent_session_id === parentId && session.subagent), 'child mapped')
    const publicChild = mapped.sessions.find(session => session.parent_session_id === parentId && session.subagent)
    await page.setViewportSize({ width: 1280, height: 900 })
    const row = page.locator(`[data-sidebar-session-row][data-session-id="${publicChild.identity.session_id}"]`)
    await row.waitFor()
    await page.locator('[data-execution-phase]').waitFor({ state: 'detached' })
    await until(() => row.locator('[data-state="running"]').count(), count => count === 0, 'child sidebar settles')
    await page.screenshot({ path: path.join(artifacts, 'child-followup-settled.png'), animations: 'disabled' })
    await page.getByRole('button', { name: '打开 Agent Team', exact: true }).first().click()
    const team = page.locator('[data-agent-team-panel]')
    await team.locator('#agent-team-tasks-tab').click()
    await team.getByText('还没有共享任务', { exact: true }).waitFor()
    await team.getByText(/对话中的执行计划单独记录/).waitFor()
    assert.ok((await team.locator('#agent-team-tasks-tab').innerText()).includes('共享任务'))
    await page.setViewportSize({ width: 390, height: 844 })
    assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth))
    await page.screenshot({ path: path.join(artifacts, 'shared-task-empty-mobile.png'), animations: 'disabled' })
    for (const asset of ['app.js', 'app.css']) assert.ok(Buffer.from(await (await fetch(`${server.origin}/assets/${asset}`)).arrayBuffer()).equals(await readFile(path.join(repository, 'web/dist/assets', asset))))
    assert.deepEqual(errors, [])
  } catch (error) {
    await page?.screenshot({ path: path.join(artifacts, 'child-followup-failure.png'), fullPage: true }).catch(() => {})
    throw error
  } finally {
    await writeFile(path.join(artifacts, 'child-followup-evidence.json'), JSON.stringify({ errors, network }, null, 2))
    await browser?.close()
    for (const process of processes.reverse()) await stopProcess(process)
    await rm(directory, { recursive: true, force: true })
  }
})
