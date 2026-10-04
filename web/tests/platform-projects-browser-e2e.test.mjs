import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { chromium } from 'playwright'
import { freePort, initializeServer, repository, serverRequest, startProcess, stopProcess, waitForHttp } from './platform-e2e-fixture.mjs'
import { localApi, until } from './model-device-fixture.mjs'

async function selectSpace(page, tenantId) {
  const trigger = page.getByRole('combobox', { name: '切换空间', exact: true })
  if (await trigger.getAttribute('data-space-id') === tenantId) return
  await trigger.click()
  await page.locator(`[data-space-menu] [role="option"][data-space-id="${tenantId}"]`).click()
  await page.locator(`[data-space-switcher] [role="combobox"][data-space-id="${tenantId}"][aria-busy="false"]:visible`).waitFor()
}

function observeBrowser(page, expectedConflicts = new Set()) {
  const evidence = { errors: [], console: [], responses: [], failedRequests: [] }
  page.on('pageerror', error => evidence.errors.push(error.message))
  page.on('console', message => {
    const location = message.location().url
    evidence.console.push({ type: message.type(), text: message.text(), url: location })
    if (message.type() !== 'error') return
    if (location && expectedConflicts.has(new URL(location).pathname) && /\b409\b/.test(message.text())) return
    evidence.errors.push(`console: ${message.text()}`)
  })
  page.on('response', response => {
    const pathname = new URL(response.url()).pathname
    const method = response.request().method()
    evidence.responses.push({ method, pathname, status: response.status() })
    if (response.status() < 400) return
    if (method === 'DELETE' && response.status() === 409 && expectedConflicts.has(pathname)) return
    evidence.errors.push(`HTTP ${response.status()}: ${method} ${pathname}`)
  })
  page.on('requestfailed', request => evidence.failedRequests.push({
    method: request.method(), pathname: new URL(request.url()).pathname, error: request.failure()?.errorText,
  }))
  return evidence
}

async function verifyAssets(origin) {
  const hashes = {}
  for (const asset of ['app.js', 'app.css']) {
    const response = await fetch(`${origin}/assets/${asset}`)
    assert.equal(response.status, 200)
    const actual = createHash('sha256').update(Buffer.from(await response.arrayBuffer())).digest('hex')
    const expected = createHash('sha256').update(await readFile(path.join(repository, 'web/dist/assets', asset))).digest('hex')
    assert.equal(actual, expected, `${asset} comes from the new production bundle`)
    hashes[asset] = actual
  }
  return hashes
}

async function saveEvidence(artifacts, name, evidence, context, application) {
  await writeFile(path.join(artifacts, `${name}-evidence.json`), `${JSON.stringify(evidence, null, 2)}\n`)
  await writeFile(path.join(artifacts, `${name}-server.log`), application?.diagnostics() ?? '')
  await context?.tracing.stop({ path: path.join(artifacts, `${name}-trace.zip`) })
}

async function settleLayout(page) {
  await page.mouse.move(0, 0)
  await page.evaluate(async () => {
    await new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve)))
    await Promise.all(document.getAnimations().filter(animation => animation.effect?.getTiming().iterations !== Infinity)
      .map(animation => animation.finished.catch(() => {})))
  })
}

async function login(page, origin, credentials) {
  await page.goto(`${origin}/spaces/current?tab=projects`)
  await page.getByLabel('用户名', { exact: true }).fill(credentials.username)
  await page.getByLabel('密码', { exact: true }).fill(credentials.password)
  await page.getByRole('button', { name: '登录', exact: true }).click()
  await page.locator('[data-project-management]').waitFor()
  await page.locator('[data-project-id]').first().waitFor()
}

function projectRow(page, projectId) {
  return page.locator(`[data-project-id="${projectId}"]`)
}

function mutationResponse(page, method, projectId) {
  return page.waitForResponse(response => response.request().method() === method
    && new URL(response.url()).pathname === `/api/v1/projects/${projectId}`)
}

