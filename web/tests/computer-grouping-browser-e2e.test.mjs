import assert from 'node:assert/strict'
import { mkdir, mkdtemp, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { chromium } from 'playwright'
import {
  freePort, initializeServer, repository, serverRequest, startProcess, stopProcess, waitForHttp,
} from './platform-e2e-fixture.mjs'

async function until(read, accepts, description) {
  for (let attempt = 0; attempt < 100; attempt += 1) {
    const result = await read()
    if (accepts(result)) return result
    await new Promise(resolve => setTimeout(resolve, 100))
  }
  throw new Error(description)
}

test('computer groups and online-only cold loads preserve separate workspaces on the same folder', { timeout: 120_000 }, async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-computer-groups-'))
  const processes = []
  let server, browser, page
  try {
    const origin = `http://127.0.0.1:${await freePort()}`
    server = await initializeServer({ directory: path.join(directory, 'server'), origin })
    const identity = server.owner.session
    const scope = { token: identity.access_token, tenantId: identity.personal_tenant_id }
    const folder = path.join(directory, 'Pictures')
    await mkdir(folder)
    const workspaces = []
    for (const id of ['alpha', 'beta']) {
      const { enrollment } = await serverRequest(origin, `/tenants/${scope.tenantId}/my-computer-enrollments`, {
        token: scope.token, body: { executor_id: id, project_id: null, ttl_seconds: 600 },
      })
      const { credential } = await serverRequest(origin, '/enrollments/consume', { body: { token: enrollment.token } })
      const localOrigin = `http://127.0.0.1:${await freePort()}`
      const node = startProcess(process.env.TERNILO_E2E_NODE_BINARY ?? path.join(repository, 'target/debug/ternilo'), [
        'serve', '--listen', new URL(localOrigin).host, '--data-dir', path.join(directory, id),
        '--node-id', id, '--gateway-url', `${origin.replace('http:', 'ws:')}/api/v1/executors/connect`,
        '--token', credential.token, '--allow-insecure-gateway',
      ])
      processes.push(node)
      await waitForHttp(localOrigin, node)
      await until(() => serverRequest(origin, '/execution-targets', scope),
        value => value.executors.some(executor => executor.executor_id === id && executor.connected), `${id} must connect`)
      const { workspace } = await serverRequest(origin, '/workspaces', { ...scope, body: {
        project_id: identity.personal_project_id, name: `Pictures ${id}`, placement: 'local_node', executor_id: id, path: folder,
      } })
      const session = await serverRequest(origin, '/sessions', { ...scope, body: { workspace_id: workspace.workspace_id } })
      workspaces.push({ workspace, session })
    }
    assert.notEqual(workspaces[0].workspace.workspace_id, workspaces[1].workspace.workspace_id)
    await stopProcess(processes[0])
    await until(() => serverRequest(origin, '/state', scope),
      value => value.workspaces.find(workspace => workspace.node_id === 'alpha')?.status === 'offline', 'alpha must be offline')

    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
    const context = await browser.newContext({ viewport: { width: 1365, height: 900 }, serviceWorkers: 'block' })
    page = await context.newPage()
    const errors = []
    const requests = []
    const states = []
    const sockets = []
    page.on('pageerror', error => errors.push(error.message))
    page.on('console', message => { if (message.type() === 'error') errors.push(message.text()) })
    page.on('request', request => requests.push(new URL(request.url())))
    page.on('websocket', socket => sockets.push(socket.url()))
    page.on('response', response => {
      const url = new URL(response.url())
      if (response.status() >= 400) errors.push(`HTTP ${response.status()}: ${url.pathname}`)
      if (url.pathname === '/api/v1/state') states.push({ url, body: response.json() })
    })
    await page.goto(origin)
    await page.getByLabel('用户名', { exact: true }).fill(server.owner.username)
    await page.getByLabel('密码', { exact: true }).fill(server.owner.password)
    await page.getByRole('button', { name: '登录', exact: true }).click()
    const alpha = page.locator('[data-sidebar-computer-group="node:alpha"]')
    const beta = page.locator('[data-sidebar-computer-group="node:beta"]')
    await alpha.waitFor()
    await beta.waitFor()
    assert.match(await alpha.textContent(), /电脑 alpha.*离线/s)
    assert.match(await beta.textContent(), /电脑 beta/s)
    assert.equal(await alpha.locator('[data-sidebar-workspace-row]').count(), 1)
    assert.equal(await beta.locator('[data-sidebar-workspace-row]').count(), 1)

    const options = page.getByRole('button', { name: '视图选项', exact: true })
    await options.click()
    await page.getByRole('menuitemcheckbox', { name: '只显示在线电脑', exact: true }).click()
    await alpha.waitFor({ state: 'detached' })
    await beta.waitFor()
    const online = states.findLast(state => state.url.searchParams.get('online_computers_only') === 'true')
    assert.ok(online, 'online-only changes must request a scoped state')
    assert.deepEqual((await online.body).workspaces.map(workspace => workspace.node_id), ['beta'])
    assert.equal((await online.body).sessions.length, 1)

    const coldStart = requests.length
    const coldStates = states.length
    const coldSockets = sockets.length
    await page.reload()
    await beta.waitFor()
    await until(() => Promise.resolve(states.slice(coldStates)), value => value.length > 0, 'cold start must receive scoped state')
    assert.equal(await alpha.count(), 0)
    assert.ok(requests.slice(coldStart).filter(url => url.pathname === '/api/v1/state')
      .every(url => url.searchParams.get('online_computers_only') === 'true'), 'cold start must not request all workspaces')
    for (const state of states.slice(coldStates)) {
      assert.deepEqual((await state.body).workspaces.map(workspace => workspace.node_id), ['beta'])
    }
    await until(() => Promise.resolve(sockets.slice(coldSockets)), value => value.length > 0, 'cold start must open scoped Live')
    assert.ok(sockets.slice(coldSockets).every(url => new URL(url).searchParams.get('online_computers_only') === 'true'))

    await options.click()
    await page.getByRole('menuitem', { name: '按工作区', exact: true }).click()
    await page.locator('[data-sidebar-workspace-row]').filter({ hasText: 'Pictures alpha' }).waitFor()
    assert.equal(await page.locator('[data-sidebar-computer-group]').count(), 0)
    await options.click()
    assert.equal(await page.getByRole('menuitemcheckbox', { name: '只显示在线电脑', exact: true }).count(), 0)
    await page.getByRole('menuitem', { name: '按电脑', exact: true }).click()
    await beta.waitFor()
    await alpha.waitFor({ state: 'detached' })
    await options.click()
    await page.getByRole('menuitemcheckbox', { name: '只显示在线电脑', exact: true }).click()
    await alpha.waitFor()
    await beta.waitFor()
    const all = states.findLast(state => !state.url.searchParams.has('online_computers_only'))
    assert.equal((await all.body).workspaces.length, 2)
    assert.equal((await all.body).sessions.length, 2)

    const artifact = process.env.TERNILO_E2E_ARTIFACT_DIR
    if (artifact) {
      await mkdir(artifact, { recursive: true })
      await page.screenshot({ path: path.join(artifact, 'computer-groups-desktop.png'), fullPage: true })
    }
    await page.setViewportSize({ width: 390, height: 844 })
    await page.getByRole('button', { name: '打开侧边栏', exact: true }).click()
    await page.waitForFunction(() => Math.abs(document.querySelector('.app-sidebar').getBoundingClientRect().left) < 1)
    assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth))
    if (artifact) await page.screenshot({ path: path.join(artifact, 'computer-groups-mobile.png'), fullPage: true })
    assert.deepEqual(errors, [])
  } catch (error) {
    throw new Error(`${error.stack}\n${server?.diagnostics() ?? ''}\n${processes.map(process => process.diagnostics()).join('\n')}`)
  } finally {
    await browser?.close()
    for (const process of processes) await stopProcess(process)
    await stopProcess(server)
    await rm(directory, { recursive: true, force: true })
  }
})
