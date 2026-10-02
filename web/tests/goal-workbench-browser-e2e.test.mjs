import { selectChoice } from './browser-select-fixture.mjs'
import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { createServer } from 'node:http'
import { mkdir, mkdtemp, readFile, rm } from 'node:fs/promises'
import path from 'node:path'
import { tmpdir } from 'node:os'
import test from 'node:test'
import { chromium } from 'playwright'
import { freePort, initializeServer, repository, selectSpace, serverRequest, startProcess, stopProcess, waitForHttp } from './platform-e2e-fixture.mjs'

async function until(read, ready, label) {
  const deadline = Date.now() + 30_000
  while (Date.now() < deadline) {
    const value = await read()
    if (ready(value)) return value
    await new Promise(resolve => setTimeout(resolve, 75))
  }
  throw new Error(`Timed out: ${label}`)
}

async function modelFixture() {
  const calls = []
  let held = false
  const server = createServer(async (request, response) => {
    const chunks = []
    for await (const chunk of request) chunks.push(chunk)
    const body = JSON.parse(Buffer.concat(chunks).toString())
    const title = body.messages[0].content.startsWith('You name software-agent conversations.')
    const goalIndex = body.messages.findLastIndex(message => message.role === 'user' && typeof message.content === 'string' && message.content.startsWith('<goal_round>'))
    if (title) { response.writeHead(200, { 'content-type': 'application/json' }).end(JSON.stringify({ choices: [{ message: { content: '目标执行验收' }, finish_reason: 'stop' }] })); return }
    assert.ok(goalIndex >= 0, 'model receives an explicit goal round')
    const prompt = body.messages[goalIndex].content
    const objective = JSON.parse(prompt.split('\n').find(line => line.startsWith('Objective: ')).slice('Objective: '.length))
    const round = Number(prompt.match(/Round: (\d+)/)[1])
    calls.push({ objective, round, body })
    if (objective.includes('停止恢复') && !held) {
      held = true
      response.writeHead(200, { 'content-type': 'text/event-stream' })
      response.write(`data: ${JSON.stringify({ choices: [{ delta: { reasoning_content: '等待用户停止运行' } }] })}\n\n`)
      return
    }
    const tool = (name, argumentsValue) => ({ tool_calls: [{ id: `${name}-${calls.length}`, type: 'function', function: { name, arguments: JSON.stringify(argumentsValue) } }] })
    const recent = body.messages.slice(goalIndex + 1)
    const completedTools = recent.filter(message => message.role === 'tool')
    let message
    if (objective.includes('遇阻')) {
      message = completedTools.length ? { content: '缺少必要的授权，目标已标记为受阻。' } : tool('update_goal', { objective, status: 'blocked' })
    } else if (objective.includes('停止恢复')) {
      message = completedTools.length ? { content: '恢复执行已验证。' } : tool('update_goal', { objective, status: 'complete' })
    } else if (round === 1) {
      message = { content: '还有文件写入和验证需要完成。' }
    } else if (!completedTools.length) {
      message = tool('write_file', { path: 'goal-proof.txt', content: 'goal runtime proof\n' })
    } else if (completedTools.length === 1) {
      message = tool('update_goal', { objective, status: 'complete' })
    } else {
      message = { content: '目标已完成，文件写入结果已验证。' }
    }
    response.writeHead(200, { 'content-type': 'application/json' }).end(JSON.stringify({ choices: [{ message, finish_reason: message.tool_calls ? 'tool_calls' : 'stop' }] }))
  })
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve))
  return { calls, baseUrl: `http://127.0.0.1:${server.address().port}/v1`, close: () => { server.closeAllConnections(); return new Promise(resolve => server.close(resolve)) } }
}