async function renameProject(page, projectId, name) {
  await projectRow(page, projectId).getByRole('button', { name: '重命名项目', exact: true }).click()
  const dialog = page.getByRole('dialog', { name: '重命名项目', exact: true })
  assert.equal(await dialog.getByRole('button', { name: '保存', exact: true }).isDisabled(), true)
  await dialog.getByLabel('项目名称', { exact: true }).fill('   ')
  assert.equal(await dialog.getByRole('button', { name: '保存', exact: true }).isDisabled(), true)
  await dialog.getByLabel('项目名称', { exact: true }).fill(`  ${name}  `)
  const response = mutationResponse(page, 'PATCH', projectId)
  await dialog.getByLabel('项目名称', { exact: true }).press('Enter')
  const completed = await response
  assert.equal(completed.status(), 200)
  assert.equal((await completed.json()).project.name, name)
  await dialog.waitFor({ state: 'hidden' })
  assert.equal(await projectRow(page, projectId).getByRole('heading').textContent(), name)
}

async function deleteProject(page, projectId, expectedStatus) {
  await projectRow(page, projectId).getByRole('button', { name: '删除项目', exact: true }).click()
  const dialog = page.getByRole('dialog', { name: '删除空项目？', exact: true })
  assert.match(await dialog.textContent(), /不会删除磁盘目录、工作区或会话/)
  const response = mutationResponse(page, 'DELETE', projectId)
  await dialog.getByRole('button', { name: '删除项目', exact: true }).click()
  const completed = await response
  assert.equal(completed.status(), expectedStatus)
  if (expectedStatus === 204) {
    await dialog.waitFor({ state: 'hidden' })
    await projectRow(page, projectId).waitFor({ state: 'detached' })
  } else {
    assert.equal((await completed.json()).error.code, 'conflict')
    await dialog.getByRole('alert').waitFor()
    assert.equal(await projectRow(page, projectId).count(), 1)
  }
  return dialog
}

async function noOverflow(page) {
  assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), 'page fits viewport')
  assert.ok(await page.locator('[data-project-management]').evaluate(element => element.scrollWidth <= element.clientWidth), 'project management fits viewport')
  for (const control of await page.locator('[data-project-management] button:visible').all()) {
    const bounds = await control.boundingBox()
    assert.ok(bounds && bounds.x >= 0 && bounds.x + bounds.width <= page.viewportSize().width + 1, 'actions stay within viewport')
  }
}

