import assert from 'node:assert/strict'
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { chromium } from 'playwright'
import { localApi, until } from './model-device-fixture.mjs'
import { freePort, initializeServer, repository, serverRequest, startProcess, stopProcess, waitForHttp } from './platform-e2e-fixture.mjs'

const row = (page, id) => page.locator(`[data-sidebar-session-row][data-session-id="${id}"]`)
const active = page => page.locator('[data-sidebar-session-active="true"]').evaluate(element => element.closest('[data-session-id]').dataset.sessionId)

async function buttonIsReachable(button, page) {
  const bounds = await button.boundingBox()
  const viewport = page.viewportSize()
  assert.ok(bounds && bounds.x >= 0 && bounds.y >= 0 && bounds.x + bounds.width <= viewport.width && bounds.y + bounds.height <= viewport.height)
  assert.ok(await button.evaluate(element => {
    const r = element.getBoundingClientRect()
    return element.contains(document.elementFromPoint(r.x + r.width / 2, r.y + r.height / 2))
  }), 'dialog action must not be covered')
}

async function scrollSidebarByTouch(page) {
  const tree = page.getByRole('tree')
  if (!await tree.evaluate(element => element.scrollHeight > element.clientHeight)) return
  await tree.evaluate(element => { element.scrollTop = 0 })
  const bounds = await tree.boundingBox()
  assert.ok(bounds && bounds.height > 32)
  const touch = await page.context().newCDPSession(page)
  const x = bounds.x + bounds.width / 2, y = bounds.y + bounds.height - 8
  const distance = Math.min(200, bounds.height - 16)
  try {
    await touch.send('Input.dispatchTouchEvent', { type: 'touchStart', touchPoints: [{ x, y }] })
    for (let step = 1; step <= 8; step += 1) await touch.send('Input.dispatchTouchEvent', { type: 'touchMove', touchPoints: [{ x, y: y - distance * step / 8 }] })
    await touch.send('Input.dispatchTouchEvent', { type: 'touchEnd', touchPoints: [] })
    await page.waitForFunction(() => document.querySelector('[role="tree"]')?.scrollTop > 0)
  } finally { await touch.detach() }
}

