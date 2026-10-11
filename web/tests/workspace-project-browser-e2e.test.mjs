import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { mkdir, mkdtemp, readFile, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { chromium } from 'playwright'
import { selectChoice } from './browser-select-fixture.mjs'
import { until } from './model-device-fixture.mjs'
import { freePort, initializeServer, repository, selectSpace, serverRequest, startProcess, stopProcess, waitForHttp } from './platform-e2e-fixture.mjs'

async function login(page, origin, account, tenant) {
  await page.goto(origin)
  await page.getByLabel('用户名', { exact: true }).fill(account.username)
  await page.getByLabel('密码', { exact: true }).fill(account.password)
  await page.getByRole('button', { name: '登录', exact: true }).click()
  await selectSpace(page, tenant)
}

async function openMove(page) {
  const toggle = page.getByRole('button', { name: /^(展开|打开)侧边栏$/ })
  if (await toggle.isVisible()) await toggle.click()
  const row = page.locator('[data-sidebar-workspace-row]').filter({ hasText: 'Moving workspace' })
  await row.hover()
  await row.getByRole('button', { name: /的操作$/ }).click()
  await page.getByRole('menuitem', { name: '移动到项目…', exact: true }).click()
  return page.getByRole('dialog', { name: '移动工作区', exact: true })
}

test('workspace project moves preserve real Node files and history, revoke inherited access, and survive reconnection', { timeout: 180_000 }, async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-workspace-project-'))
  const artifacts = process.env.TERNILO_E2E_ARTIFACT_DIR ?? directory
  await mkdir(artifacts, { recursive: true })
  const processes = []
  let browser, ownerPage, readerPage
  const observations = { pageErrors: [], consoleErrors: [], httpErrors: [] }
  let revoking = false
  let restarting = false
  const responseBodies = []
  try {
    const origin = `http://127.0.0.1:${await freePort()}`
    const environment = { XDG_STATE_HOME: path.join(directory, 'state') }
    const server = await initializeServer({ directory: path.join(directory, 'server'), origin, environment })
    processes.push(server)
    const admin = (resource, options = {}) => serverRequest(origin, resource, { token: server.owner.session.access_token, ...options })
    const tenant = (await admin('/tenants', { body: { slug: 'project-move', display_name: 'Project move team' } })).tenant.tenant_id
    const owner = (resource, options = {}) => admin(resource, { tenantId: tenant, ...options })
    const source = (await owner('/projects')).projects[0]
    await owner(`/projects/${source.project_id}`, { method: 'PATCH', body: { name: 'Source project' } })
    const destination = (await owner('/projects', { body: { name: 'Destination project' } })).project
    const member = async username => {
      const invitation = await admin('/admin/invitations', { body: { tenant_id: null, role: 'member', expires_in_seconds: 3600 } })
      const account = { username, password: `${username}-password`, email: `${username}@example.test` }
      const identity = await serverRequest(origin, '/auth/invitations/accept', { body: { ...account, token: invitation.token } })
      await admin(`/tenants/${tenant}/members/${identity.user.user_id}`, { method: 'PUT', body: { role: 'member' } })
      return { ...account, identity, request: (resource, options = {}) => serverRequest(origin, resource, { token: identity.access_token, tenantId: tenant, ...options }) }
    }
    const reader = await member('previous-reader'), nextReader = await member('next-reader')
    const read = { view: true, submit: false, stop: false, configure: false }
    await owner(`/projects/${source.project_id}/sharing/user/${reader.identity.user.user_id}`, { method: 'PUT', body: read })
    await owner(`/projects/${destination.project_id}/sharing/user/${nextReader.identity.user.user_id}`, { method: 'PUT', body: read })
    const enrollment = (await owner(`/tenants/${tenant}/my-computer-enrollments`, { body: { name: 'project-computer', project_id: source.project_id, ttl_seconds: 600 } })).enrollment
    const credential = (await owner('/enrollments/consume', { body: { token: enrollment.token } })).credential
    const nodeOrigin = `http://127.0.0.1:${await freePort()}`
    const nodeArguments = ['serve', '--listen', new URL(nodeOrigin).host, '--data-dir', path.join(directory, 'node'), '--node-id', enrollment.executor_id,
      '--gateway-url', `${origin.replace('http:', 'ws:')}/api/v1/executors/connect`, '--allow-insecure-gateway']
    const startNode = () => startProcess(process.env.TERNILO_E2E_NODE_BINARY ?? path.join(repository, 'target/debug/ternilo'), nodeArguments, { ...environment, TERNILO_LOCAL_TOKEN: credential.token })
    let node = startNode(); processes.push(node); await waitForHttp(nodeOrigin, node)
    await until(() => owner('/execution-targets'), value => value.executors.some(executor => executor.executor_id === enrollment.executor_id && executor.connected), 'Node connected')
    const folder = path.join(directory, 'workspace'); await mkdir(folder)
    const workspace = (await owner('/workspaces', { body: { project_id: source.project_id, name: 'Moving workspace', placement: 'local_node', executor_id: enrollment.executor_id, path: folder } })).workspace
    const session = await owner('/sessions', { body: { workspace_id: workspace.workspace_id } })
    const id = session.identity.session_id, endpoint = `/workspaces/${workspace.workspace_id}/project`
    const run = async (runId, input) => {
      await owner(`/sessions/${id}/queue`, { body: { run_id: runId, content: { kind: 'prompt', input } } })
      await until(() => owner(`/sessions/${id}/events`), events => events.some(event => event.run_id === runId && event.type === 'turn_finished'), `${runId} completed`)
    }
    await run('before-project-move', '/write project.txt before-project-move')
    const history = await owner(`/sessions/${id}/events`)
    await owner(`/workspaces/${workspace.workspace_id}/project-sharing`, { method: 'PUT', body: { enabled: true } })
    await assert.rejects(reader.request(endpoint, { method: 'PUT', body: { project_id: destination.project_id, expected_project_id: source.project_id, expected_revision: 0, inherit_project_sharing: false } }), /403/)
    for (const asset of ['app.js', 'app.css']) {
      const served = Buffer.from(await (await fetch(`${origin}/assets/${asset}`)).arrayBuffer())
      assert.equal(createHash('sha256').update(served).digest('hex'), createHash('sha256').update(await readFile(path.join(repository, 'web/dist/assets', asset))).digest('hex'))
    }
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
    const ownerContext = await browser.newContext({ locale: 'zh-CN', viewport: { width: 1280, height: 800 }, hasTouch: true, serviceWorkers: 'block' })
    const readerContext = await browser.newContext({ locale: 'zh-CN', viewport: { width: 1280, height: 800 }, serviceWorkers: 'block' })
    ownerPage = await ownerContext.newPage(); readerPage = await readerContext.newPage()
    const receivedRuns = new Set()
    readerPage.on('websocket', socket => socket.on('framereceived', frame => {
      try {
        const value = JSON.parse(String(frame.payload))
        if (value.type === 'event_batch' && value.session_id === id) for (const event of value.events ?? []) receivedRuns.add(event.run_id)
      } catch {}
    }))
    for (const page of [ownerPage, readerPage]) {
      page.on('pageerror', error => observations.pageErrors.push(error.message))
      page.on('console', message => { if (message.type() === 'error') observations.consoleErrors.push({ url: message.location().url, text: message.text() }) })
      page.on('response', response => {
        if (response.status() < 400) return
        const failure = { url: response.url(), status: response.status(), actor: page === ownerPage ? 'owner' : 'previous-reader', revoking, restarting, body: null }
        observations.httpErrors.push(failure)
        responseBodies.push(response.json().then(body => { failure.body = body }).catch(error => { observations.pageErrors.push(`Unreadable error response: ${error.message}`) }))
      })
    }
    await login(ownerPage, origin, server.owner, tenant)
    await login(readerPage, origin, reader, tenant)
    await readerPage.locator(`[data-session-id="${id}"] [data-sidebar-session-button]`).click()
    await readerPage.getByText('/write project.txt before-project-move', { exact: false }).first().waitFor()
    let dialog = await openMove(ownerPage)
    await selectChoice(dialog.locator('#workspace-move-project'), destination.project_id)
    assert.equal(await dialog.getByLabel('采用目标项目的共享规则', { exact: true }).isChecked(), false)
    assert.equal(await dialog.getByRole('button', { name: '移动工作区', exact: true }).isEnabled(), false)
    await dialog.getByLabel('确认移动到“Destination project”', { exact: true }).check()
    await dialog.screenshot({ path: path.join(artifacts, 'workspace-project-desktop.png') })
    revoking = true
    const saved = ownerPage.waitForResponse(response => new URL(response.url()).pathname === `/api/v1${endpoint}` && response.request().method() === 'PUT')
    await dialog.getByRole('button', { name: '移动工作区', exact: true }).click()
    assert.equal((await saved).status(), 200)
    await dialog.waitFor({ state: 'hidden' })
    await readerPage.locator(`[data-session-id="${id}"] [data-sidebar-session-button]`).waitFor({ state: 'detached' })
    assert.equal(await readerPage.locator('article[data-role="user"]').count(), 0)
    assert.equal((await nextReader.request('/state')).sessions.length, 0)
    let current = (await owner('/state')).workspaces.find(value => value.workspace_id === workspace.workspace_id)
    assert.equal(current.management_project_id, destination.project_id)
    assert.equal(current.project_id, source.project_id)
    await ownerPage.locator('[data-session-project]').filter({ hasText: 'Destination project' }).waitFor()
    assert.deepEqual((await owner(`/sessions/${id}/events`)).slice(0, history.length), history)
    assert.equal(await readFile(path.join(folder, 'project.txt'), 'utf8'), 'before-project-move')
    await run('after-project-move', '/write after.txt after-project-move')
    assert.equal(receivedRuns.has('after-project-move'), false, 'revoked project readers receive no events from later tasks')
    assert.equal(await readFile(path.join(folder, 'after.txt'), 'utf8'), 'after-project-move')
    restarting = true
    await stopProcess(node)
    node = startNode(); processes.push(node); await waitForHttp(nodeOrigin, node)
    await until(() => owner('/execution-targets'), value => value.executors.some(executor => executor.executor_id === enrollment.executor_id && executor.connected), 'Node reconnected')
    current = (await owner('/state')).workspaces.find(value => value.workspace_id === workspace.workspace_id)
    assert.equal(current.management_project_id, destination.project_id)
    assert.equal(current.node_id, enrollment.executor_id)
    await run('after-reconnect', '/read after.txt')
    restarting = false
    await ownerPage.setViewportSize({ width: 320, height: 560 })
    dialog = await openMove(ownerPage)
    await selectChoice(dialog.locator('#workspace-move-project'), source.project_id)
    const scroll = dialog.locator('.overflow-y-auto')
    const scrollBox = await scroll.boundingBox()
    assert.ok(scrollBox)
    await scroll.evaluate(element => { element.scrollTop = 0 })
    const touch = await ownerContext.newCDPSession(ownerPage)
    const x = scrollBox.x + scrollBox.width / 2, y = scrollBox.y + scrollBox.height * 0.8
    await touch.send('Input.dispatchTouchEvent', { type: 'touchStart', touchPoints: [{ x, y }] })
    for (let step = 1; step <= 8; step++) await touch.send('Input.dispatchTouchEvent', { type: 'touchMove', touchPoints: [{ x, y: y - scrollBox.height * 0.6 * step / 8 }] })
    await touch.send('Input.dispatchTouchEvent', { type: 'touchEnd', touchPoints: [] })
    await until(() => scroll.evaluate(element => element.scrollTop), value => value > 0, 'mobile project dialog scrolls with touch')
    await touch.detach()
    await dialog.getByLabel('采用目标项目的共享规则', { exact: true }).check()
    await dialog.getByLabel('确认移动到“Source project”', { exact: true }).check()
    const bounds = await dialog.evaluate(element => ({ left: element.getBoundingClientRect().left, right: element.getBoundingClientRect().right, width: innerWidth, overflow: document.documentElement.scrollWidth > innerWidth }))
    assert.ok(bounds.left >= 0 && bounds.right <= bounds.width && !bounds.overflow, JSON.stringify(bounds))
    for (const name of ['取消', '移动工作区']) {
      const reachable = await dialog.getByRole('button', { name, exact: true }).evaluate(element => {
        const box = element.getBoundingClientRect()
        const hit = document.elementFromPoint(box.x + box.width / 2, box.y + box.height / 2)
        return box.top >= 0 && box.bottom <= innerHeight && (hit === element || element.contains(hit))
      })
      assert.ok(reachable, `${name} remains fully visible and unobstructed on mobile`)
    }
    await dialog.screenshot({ path: path.join(artifacts, 'workspace-project-mobile.png') })
    await dialog.getByRole('button', { name: '移动工作区', exact: true }).click()
    await dialog.waitFor({ state: 'hidden' })
    await readerPage.locator(`[data-session-id="${id}"] [data-sidebar-session-button]`).waitFor()
    assert.equal((await owner(endpoint)).revision, 2)
    assert.equal((await owner(endpoint)).execution_project_id, source.project_id)
    assert.deepEqual((await reader.request(`/sessions/${id}/events`)).slice(0, history.length), history)
    assert.equal(await readFile(path.join(folder, 'after.txt'), 'utf8'), 'after-project-move')
    await Promise.all(responseBodies)
    assert.deepEqual(observations.pageErrors, [])
    for (const failure of observations.httpErrors) {
      const resourceRead = new URL(failure.url).pathname.startsWith(`/api/v1/sessions/${id}/`)
      const revoked = failure.actor === 'previous-reader' && failure.revoking && [400, 403].includes(failure.status)
        && ['policy_denied', 'invalid_input'].includes(failure.body?.error?.code)
      const offline = failure.restarting && failure.status === 503
        && failure.body?.error?.code === 'unavailable'
        && failure.body?.error?.message?.startsWith('selected Ternilo node is offline')
      assert.ok(resourceRead && (revoked || offline), JSON.stringify(failure))
    }
    for (const failure of observations.consoleErrors) assert.ok(observations.httpErrors.some(value => value.url === failure.url) && failure.text.startsWith('Failed to load resource:'), JSON.stringify(failure))
    console.log(JSON.stringify({ source: source.project_id, destination: destination.project_id, revision: 2, filesPreserved: true, historyPreserved: true, observations }))
  } catch (error) {
    await ownerPage?.screenshot({ path: path.join(artifacts, 'workspace-project-failure.png') }).catch(() => {})
    error.message += `\n${processes.map(process => process.diagnostics()).join('\n')}`
    throw error
  } finally {
    await browser?.close()
    for (const process of processes.reverse()) await stopProcess(process)
    await rm(directory, { recursive: true, force: true })
  }
})