test('project management preserves resources and enforces roles on desktop and mobile', { timeout: 180_000 }, async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-projects-browser-'))
  const artifacts = process.env.TERNILO_E2E_ARTIFACT_DIR ?? directory
  await mkdir(artifacts, { recursive: true })
  let application, browser, context, page
  let evidence = { errors: [] }
  const expectedConflicts = new Set()
  try {
    const origin = `http://127.0.0.1:${await freePort()}`
    application = await initializeServer({ directory: path.join(directory, 'server'), origin, managedExecutionEnabled: true })
    const assets = await verifyAssets(origin)
    const token = application.owner.session.access_token
    const owner = (resource, options = {}) => serverRequest(origin, resource, { token, ...options })
    const { tenant } = await owner('/tenants', { body: { slug: 'projects-team', display_name: 'Project management' } })
    const tenantId = tenant.tenant_id
    const request = (resource, options = {}) => owner(resource, { tenantId, ...options })
    const { tenant: lastProjectTeam } = await owner('/tenants', { body: { slug: 'last-project-team', display_name: 'Last project' } })
    const [lastProject] = (await owner(`/tenants/${lastProjectTeam.tenant_id}/projects`)).projects
    const { project: editable } = await request('/projects', { body: { name: 'Editable project' } })
    const registration = await owner('/admin/registration')
    await owner('/admin/registration', { method: 'PATCH', body: { mode: 'open', require_approval: false, oidc_only: false, revision: registration.revision } })
    const credentials = { username: 'project-member', email: 'project-member@example.test', password: 'project-member-password' }
    const { session: member } = await serverRequest(origin, '/auth/register', { body: credentials })
    for (const role of [null, 'viewer', 'member', 'admin']) {
      if (role) await owner(`/tenants/${tenantId}/members/${member.user.user_id}`, { method: 'PUT', body: { role } })
      for (const method of ['PATCH', 'DELETE']) {
        const response = await fetch(`${origin}/api/v1/tenants/${tenantId}/projects/${editable.project_id}`, {
          method,
          headers: { authorization: `Bearer ${member.access_token}`, 'content-type': 'application/json' },
          ...(method === 'PATCH' ? { body: JSON.stringify({ name: 'Admin changed' }) } : {}),
        })
        assert.equal(response.status, role === 'admin' ? method === 'PATCH' ? 200 : 204 : 403)
      }
      if (role !== 'admin') assert.deepEqual((await request('/projects')).projects.find(project => project.project_id === editable.project_id), editable)
    }
    const crossTenant = await fetch(`${origin}/api/v1/projects/${member.personal_project_id}`, {
      method: 'DELETE', headers: { authorization: `Bearer ${token}`, 'x-ternilo-tenant': member.personal_tenant_id },
    })
    assert.equal(crossTenant.status, 403)
    await owner(`/tenants/${tenantId}/members/${member.user.user_id}`, { method: 'PUT', body: { role: 'member' } })
    const { project: occupied } = await request('/projects', { body: { name: 'Other member resources' } })
    const memberRequest = (resource, options = {}) => serverRequest(origin, resource, { token: member.access_token, tenantId, ...options })
    const { workspace } = await memberRequest('/workspaces', { body: { project_id: occupied.project_id, name: 'Private workspace', placement: 'cloud' } })
    const session = await memberRequest('/sessions', { body: { workspace_id: workspace.workspace_id } })
    const config = JSON.parse(await readFile(application.configPath, 'utf8'))
    const sentinelPath = path.join(config.workspace_root, 'project-deletion-sentinel.txt')
    await mkdir(config.workspace_root, { recursive: true })
    await writeFile(sentinelPath, 'preserve workspace files')
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
    context = await browser.newContext({ locale: 'zh-CN', viewport: { width: 1440, height: 1000 }, hasTouch: true, serviceWorkers: 'block' })
    await context.tracing.start({ screenshots: true, snapshots: true })
    page = await context.newPage()
    evidence = { ...observeBrowser(page, expectedConflicts), assets }
    await login(page, origin, application.owner)
    const defaultProject = application.owner.session.personal_project_id
    expectedConflicts.add(`/api/v1/projects/${defaultProject}`)
    let dialog = await deleteProject(page, defaultProject, 409)
    assert.match(await dialog.getByRole('alert').textContent(), /个人默认项目不能删除/)
    await dialog.getByRole('button', { name: '取消', exact: true }).click()
    await selectSpace(page, lastProjectTeam.tenant_id)
    await projectRow(page, lastProject.project_id).waitFor()
    expectedConflicts.add(`/api/v1/projects/${lastProject.project_id}`)
    dialog = await deleteProject(page, lastProject.project_id, 409)
    assert.match(await dialog.getByRole('alert').textContent(), /空间必须保留至少一个项目/)
    await dialog.getByRole('button', { name: '取消', exact: true }).click()
    await selectSpace(page, tenantId)
    await page.locator('[data-project-management]').waitFor()
    await projectRow(page, occupied.project_id).waitFor()
    expectedConflicts.add(`/api/v1/projects/${occupied.project_id}`)
    dialog = await deleteProject(page, occupied.project_id, 409)
    assert.match(await dialog.getByRole('alert').textContent(), /项目仍有关联资源/)
    await dialog.getByRole('button', { name: '取消', exact: true }).click()
    assert.ok((await memberRequest('/state')).sessions.some(candidate => candidate.identity.session_id === session.identity.session_id))
    assert.equal((await request('/workspaces')).workspaces.find(candidate => candidate.workspace_id === workspace.workspace_id).project_id, occupied.project_id)

    for (const width of [1440, 390, 320]) {
      await page.setViewportSize({ width, height: 960 })
      const name = `Project-${width}-${'long'.repeat(25)}`
      await page.locator('[data-project-management]').getByLabel('项目名称', { exact: true }).fill(name)
      const createdResponse = page.waitForResponse(response => response.request().method() === 'POST' && new URL(response.url()).pathname === '/api/v1/projects')
      await page.getByRole('button', { name: '新建项目', exact: true }).click()
      const created = await createdResponse
      assert.equal(created.status(), 201)
      const { project } = await created.json()
      await projectRow(page, project.project_id).waitFor()
      await noOverflow(page)
      await projectRow(page, project.project_id).getByRole('button', { name: '删除项目', exact: true }).click()
      dialog = page.getByRole('dialog', { name: '删除空项目？', exact: true })
      await settleLayout(page)
      await page.screenshot({ path: path.join(artifacts, `projects-long-name-${width}.png`) })
      assert.ok(await dialog.evaluate(element => element.scrollWidth <= element.clientWidth), 'long project names must fit the confirmation dialog')
      await dialog.getByRole('button', { name: '取消', exact: true }).click()
      await projectRow(page, project.project_id).getByRole('button', { name: '重命名项目', exact: true }).click()
      dialog = page.getByRole('dialog', { name: '重命名项目', exact: true })
      await dialog.getByLabel('项目名称', { exact: true }).fill('Discarded name')
      await dialog.getByRole('button', { name: '取消', exact: true }).click()
      assert.equal((await request('/projects')).projects.find(candidate => candidate.project_id === project.project_id).name, name)
      await renameProject(page, project.project_id, `Renamed-${width}`)
      await page.reload()
      await projectRow(page, project.project_id).waitFor()
      assert.equal(await projectRow(page, project.project_id).getByRole('heading').textContent(), `Renamed-${width}`)
      await projectRow(page, project.project_id).getByRole('button', { name: '删除项目', exact: true }).tap()
      dialog = page.getByRole('dialog', { name: '删除空项目？', exact: true })
      await dialog.getByRole('button', { name: '取消', exact: true }).tap()
      assert.ok((await request('/projects')).projects.some(candidate => candidate.project_id === project.project_id))
      await deleteProject(page, project.project_id, 204)
      assert.ok(!(await request('/projects')).projects.some(candidate => candidate.project_id === project.project_id))
      assert.equal(await readFile(sentinelPath, 'utf8'), 'preserve workspace files')
      await page.screenshot({ path: path.join(artifacts, `projects-${width}.png`) })
    }
    await page.evaluate(() => localStorage.setItem('ternilo.locale', 'en'))
    await page.reload()
    await projectRow(page, occupied.project_id).getByRole('button', { name: 'Delete project', exact: true }).click()
    dialog = page.getByRole('dialog', { name: 'Delete empty project?', exact: true })
    const conflict = mutationResponse(page, 'DELETE', occupied.project_id)
    await dialog.getByRole('button', { name: 'Delete project', exact: true }).click()
    assert.equal((await conflict).status(), 409)
    await dialog.getByRole('alert').waitFor()
    assert.match(await dialog.getByRole('alert').textContent(), /No resources were deleted or moved/)
    assert.doesNotMatch(await dialog.textContent(), /[\u3400-\u9fff]/)
    await page.screenshot({ path: path.join(artifacts, 'projects-english-conflict.png') })
    assert.deepEqual(evidence.errors, [])
  } catch (error) {
    await page?.screenshot({ path: path.join(artifacts, 'projects-failure.png') })
    error.message += `\n${evidence.errors.join('\n')}\n${application?.diagnostics() ?? ''}`
    throw error
  } finally {
    try { await saveEvidence(artifacts, 'projects', evidence, context, application) }
    finally {
      await browser?.close()
      await stopProcess(application)
      await rm(directory, { recursive: true, force: true })
    }
  }
})