for (const placement of ['local', 'server-node']) {
  test(`blank sessions can be reused, explicitly created and removed on desktop and touch (${placement})`, { timeout: 180_000 }, async () => {
    const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-blank-sessions-'))
    const evidence = process.env.TERNILO_E2E_ARTIFACT_DIR ?? directory
    await mkdir(evidence, { recursive: true })
    const processes = []
    let browser, page, readCurrentState
    let uiCreates = 0, rosterReads = 0
    const observations = { pageErrors: [], consoleErrors: [], httpErrors: [] }
    try {
      const environment = {
        ...Object.fromEntries(Object.keys(process.env).filter(key => key.startsWith('TERNILO_')).map(key => [key, undefined])),
        XDG_STATE_HOME: path.join(directory, 'state'),
      }
      const nodeOrigin = `http://127.0.0.1:${await freePort()}`
      const binary = process.env.TERNILO_E2E_NODE_BINARY ?? path.join(repository, 'target/debug/ternilo')
      const nodeArgs = ['serve', '--listen', new URL(nodeOrigin).host, '--data-dir', path.join(directory, 'node')]
      const folder = path.join(directory, 'workspace')
      await mkdir(folder)
      await writeFile(path.join(folder, 'keep.txt'), 'Workspace files must remain after removing blank sessions.\n')
      let origin = nodeOrigin, request, workspace, server
      if (placement === 'server-node') {
        origin = `http://127.0.0.1:${await freePort()}`
        server = await initializeServer({ directory: path.join(directory, 'server'), origin, environment })
        processes.push(server)
        const tenant = server.owner.session.personal_tenant_id
        request = (url, options = {}) => serverRequest(origin, url, { token: server.owner.session.access_token, tenantId: tenant, ...options })
        const project = (await request('/projects')).projects[0]
        const enrollment = (await request(`/tenants/${tenant}/my-computer-enrollments`, { body: { name: 'blank-session-computer', project_id: project.project_id, ttl_seconds: 600 } })).enrollment
        const credential = (await request('/enrollments/consume', { body: { token: enrollment.token } })).credential
        nodeArgs.push('--node-id', enrollment.executor_id, '--gateway-url', `${origin.replace('http:', 'ws:')}/api/v1/executors/connect`, '--allow-insecure-gateway')
        const node = startProcess(binary, nodeArgs, { ...environment, TERNILO_LOCAL_TOKEN: credential.token })
        processes.push(node); await waitForHttp(nodeOrigin, node)
        await until(() => request('/execution-targets'), value => value.executors.some(item => item.executor_id === enrollment.executor_id && item.connected), 'connected Node')
        workspace = (await request('/workspaces', { body: { project_id: project.project_id, name: 'Blank sessions', placement: 'local_node', executor_id: enrollment.executor_id, path: folder } })).workspace
      } else {
        const node = startProcess(binary, nodeArgs, environment)
        processes.push(node); await waitForHttp(nodeOrigin, node)
        request = await localApi(origin)
        workspace = await request('/workspaces', { body: { path: folder } })
      }
      const legacy = []
      for (let index = 0; index < 3; index += 1) legacy.push(await request('/sessions', { body: { workspace_id: workspace.workspace_id } }))
      browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
      page = await browser.newPage({ locale: 'zh-CN', viewport: { width: 1280, height: 800 }, hasTouch: true, serviceWorkers: 'block' })
      page.on('pageerror', error => observations.pageErrors.push(error.message))
      page.on('console', message => { if (message.type() === 'error') observations.consoleErrors.push(message.text()) })
      page.on('response', response => { if (response.status() >= 400) observations.httpErrors.push({ url: response.url(), status: response.status() }) })
      readCurrentState = () => request('/state')
      page.on('request', call => {
        if (call.method() === 'POST' && new URL(call.url()).pathname === '/api/v1/sessions') uiCreates += 1
        if (new URL(call.url()).pathname === '/api/v1/agent-presets' && new URL(call.url()).searchParams.has('workspace_id')) rosterReads += 1
      })
      await page.goto(origin)
      if (server) {
        await page.getByLabel('用户名', { exact: true }).fill(server.owner.username)
        await page.getByLabel('密码', { exact: true }).fill(server.owner.password)
        await page.getByRole('button', { name: '登录', exact: true }).click()
      }
      await row(page, legacy[0].identity.session_id).locator('[data-sidebar-session-button]').click()
      await page.locator('[data-composer-input]').waitFor()
      const rosterBefore = rosterReads
      await page.locator('[data-sidebar-new-session]').evaluate(element => { for (let index = 0; index < 5; index += 1) element.click() })
      const newest = legacy.at(-1).identity.session_id
      await row(page, newest).locator('[data-sidebar-session-active="true"]').waitFor()
      assert.equal(rosterReads - rosterBefore, 1, 'rapid ordinary clicks share one create/reuse operation')
      assert.equal(uiCreates, 0, 'existing unused sessions are reused without POSTing a new session')
      const createAnother = async () => {
        const workspaceRow = page.locator('[data-sidebar-workspace-row]').filter({ has: page.getByText(workspace.name ?? workspace.title, { exact: true }) }).first()
        await workspaceRow.hover()
        await workspaceRow.locator('button[aria-label^="工作区"][aria-label$="的操作"]').click()
        await page.getByRole('menuitem', { name: '另建会话', exact: true }).click()
      }
      const created = page.waitForResponse(response => response.request().method() === 'POST' && new URL(response.url()).pathname === '/api/v1/sessions')
      await createAnother()
      const prepared = await (await created).json()
      const preparedId = prepared.identity.session_id
      await row(page, preparedId).locator('[data-sidebar-session-active="true"]').waitFor()
      assert.equal(uiCreates, 1)
      await page.locator('[data-composer-input]').fill('Keep this prepared task')
      await page.locator('input[type="file"]').setInputFiles({ name: 'prepared.txt', mimeType: 'text/plain', buffer: Buffer.from('unsent attachment') })
      await page.getByRole('button', { name: '移除 prepared.txt' }).waitFor()
      await page.locator('[data-sidebar-new-session]').click()
      await row(page, newest).locator('[data-sidebar-session-active="true"]').waitFor()
      assert.equal(uiCreates, 1, 'drafts are kept while another unused session is reused')
      await row(page, preparedId).locator('[data-sidebar-session-button]').click()
      assert.equal(await page.locator('[data-composer-input]').inputValue(), 'Keep this prepared task')
      await page.getByRole('button', { name: '移除 prepared.txt' }).waitFor()
      const openDelete = async (id, touch = false) => {
        const trigger = row(page, id).locator('button[aria-label^="会话"][aria-label$="的操作"]')
        if (touch) {
          await trigger.scrollIntoViewIfNeeded()
          await buttonIsReachable(trigger, page)
          await trigger.tap()
          await page.getByRole('menuitem', { name: '删除空白会话…', exact: true }).tap()
        } else {
          await row(page, id).hover()
          await trigger.click()
          await page.getByRole('menuitem', { name: '删除空白会话…', exact: true }).click()
        }
        const dialog = page.getByRole('dialog', { name: '删除空白会话…', exact: true })
        await dialog.waitFor()
        await buttonIsReachable(dialog.getByRole('button', { name: '取消', exact: true }), page)
        await buttonIsReachable(dialog.getByRole('button', { name: '删除会话', exact: true }), page)
        return dialog
      }
      let dialog = await openDelete(preparedId)
      assert.match(await dialog.textContent(), /草稿和附件/)
      await dialog.screenshot({ path: path.join(evidence, `blank-session-${placement}-desktop.png`) })
      await page.screenshot({ path: path.join(evidence, `blank-session-${placement}-desktop-page.png`) })
      await dialog.getByRole('button', { name: '取消', exact: true }).click()
      assert.equal(await page.locator('[data-composer-input]').inputValue(), 'Keep this prepared task')
      for (const value of legacy) {
        dialog = await openDelete(value.identity.session_id)
        await dialog.getByRole('button', { name: '删除会话', exact: true }).click()
        await row(page, value.identity.session_id).waitFor({ state: 'detached' })
        await dialog.waitFor({ state: 'detached' })
      }
      const onlyPrepared = (await request('/state')).sessions
      assert.deepEqual(onlyPrepared.map(session => session.identity.session_id), [preparedId])
      const newResponse = page.waitForResponse(response => response.request().method() === 'POST' && new URL(response.url()).pathname === '/api/v1/sessions')
      await page.locator('[data-sidebar-new-session]').click()
      const mobile = await (await newResponse).json()
      await row(page, mobile.identity.session_id).locator('[data-sidebar-session-active="true"]').waitFor()
      assert.equal(uiCreates, 2, 'a prepared draft is never taken over')
      await page.setViewportSize({ width: 320, height: 560 })
      // Selection precedes history loading; wait for the blank view's own sidebar control.
      await page.locator('#conversation-main[data-phase="hero"][data-session-state="empty"]').waitFor()
      const sidebarToggle = page.getByRole('button', { name: '打开侧边栏', exact: true })
      await buttonIsReachable(sidebarToggle, page)
      await sidebarToggle.tap()
      await page.locator('[data-app-sidebar-column]').evaluate(async element => {
        await Promise.all(element.getAnimations({ subtree: true }).filter(animation => animation.effect?.getTiming().iterations !== Infinity).map(animation => animation.finished.catch(() => {})))
      })
      await scrollSidebarByTouch(page)
      dialog = await openDelete(mobile.identity.session_id, true)
      await dialog.screenshot({ path: path.join(evidence, `blank-session-${placement}-mobile.png`) })
      await page.screenshot({ path: path.join(evidence, `blank-session-${placement}-mobile-page.png`) })
      await dialog.getByRole('button', { name: '删除会话', exact: true }).tap()
      await row(page, mobile.identity.session_id).waitFor({ state: 'detached' })
      await dialog.waitFor({ state: 'detached' })
      assert.equal((await request('/state')).sessions.length, 1)
      assert.match(await readFile(path.join(folder, 'keep.txt'), 'utf8'), /Workspace files must remain/)
      assert.deepEqual(observations, { pageErrors: [], consoleErrors: [], httpErrors: [] })
      console.log(JSON.stringify({ placement, reusedLegacy: true, rapidClicksCoalesced: true, explicitNew: true, draftAndAttachmentPreserved: true, deletedLegacy: 3, mobileDelete: true, filesPreserved: true, observations }))
    } catch (error) {
      const current = await readCurrentState?.().catch(() => null)
      console.error(JSON.stringify({ uiCreates, rosterReads, active: page ? await active(page).catch(() => null) : null, sessions: current?.sessions.map(session => ({ id: session.identity.session_id, title: session.title, blank: session.blank, agent_preset: session.agent_preset, permissions: session.permissions, created: session.created_at_ms, updated: session.updated_at_ms, model: session.model })), observations }))
      await page?.screenshot({ path: path.join(evidence, `blank-session-${placement}-failure.png`), fullPage: true }).catch(() => {})
      console.error(processes.map(process => process.diagnostics()).join('\n').slice(-8000))
      throw error
    } finally {
      await browser?.close()
      for (const process of processes.reverse()) await stopProcess(process)
      await rm(directory, { recursive: true, force: true })
    }
  })
}
