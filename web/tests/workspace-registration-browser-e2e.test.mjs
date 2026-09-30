import assert from 'node:assert/strict'
import { mkdir, mkdtemp, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { chromium } from 'playwright'
import { initializeServer, freePort, repository, serverRequest, selectSpace, startProcess, stopProcess, waitForHttp } from './platform-e2e-fixture.mjs'
import { localApi, until } from './model-device-fixture.mjs'
import { selectChoice } from './browser-select-fixture.mjs'

test('another computer can open an offline computer’s same-named folder without polling or an internal error', { timeout: 120_000 }, async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-workspace-registration-'))
  const artifacts = process.env.TERNILO_E2E_ARTIFACT_DIR ?? directory
  await mkdir(artifacts, { recursive: true })
  const processes = [], errors = [], listRequests = []
  let browser, page, oldSessionId
  const expectedFailure = (url, status, method = 'GET') => {
    const resource = new URL(url)
    if (status === 409 && method === 'POST' && resource.pathname === '/api/v1/workspaces') return true
    return status === 503 && oldSessionId && (resource.pathname.includes(`/sessions/${oldSessionId}/`) || resource.searchParams.get('session_id') === oldSessionId)
  }
  try {
    const server = await initializeServer({ directory: path.join(directory, 'server'), origin: `http://127.0.0.1:${await freePort()}` })
    processes.push(server)
    const account = (resource, options = {}) => serverRequest(server.origin, resource, { token: server.owner.session.access_token, ...options })
    const tenant = (await account('/tenants', { body: { slug: 'same-folders', display_name: 'Folder test' } })).tenant
    const request = (resource, options = {}) => account(resource, { tenantId: tenant.tenant_id, ...options })
    const project = (await request('/projects')).projects[0]
    const startNode = async id => {
      const enrollment = (await request(`/tenants/${tenant.tenant_id}/my-computer-enrollments`, { body: { executor_id: id, project_id: project.project_id, ttl_seconds: 600 } })).enrollment
      const credential = (await request('/enrollments/consume', { body: { token: enrollment.token } })).credential
      assert.match(credential.token, /^ter_n_/)
      const origin = `http://127.0.0.1:${await freePort()}`
      const node = startProcess(process.env.TERNILO_E2E_NODE_BINARY ?? path.join(repository, 'target/debug/ternilo'), [
        'serve', '--listen', new URL(origin).host, '--data-dir', path.join(directory, id), `--node-id=${id}`,
        '--gateway-url', `${server.origin.replace('http:', 'ws:')}/api/v1/executors/connect`, '--token', credential.token, '--allow-insecure-gateway',
      ])
      processes.push(node)
      await waitForHttp(origin, node)
      return { node, local: await localApi(origin) }
    }
    const folder = path.join(directory, 'Pictures')
    await mkdir(folder)
    const old = await startNode('old-computer')
    const workspace = await old.local('/workspaces', { body: { path: folder } })
    await old.local('/sessions', { body: { workspace_id: workspace.workspace_id } })
    const initial = await until(() => request('/state'), state => state.sessions.length === 1, 'first computer discovery')
    oldSessionId = initial.sessions[0].identity.session_id
    const oldWorkspaceId = initial.workspaces[0].workspace_id
    await stopProcess(old.node)
    await until(() => request('/state'), state => state.workspaces[0].status === 'offline', 'first computer offline')
    await startNode('d')
    await until(() => request('/execution-targets'), value => value.executors.some(node => node.executor_id === 'd' && node.connected), 'second computer online')
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
    page = await browser.newPage({ viewport: { width: 1280, height: 960 }, serviceWorkers: 'block' })
    page.on('pageerror', error => errors.push(error.message))
    page.on('console', message => {
      if (message.type() !== 'error') return
      const status = Number(message.text().match(/status of (\d+)/)?.[1])
      if (!expectedFailure(message.location().url || server.origin, status, status === 409 ? 'POST' : 'GET')) errors.push(message.text())
    })
    page.on('response', response => {
      if (response.status() >= 400 && !expectedFailure(response.url(), response.status(), response.request().method())) errors.push(`${response.status()} ${response.url()}`)
    })
    page.on('request', request => {
      if (request.method() === 'GET' && ['/api/v1/projects', '/api/v1/execution-targets'].includes(new URL(request.url()).pathname)) listRequests.push(request.url())
    })
    await page.goto(server.origin)
    await page.getByLabel('用户名', { exact: true }).fill(server.owner.username)
    await page.getByLabel('密码', { exact: true }).fill(server.owner.password)
    await page.getByRole('button', { name: '登录', exact: true }).click()
    await selectSpace(page, tenant.tenant_id)
    const chooseFolder = async () => {
      await page.getByRole('button', { name: '添加工作区', exact: true }).click()
      let dialog = page.getByRole('dialog', { name: '打开工作区', exact: true })
      await selectChoice(dialog.locator('#workspace-executor'), 'd')
      await dialog.getByRole('button', { name: '选择文件夹', exact: true }).click()
      const picker = page.getByRole('dialog', { name: '选择 d 上的工作文件夹', exact: true })
      await picker.getByRole('button', { name: '编辑文件夹路径', exact: true }).click()
      const editor = picker.getByRole('textbox', { name: '编辑文件夹路径', exact: true })
      await editor.fill(folder)
      await editor.press('Enter')
      await picker.getByRole('list', { name: `目录 ${folder}`, exact: true }).waitFor()
      await picker.getByRole('button', { name: '选择此文件夹', exact: true }).click()
      dialog = page.getByRole('dialog', { name: '打开工作区', exact: true })
      return dialog
    }
    let dialog = await chooseFolder()
    const name = dialog.locator('#workspace-name')
    assert.equal(await name.inputValue(), 'Pictures (d)')
    const beforeIdle = listRequests.length
    await page.waitForTimeout(3_500)
    assert.equal(listRequests.length, beforeIdle, 'an idle dialog does not poll catalogs')
    await name.fill('Pictures')
    const rejected = page.waitForResponse(response => response.request().method() === 'POST' && new URL(response.url()).pathname === '/api/v1/workspaces')
    await dialog.getByRole('button', { name: '打开并开始会话', exact: true }).click()
    const failure = await rejected
    assert.equal(failure.status(), 409)
    assert.equal((await failure.json()).error.code, 'conflict')
    await dialog.getByText('这个项目中已有同名工作区，请修改工作区名称。', { exact: true }).waitFor()
    const beforeErrorIdle = listRequests.length
    await page.waitForTimeout(3_500)
    assert.equal(listRequests.length, beforeErrorIdle, 'a failed submission does not restart polling')
    await name.fill('Pictures (d)')
    await dialog.getByRole('button', { name: '打开并开始会话', exact: true }).click()
    await dialog.waitFor({ state: 'hidden' })
    const current = await until(() => request('/state'), state => state.sessions.some(session => state.workspaces.some(workspace => workspace.node_id === 'd' && workspace.workspace_id === session.workspace_id)), 'new computer session created')
    assert.ok(current.workspaces.some(workspace => workspace.workspace_id === oldWorkspaceId && workspace.status === 'offline' && workspace.title === 'Pictures'))
    assert.ok(current.workspaces.some(workspace => workspace.node_id === 'd' && workspace.title === 'Pictures (d)' && workspace.workspace_id !== oldWorkspaceId))
    const newWorkspace = current.workspaces.find(workspace => workspace.node_id === 'd')
    dialog = await chooseFolder()
    assert.equal(await dialog.locator('#workspace-name').inputValue(), 'Pictures (d)', 'reopening the same computer’s folder keeps its name')
    await dialog.getByRole('button', { name: '打开并开始会话', exact: true }).click()
    await dialog.waitFor({ state: 'hidden' })
    const reopened = await until(() => request('/state'), state => state.sessions.length === 3, 'same folder reopened')
    assert.equal(reopened.workspaces.length, 2)
    assert.equal(reopened.workspaces.find(workspace => workspace.workspace_id === newWorkspace.workspace_id).title, 'Pictures (d)')
    await page.screenshot({ path: path.join(artifacts, 'workspace-registration.png') })
    assert.deepEqual(errors, [])
  } catch (error) {
    await page?.screenshot({ path: path.join(artifacts, 'workspace-registration-failure.png') }).catch(() => {})
    throw error
  } finally {
    await browser?.close()
    for (const process of processes.reverse()) await stopProcess(process)
    await rm(directory, { recursive: true, force: true })
  }
})