test('goal execution, stop/resume and themed space switching work in real Local and Server browsers', { timeout: 240_000 }, async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-goal-browser-'))
  const artifacts = process.env.TERNILO_E2E_ARTIFACT_DIR
  const processes = [], errors = []
  let browser, page, model
  try {
    if (artifacts) await mkdir(artifacts, { recursive: true })
    model = await modelFixture()
    const application = await initializeServer({ directory: path.join(directory, 'server'), origin: `http://127.0.0.1:${await freePort()}` })
    processes.push(application)
    const owner = (resource, options = {}) => serverRequest(application.origin, resource, { token: application.owner.session.access_token, ...options })
    const team = (await owner('/tenants', { body: { slug: 'goal-team', display_name: '目标验收工作空间' } })).tenant
    const project = (await owner('/projects', { tenantId: team.tenant_id })).projects[0]
    const enrollment = (await owner(`/tenants/${team.tenant_id}/my-computer-enrollments`, { body: { name: 'goal-node', project_id: project.project_id, ttl_seconds: 600 } })).enrollment
    const enrolledComputerId = enrollment.executor_id
    const credential = (await serverRequest(application.origin, '/enrollments/consume', { body: { token: enrollment.token } })).credential
    const folder = path.join(directory, 'workspace')
    await mkdir(folder)
    const localOrigin = `http://127.0.0.1:${await freePort()}`
    const node = startProcess(process.env.TERNILO_E2E_NODE_BINARY ?? path.join(repository, 'target/debug/ternilo'), [
      'serve', '--listen', new URL(localOrigin).host, '--data-dir', path.join(directory, 'node'), '--node-id', enrolledComputerId,
      '--gateway-url', `${application.origin.replace('http:', 'ws:')}/api/v1/executors/connect`, '--allow-insecure-gateway',
    ], { TERNILO_LOCAL_TOKEN: credential.token, XDG_STATE_HOME: path.join(directory, 'state') })
    processes.push(node)
    await waitForHttp(localOrigin, node)
    const html = await (await fetch(localOrigin)).text()
    const localToken = JSON.parse(html.match(/window\.__TERNILO_BOOT__\s*=\s*(\{[^<]+\});/)[1]).apiToken
    const local = (resource, options = {}) => serverRequest(localOrigin, resource, { token: localToken, ...options })
    const workspace = await local('/workspaces', { body: { path: folder } })
    const session = await local('/sessions', { body: { workspace_id: workspace.workspace_id } })
    const resource = `/sessions/${session.identity.session_id}`
    await local(resource, { method: 'PATCH', body: { model: { provider: 'open_ai_compatible', base_url: model.baseUrl, model: 'goal-fixture', api_key_env: null, timeout_ms: 0, max_attempts: 1, retry_base_delay_ms: 10 } } })
    const state = await until(() => owner('/state', { tenantId: team.tenant_id }), value => value.sessions.length === 1, 'Node discovery')
    const remoteSessionId = state.sessions[0].identity.session_id
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE })
    page = await browser.newPage({ viewport: { width: 1500, height: 950 }, hasTouch: true })
    page.on('pageerror', error => errors.push(error.message))
    page.on('console', message => { if (message.type() === 'error') errors.push(message.text()) })
    page.on('response', response => { if (response.status() >= 400) errors.push(`${response.status()} ${new URL(response.url()).pathname}`) })
    for (const origin of [localOrigin, application.origin]) for (const asset of ['app.js', 'app.css']) {
      assert.equal(createHash('sha256').update(Buffer.from(await (await fetch(`${origin}/assets/${asset}`)).arrayBuffer())).digest('hex'), createHash('sha256').update(await readFile(path.join(repository, 'web/dist/assets', asset))).digest('hex'))
    }
    const input = page.locator('[data-composer-input]')
    const submitGoal = async objective => {
      await input.fill(`/goal ${objective}`)
      await page.getByRole('button', { name: '发送', exact: true }).click()
    }
    const waitIdle = () => until(() => local(`${resource}/queue`), queue => !queue.active_run_id && queue.items.length === 0, 'goal execution finishes')
    await page.goto(localOrigin)
    await page.locator(`[data-sidebar-session-row][data-session-id="${session.identity.session_id}"] [data-sidebar-session-button]`).click()
    await submitGoal('验证本机目标并写入文件')
    await page.getByText('目标已完成，文件写入结果已验证。', { exact: true }).first().waitFor()
    await waitIdle()
    assert.equal(await readFile(path.join(folder, 'goal-proof.txt'), 'utf8'), 'goal runtime proof\n')
    assert.equal(model.calls.filter(call => call.objective === '验证本机目标并写入文件').length, 4)
    const localEvents = await local(`${resource}/events`)
    assert.deepEqual(localEvents.filter(event => event.type === 'goal_round_started').map(event => event.round), [1, 2])
    assert.equal(localEvents.filter(event => event.type === 'user_message').length, 1)
    if (artifacts) await page.screenshot({ path: path.join(artifacts, 'goal-local.png') , animations: 'disabled' })

    await page.goto(application.origin)
    await page.getByLabel('用户名', { exact: true }).fill(application.owner.username)
    await page.getByLabel('密码', { exact: true }).fill(application.owner.password)
    await page.getByRole('button', { name: '登录', exact: true }).click()
    await selectSpace(page, team.tenant_id)
    await page.locator(`[data-sidebar-session-row][data-session-id="${remoteSessionId}"] [data-sidebar-session-button]`).click()
    const space = page.getByRole('combobox', { name: '切换空间', exact: true })
    assert.ok((await page.locator('[data-space-switcher]').boundingBox()).y < (await page.locator('[data-sidebar-files]').boundingBox()).y)
    await space.focus()
    await space.press('ArrowDown')
    const spaceMenu = page.locator('[data-space-menu]')
    await spaceMenu.waitFor()
    assert.equal(await spaceMenu.evaluate(element => getComputedStyle(element).colorScheme), 'dark')
    assert.notEqual(await spaceMenu.evaluate(element => getComputedStyle(element).backgroundColor), 'rgb(255, 255, 255)')
    assert.equal(await spaceMenu.locator('[data-state="checked"]').getAttribute('data-space-id'), team.tenant_id)
    if (artifacts) await page.screenshot({ path: path.join(artifacts, 'space-menu-dark.png') , animations: 'disabled' })
    await page.keyboard.press('ArrowUp')
    await page.keyboard.press('Escape')
    assert.equal(await space.getAttribute('data-space-id'), team.tenant_id)
    await until(() => space.evaluate(element => element === document.activeElement), Boolean, 'space trigger focus restored')
    assert.equal(await page.locator('[data-workspace-open-app]').count(), 0)

    await submitGoal('验证远程目标并写入文件')
    await until(() => model.calls.filter(call => call.objective === '验证远程目标并写入文件').length, count => count === 4, 'remote goal automatic continuation')
    await waitIdle()
    const remoteEvents = await local(`${resource}/events`)
    const original = remoteEvents.find(event => event.type === 'user_message' && event.content === '/goal 验证远程目标并写入文件')
    assert.equal(original.provenance.author.kind, 'account')
    assert.equal(original.provenance.author.user_id, application.owner.session.user.user_id)
    assert.equal(remoteEvents.filter(event => event.run_id === original.run_id && event.type === 'user_message').length, 1)
    assert.deepEqual(remoteEvents.filter(event => event.run_id === original.run_id && event.type === 'goal_round_started').map(event => event.round), [1, 2])
    if (artifacts) await page.screenshot({ path: path.join(artifacts, 'goal-server.png') , animations: 'disabled' })

    await page.setViewportSize({ width: 390, height: 844 })
    await submitGoal('验证目标停止恢复')
    await until(() => model.calls.some(call => call.objective === '验证目标停止恢复'), Boolean, 'goal begins before cancellation')
    await page.getByText('已启动执行', { exact: true }).last().waitFor()
    await page.getByRole('button', { name: '停止运行', exact: true }).tap()
    await waitIdle()
    const beforeReload = model.calls.length
    await page.reload()
    await page.getByRole('button', { name: '继续目标', exact: true }).waitFor()
    await new Promise(resolve => setTimeout(resolve, 250))
    assert.equal(model.calls.length, beforeReload)
    assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth))
    if (artifacts) await page.screenshot({ path: path.join(artifacts, 'goal-stopped-mobile.png') , animations: 'disabled' })
    await page.getByRole('button', { name: '继续目标', exact: true }).tap()
    await page.getByText('恢复执行已验证。', { exact: true }).first().waitFor()
    await waitIdle()
    await submitGoal('验证目标遇阻退出')
    await page.getByText('缺少必要的授权，目标已标记为受阻。', { exact: true }).first().waitFor()
    await waitIdle()
    assert.equal(model.calls.filter(call => call.objective === '验证目标遇阻退出').length, 2)
    assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth))
    await page.getByRole('button', { name: '打开侧边栏', exact: true }).tap()
    await space.tap()
    await spaceMenu.waitFor()
    const bounds = await spaceMenu.boundingBox()
    assert.ok(bounds.x >= 0 && bounds.x + bounds.width <= 390)
    assert.ok((await spaceMenu.getByRole('option').first().boundingBox()).height >= 40)
    if (artifacts) await page.screenshot({ path: path.join(artifacts, 'space-menu-mobile.png'), animations: 'disabled' })
    await spaceMenu.locator(`[data-space-id="${application.owner.session.personal_tenant_id}"]`).tap()
    await until(() => space.getAttribute('data-space-id'), value => value === application.owner.session.personal_tenant_id, 'touch switches to personal space')
    await until(() => space.isEnabled(), Boolean, 'space switch completes')
    await space.tap()
    await spaceMenu.locator(`[data-space-id="${team.tenant_id}"]`).tap()
    await until(() => space.getAttribute('data-space-id'), value => value === team.tenant_id, 'touch returns to team space')
    await page.locator('aside').getByRole('button', { name: '关闭侧边栏', exact: true }).tap()

    await page.setViewportSize({ width: 1500, height: 950 })
    await page.getByRole('button', { name: '用户设置', exact: true }).click()
    const theme = page.getByRole('combobox', { name: '界面主题', exact: true })
    await selectChoice(theme, 'light')
    await until(() => theme.evaluate(element => getComputedStyle(element).colorScheme), value => value === 'light', 'native light controls')
    if (artifacts) await page.screenshot({ path: path.join(artifacts, 'settings-light.png') , animations: 'disabled' })
    await selectChoice(theme, 'dark')
    await until(() => theme.evaluate(element => getComputedStyle(element).colorScheme), value => value === 'dark', 'native dark controls')
    if (artifacts) await page.screenshot({ path: path.join(artifacts, 'settings-dark.png') , animations: 'disabled' })
    await page.setViewportSize({ width: 390, height: 844 })
    assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth))
    if (artifacts) await page.screenshot({ path: path.join(artifacts, 'settings-mobile.png') , animations: 'disabled' })
    await page.setViewportSize({ width: 1500, height: 950 })
    await page.goto(`${application.origin}/models`)
    await page.getByRole('heading', { name: '我的模型', exact: true }).waitFor()
    if (artifacts) await page.screenshot({ path: path.join(artifacts, 'models-desktop.png') , animations: 'disabled' })
    await page.setViewportSize({ width: 390, height: 844 })
    assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth))
    if (artifacts) await page.screenshot({ path: path.join(artifacts, 'models-mobile.png') , animations: 'disabled' })
    assert.deepEqual(errors, [])
  } catch (error) {
    if (page && artifacts) await page.screenshot({ path: path.join(artifacts, 'goal-failure.png') , animations: 'disabled' }).catch(() => {})
    throw new Error(`${error.stack}\nGoal calls: ${JSON.stringify(model?.calls.map(({ objective, round }) => ({ objective, round })))}\n${processes.map(process => process.diagnostics()).join('\n')}`)
  } finally {
    await browser?.close()
    for (const process of processes.reverse()) await stopProcess(process)
    await model?.close()
    await rm(directory, { recursive: true, force: true })
  }
})