test('returning to the same session after project rename updates its title location', { timeout: 180_000 }, async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-project-title-browser-'))
  const artifacts = process.env.TERNILO_E2E_ARTIFACT_DIR ?? directory
  await mkdir(artifacts, { recursive: true })
  let application, node, browser, context, page
  let evidence = { errors: [] }
  try {
    const origin = `http://127.0.0.1:${await freePort()}`
    application = await initializeServer({ directory: path.join(directory, 'server'), origin })
    const assets = await verifyAssets(origin)
    const token = application.owner.session.access_token
    const tenantId = application.owner.session.personal_tenant_id
    const request = (resource, options = {}) => serverRequest(origin, resource, { token, tenantId, ...options })
    const { project } = await request('/projects', { body: { name: 'Title project before' } })
    const enrolled = await request(`/tenants/${tenantId}/my-computer-enrollments`, {
      body: { name: 'project-title-node', project_id: project.project_id, ttl_seconds: 600 },
    })
    const credential = await request('/enrollments/consume', { body: { token: enrolled.enrollment.token } })
    const nodeOrigin = `http://127.0.0.1:${await freePort()}`
    const binary = process.env.TERNILO_E2E_NODE_BINARY ?? path.join(repository, 'target/debug/ternilo')
    node = startProcess(binary, ['serve', '--listen', new URL(nodeOrigin).host, '--data-dir', path.join(directory, 'node'),
      '--node-id', enrolled.enrollment.executor_id, '--gateway-url', `${origin.replace('http:', 'ws:')}/api/v1/executors/connect`, '--allow-insecure-gateway'], {
      TERNILO_LOCAL_TOKEN: credential.credential.token, XDG_STATE_HOME: path.join(directory, 'state'),
    })
    await waitForHttp(nodeOrigin, node)
    const local = await localApi(nodeOrigin)
    const folder = path.join(directory, 'workspace')
    await mkdir(folder)
    await writeFile(path.join(folder, 'proof.txt'), 'Project rename keeps this file.')
    const workspace = await local('/workspaces', { body: { path: folder } })
    const localSession = await local('/sessions', { body: { workspace_id: workspace.workspace_id } })
    await local(`/sessions/${localSession.identity.session_id}/queue`, { body: { content: { kind: 'prompt', input: '/glob *' } } })
    await until(() => local(`/sessions/${localSession.identity.session_id}/queue`), queue => !queue.active_run_id && !queue.items.some(item => item.placement === 'queued'), 'real local command completion')
    const snapshot = await until(() => request('/state'), state => state.sessions.length === 1 && !state.sessions[0].blank, 'nonblank session synchronized')
    const sessionId = snapshot.sessions[0].identity.session_id
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
    context = await browser.newContext({ locale: 'zh-CN', viewport: { width: 1440, height: 960 }, hasTouch: true, serviceWorkers: 'block' })
    await context.tracing.start({ screenshots: true, snapshots: true })
    page = await context.newPage()
    evidence = { ...observeBrowser(page), assets, sessionId, titleChecks: [] }
    await login(page, origin, application.owner)
    await page.getByRole('link', { name: '返回工作台', exact: true }).click()
    const sessionRow = page.locator(`[data-sidebar-session-row][data-session-id="${sessionId}"]`)
    await sessionRow.locator('[data-sidebar-session-button]').click()
    const location = page.locator('[data-session-workspace-context]:visible')
    await location.locator('[data-session-project]').filter({ hasText: 'Title project before' }).waitFor()
    await settleLayout(page)
    await page.screenshot({ path: path.join(artifacts, 'project-title-before.png') })
    let before = 'Title project before'
    for (const width of [1440, 390]) {
      await page.setViewportSize({ width, height: 960 })
      await settleLayout(page)
      if (width < 768) await page.getByRole('button', { name: '打开侧边栏', exact: true }).click()
      await page.getByRole('button', { name: '空间管理', exact: true }).click()
      await page.getByRole('tab', { name: '项目', exact: true }).click()
      const after = `Title project after ${width}`
      await renameProject(page, project.project_id, after)
      const requestsBeforeReturn = evidence.responses.filter(response => response.pathname === '/api/v1/projects' && response.method === 'GET').length
      await page.getByRole('link', { name: '返回工作台', exact: true }).click()
      await location.waitFor()
      await location.locator('[data-session-project]').filter({ hasText: after }).waitFor({ timeout: 10_000 })
      assert.equal(await location.locator('[data-session-project]').textContent(), ` · ${after}`)
      assert.equal(await sessionRow.getAttribute('aria-selected'), 'true', 'the original session stays selected')
      await settleLayout(page)
      await location.locator('[data-session-project]').scrollIntoViewIfNeeded()
      evidence.titleChecks.push({ before, after, width, projectReadsAfterReturn: evidence.responses.filter(response => response.pathname === '/api/v1/projects' && response.method === 'GET').length - requestsBeforeReturn })
      await page.screenshot({ path: path.join(artifacts, `project-title-after-${width}.png`) })
      before = after
    }
    assert.equal(await readFile(path.join(folder, 'proof.txt'), 'utf8'), 'Project rename keeps this file.')
    assert.deepEqual(evidence.errors, [])
  } catch (error) {
    await page?.screenshot({ path: path.join(artifacts, 'project-title-failure.png') })
    error.message += `\n${evidence.errors.join('\n')}\n${application?.diagnostics() ?? ''}\n${node?.diagnostics() ?? ''}`
    throw error
  } finally {
    try {
      await saveEvidence(artifacts, 'project-title', evidence, context, application)
      await writeFile(path.join(artifacts, 'project-title-node.log'), node?.diagnostics() ?? '')
    } finally {
      await browser?.close()
      await stopProcess(node)
      await stopProcess(application)
      await rm(directory, { recursive: true, force: true })
    }
  }
})
