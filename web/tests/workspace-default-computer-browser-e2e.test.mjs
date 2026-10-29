import assert from 'node:assert/strict'
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { chromium } from 'playwright'
import { selectChoice } from './browser-select-fixture.mjs'
import {
  freePort, initializeServer, repository, selectSpace, serverRequest, startProcess, stopProcess, waitForHttp,
} from './platform-e2e-fixture.mjs'

const labels = {
  zh: { add: '添加工作区', title: '打开工作区', computer: '运行电脑', cancel: '取消' },
  en: { add: 'Add workspace', title: 'Open a workspace', computer: 'Computer', cancel: 'Cancel' },
  ko: { add: '작업 공간 추가', title: '작업 공간 열기', computer: '컴퓨터', cancel: '취소' },
}

async function until(read, accepts, description) {
  const deadline = Date.now() + 20_000
  let latest
  while (Date.now() < deadline) {
    latest = await read()
    if (accepts(latest)) return latest
    await new Promise(resolve => setTimeout(resolve, 100))
  }
  throw new Error(`${description}: ${JSON.stringify(latest)}`)
}

async function isolatedEnvironment(directory) {
  const environment = {
    ...Object.fromEntries(Object.keys(process.env).filter(key => key.startsWith('TERNILO_')).map(key => [key, undefined])),
    HOME: path.join(directory, 'home'),
    XDG_STATE_HOME: path.join(directory, 'state'),
    XDG_CONFIG_HOME: path.join(directory, 'config'),
    XDG_CACHE_HOME: path.join(directory, 'cache'),
  }
  await Promise.all(['HOME', 'XDG_STATE_HOME', 'XDG_CONFIG_HOME', 'XDG_CACHE_HOME']
    .map(key => mkdir(environment[key], { recursive: true })))
  return environment
}

async function verifyEmbeddedAssets(origin) {
  for (const asset of ['app.js', 'app.css']) {
    const response = await fetch(`${origin}/assets/${asset}`)
    assert.equal(response.status, 200)
    const served = Buffer.from(await response.arrayBuffer())
    const built = await readFile(path.join(repository, 'web/dist/assets', asset))
    assert.ok(served.equals(built), `Server must embed the current ${asset}; rebuild the Server after building Web`)
  }
}

async function openPicker(page, locale = 'zh') {
  const translated = labels[locale]
  const targets = page.waitForResponse(response => new URL(response.url()).pathname === '/api/v1/execution-targets')
  await page.getByRole('button', { name: translated.add, exact: true }).click()
  const dialog = page.getByRole('dialog', { name: translated.title, exact: true })
  await dialog.waitFor()
  assert.equal((await targets).status(), 200)
  await until(() => dialog.getAttribute('aria-busy'), busy => busy !== 'true', 'workspace choices finish loading')
  return dialog
}

async function closePicker(dialog, locale = 'zh') {
  await dialog.getByRole('button', { name: labels[locale].cancel, exact: true }).click()
  await dialog.waitFor({ state: 'detached' })
}

async function chooseComputer(page, value) {
  const picker = page.locator('[data-sidebar-computer-picker]')
  await selectChoice(picker, value)
  await page.locator('[data-choice-menu]').waitFor({ state: 'detached' })
  assert.equal(await picker.getAttribute('data-choice-value'), value)
}

async function login(page, credentials) {
  await page.getByLabel('用户名', { exact: true }).fill(credentials.username)
  await page.getByLabel('密码', { exact: true }).fill(credentials.password)
  await page.getByRole('button', { name: '登录', exact: true }).click()
  await page.getByRole('button', { name: '添加工作区', exact: true }).waitFor()
}

async function selectBetaSession(page, beta) {
  await chooseComputer(page, 'all')
  const group = page.locator(`[data-sidebar-computer-group="node:${beta.id}"]`)
  const computer = group.locator('[data-sidebar-computer-button]')
  if (await computer.getAttribute('aria-expanded') === 'false') await computer.click()
  const workspace = group.locator('[data-sidebar-workspace-button]').filter({ hasText: 'Existing beta' })
  if (await workspace.getAttribute('aria-expanded') !== 'true') await workspace.click()
  await group.locator(`[data-session-id="${beta.sessionId}"] [data-sidebar-session-button]`).click()
  await page.waitForFunction(id => localStorage.getItem('ternilo.current-session') === id, beta.sessionId)
}

async function openFolder(page, dialog, node, folder, origin, scope) {
  const listed = page.waitForResponse(response => new URL(response.url()).pathname === `/api/v1/executors/${node.id}/directories`)
  await dialog.getByRole('button', { name: '选择文件夹', exact: true }).click()
  const directory = page.getByRole('dialog', { name: `选择 ${node.name} 上的工作文件夹`, exact: true })
  await directory.waitFor()
  assert.equal((await listed).status(), 200, 'directory browsing uses the chosen real Node')
  await directory.getByRole('button', { name: '编辑文件夹路径', exact: true }).click()
  const editor = directory.getByRole('textbox', { name: '编辑文件夹路径', exact: true })
  await editor.fill(folder)
  await editor.press('Enter')
  await directory.getByRole('list', { name: `目录 ${folder}`, exact: true }).waitFor()
  await directory.getByRole('button', { name: '选择此文件夹', exact: true }).click()
  await dialog.waitFor()
  assert.equal(await dialog.locator('[data-selected-directory]').textContent(), folder)
  const created = page.waitForResponse(response => response.request().method() === 'POST'
    && new URL(response.url()).pathname === '/api/v1/workspaces')
  await dialog.getByRole('button', { name: '打开并开始会话', exact: true }).click()
  const response = await created
  assert.ok(response.ok(), `workspace creation returns ${response.status()}`)
  const submitted = response.request().postDataJSON()
  assert.equal(submitted.executor_id, node.id)
  assert.equal(submitted.placement, 'local_node')
  assert.equal(submitted.path, folder)
  const { workspace } = await response.json()
  await dialog.waitFor({ state: 'detached' })
  const state = await serverRequest(origin, '/state', scope)
  const registered = state.workspaces.find(item => item.workspace_id === workspace.workspace_id)
  assert.ok(registered, 'the created workspace is present in the real Server state')
  assert.equal(registered.node_id, node.id)
  const location = await serverRequest(origin, `/workspaces/${workspace.workspace_id}/location`, scope)
  assert.equal(location.path, folder)
  await page.waitForFunction(id => localStorage.getItem('ternilo.current-workspace') === id, workspace.workspace_id)
  return { workspaceId: workspace.workspace_id, nodeId: registered.node_id, path: location.path }
}

async function verifyIndependentLoading(page, beta, alpha, artifacts, evidence) {
  // Pause only the fixture-owned process; it remains connected but cannot answer RPCs.
  assert.equal(beta.process.child.kill('SIGSTOP'), true)
  try {
    const requested = page.waitForRequest(request => new URL(request.url()).pathname === '/api/v1/catalog')
    const index = page.waitForResponse(response => new URL(response.url()).pathname === '/api/v1/state')
    const started = Date.now()
    await page.reload({ waitUntil: 'domcontentloaded' })
    const response = await index
    assert.equal(response.status(), 200)
    assert.equal(new URL(response.url()).searchParams.get('refresh_computers'), 'false')
    await requested
    await page.getByRole('button', { name: '用户设置', exact: true }).click({ timeout: 3000 })
    await page.getByRole('button', { name: '返回工作台', exact: true }).click()
    assert.equal(await page.evaluate(() => localStorage.getItem('ternilo.current-session')), beta.sessionId)
    await page.locator('[data-target-configuration="loading"], [data-history-state="loading"]').first().waitFor()
    if (artifacts) await page.screenshot({ path: path.join(artifacts, 'server-loading-slow-computer.png'), fullPage: true, animations: 'disabled' })
    await chooseComputer(page, `node:${alpha.id}`)
    const workspace = page.locator('[data-sidebar-workspace-button]').filter({ hasText: 'Existing alpha' })
    await workspace.click({ timeout: 3000 })
    await page.waitForFunction(id => localStorage.getItem('ternilo.current-workspace') === id, alpha.workspace.workspace_id)
    assert.ok(Date.now() - started < 8000, 'Server navigation and switching must not wait for a paused computer')
    evidence.checks.push('paused selected Node cannot block Server index, settings, or switching to another computer')
  } finally { beta.process.child.kill('SIGCONT') }
  await page.locator('[data-target-configuration="loading"]').waitFor({ state: 'detached' })
  assert.equal(await page.evaluate(() => localStorage.getItem('ternilo.current-workspace')), alpha.workspace.workspace_id,
    'late computer responses must not restore the old selection')
}

async function verifyConfigurationRetry(page, beta, errors, artifacts, evidence) {
  await selectBetaSession(page, beta)
  let failed = false
  const failuresFrom = errors.length
  const pattern = '**/api/v1/catalog?*'
  await page.route(pattern, async route => {
    if (!failed && new URL(route.request().url()).searchParams.get('session_id') === beta.sessionId) {
      failed = true
      await route.fulfill({ status: 503, json: { error: { code: 'unavailable', message: 'fixture computer timed out' } } })
    } else await route.continue()
  })
  try {
    await page.reload()
    const notice = page.locator('[data-target-configuration="error"]')
    await notice.waitFor()
    assert.ok((await notice.textContent()).includes('请重试，或切换到其他电脑'))
    assert.equal(await page.locator('[data-offline-shell]').count(), 0)
    if (artifacts) await page.screenshot({ path: path.join(artifacts, 'server-configuration-retry.png'), fullPage: true, animations: 'disabled' })
    await notice.getByRole('button', { name: '重试配置', exact: true }).click()
    await page.locator('[data-target-configuration]').waitFor({ state: 'detached' })
    assert.equal(failed, true)
    const expected = errors.splice(failuresFrom)
    assert.ok(expected.includes('HTTP 503: /api/v1/catalog'))
    assert.ok(expected.every(message => message === 'HTTP 503: /api/v1/catalog' || message.startsWith('Console: Failed to load resource: the server responded with a status of 503')),
      `unexpected errors during injected failure: ${expected.join(' | ')}`)
    evidence.checks.push('a selected computer configuration failure is local and retry restores it without reloading Server')
  } finally { await page.unroute(pattern) }
}

async function verifyVersionSummaries(page, alpha, beta, scope, artifacts, evidence) {
  const origin = new URL(page.url()).origin
  const snapshots = Object.fromEntries(await Promise.all([alpha, beta].map(async computer => [computer.id,
    await serverRequest(origin, `/tenants/${scope.tenantId}/my-computers/${computer.id}/updates`, scope),
  ])))
  const actual = []
  const pattern = '**/api/v1/tenants/*/my-computers/*/updates*'
  await page.route(pattern, async route => {
    const isAlpha = route.request().url().includes(`/${alpha.id}/`)
    const status = snapshots[isAlpha ? alpha.id : beta.id]
    actual.push(new URL(route.request().url()).pathname)
    // Exercise both display outcomes without installing an older binary or starting an update.
    await route.fulfill({ json: { ...status, availability: isAlpha ? 'available' : 'current',
      info: { ...status.info, version: isAlpha ? '0.2.22' : status.target_version } } })
  })
  const translated = {
    zh: { arrow: '可更新，任务结束后安装', check: '已与 Server 配套，无需更新', action: '更新客户端', close: '关闭' },
    en: { arrow: 'Update available; installs after tasks finish', check: 'Matches Server; no update needed', action: 'Update client', close: 'Close' },
    ko: { arrow: '업데이트 가능 · 작업 종료 후 설치', check: 'Server와 버전이 일치하여 업데이트가 필요하지 않습니다', action: '클라이언트 업데이트', close: '닫기' },
  }
  try {
    for (const locale of ['zh', 'en', 'ko']) {
      await page.evaluate(locale => localStorage.setItem('ternilo.locale', locale), locale)
      await page.goto(`${new URL(page.url()).origin}/settings/computers`)
      const cardA = page.locator(`[data-platform-computer="${alpha.id}"]`)
      const cardB = page.locator(`[data-platform-computer="${beta.id}"]`)
      await cardA.locator('[data-computer-version-state="available"]').waitFor()
      await cardB.locator('[data-computer-version-state="current"]').waitFor()
      assert.equal(await cardA.locator('[data-computer-version-state] svg').getAttribute('aria-label'), translated[locale].arrow)
      assert.equal(await cardB.locator('[data-computer-version-state] svg').getAttribute('aria-label'), translated[locale].check)
      assert.ok(!(await cardB.locator('[data-computer-version-state]').textContent()).includes(translated[locale].check))
      if (artifacts) await page.screenshot({ path: path.join(artifacts, `computer-versions-${locale}.png`), fullPage: true, animations: 'disabled' })
      await cardA.getByRole('button', { name: translated[locale].action, exact: true }).click()
      const dialog = page.locator('[data-computer-updates]')
      await dialog.locator('[data-computer-version-state="available"]').waitFor()
      assert.equal(await dialog.locator('[data-computer-version-state]').textContent(), await cardA.locator('[data-computer-version-state]').textContent())
      if (artifacts) await page.screenshot({ path: path.join(artifacts, `computer-update-${locale}.png`), fullPage: true, animations: 'disabled' })
      await dialog.locator('[data-slot="dialog-footer"]').getByRole('button', { name: translated[locale].close, exact: true }).click()
    }
    await page.setViewportSize({ width: 390, height: 844 })
    await page.evaluate(() => { localStorage.setItem('ternilo.theme', 'dark') })
    await page.reload()
    await page.locator('[data-computer-version-state="current"]').waitFor()
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true)
    const narrowRow = page.locator(`[data-platform-computer="${beta.id}"]`)
    await narrowRow.scrollIntoViewIfNeeded()
    assert.equal(await narrowRow.evaluate(element => element.scrollWidth <= element.clientWidth), true)
    if (artifacts) await page.screenshot({ path: path.join(artifacts, 'computer-versions-mobile-dark.png'), fullPage: true, animations: 'disabled' })
    await narrowRow.getByRole('button', { name: translated.ko.action, exact: true }).click()
    const narrowDialog = page.locator('[data-computer-updates]')
    await narrowDialog.locator('[data-computer-version-state="current"]').waitFor()
    const footer = await narrowDialog.locator('[data-slot="dialog-footer"]').boundingBox()
    assert.ok(footer && footer.y >= 0 && footer.y + footer.height <= 844)
    assert.equal(await narrowDialog.evaluate(element => element.scrollWidth <= element.clientWidth), true)
    if (artifacts) await page.screenshot({ path: path.join(artifacts, 'computer-update-mobile-dark.png'), fullPage: true, animations: 'disabled' })
    await narrowDialog.locator('[data-slot="dialog-footer"]').getByRole('button', { name: translated.ko.close, exact: true }).click()
    assert.ok(actual.length > 0)
    evidence.checks.push('version summaries and update dialog share check/arrow presentation in Chinese, English and Korean, including narrow dark layout')
  } finally {
    await page.unrouteAll({ behavior: 'wait' })
    await page.setViewportSize({ width: 1365, height: 900 })
    await page.evaluate(() => { localStorage.setItem('ternilo.locale', 'zh'); localStorage.setItem('ternilo.theme', 'light') })
    await page.goto(new URL(page.url()).origin)
  }
}

test('Server loading, computer defaults and update summaries remain independently usable', { timeout: 240_000 }, async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-workspace-default-computer-'))
  const artifacts = process.env.TERNILO_E2E_ARTIFACT_DIR
  const processes = []
  const errors = []
  const requests = []
  const evidence = { checks: [], workspaces: [], errors }
  let server, browser, page
  try {
    if (artifacts) await mkdir(artifacts, { recursive: true })
    const origin = `http://127.0.0.1:${await freePort()}`
    server = await initializeServer({ directory: path.join(directory, 'server'), origin,
      environment: await isolatedEnvironment(path.join(directory, 'server-environment')) })
    await verifyEmbeddedAssets(origin)
    evidence.checks.push('Server embeds the current app.js and app.css')
    const identity = server.owner.session
    const scope = { token: identity.access_token, tenantId: identity.personal_tenant_id }
    const team = (await serverRequest(origin, '/tenants', { token: scope.token,
      body: { slug: 'workspace-default-team', display_name: 'Workspace default team' } })).tenant
    const policy = await serverRequest(origin, '/admin/registration', { token: scope.token })
    await serverRequest(origin, '/admin/registration', { token: scope.token, method: 'PATCH',
      body: { mode: 'open', require_approval: false, oidc_only: false, revision: policy.revision } })
    const member = { username: 'workspace-member', email: 'workspace-member@example.test', password: 'workspace-member-password' }
    const { session: memberIdentity } = await serverRequest(origin, '/auth/register', { body: member })
    assert.ok(memberIdentity?.personal_tenant_id)

    const nodes = []
    for (const name of ['alpha', 'beta']) {
      const { enrollment } = await serverRequest(origin, `/tenants/${scope.tenantId}/my-computer-enrollments`, {
        token: scope.token, body: { name, project_id: null, ttl_seconds: 600 },
      })
      const { credential } = await serverRequest(origin, '/enrollments/consume', { body: { token: enrollment.token } })
      const localOrigin = `http://127.0.0.1:${await freePort()}`
      const environment = await isolatedEnvironment(path.join(directory, `${name}-environment`))
      const node = startProcess(process.env.TERNILO_E2E_NODE_BINARY ?? path.join(repository, 'target/debug/ternilo'), [
        'serve', '--listen', new URL(localOrigin).host, '--data-dir', path.join(directory, `${name}-data`),
        '--node-id', enrollment.executor_id, '--gateway-url', `${origin.replace('http:', 'ws:')}/api/v1/executors/connect`,
        '--allow-insecure-gateway',
      ], { ...environment, TERNILO_LOCAL_TOKEN: credential.token })
      processes.push(node)
      await waitForHttp(localOrigin, node)
      await until(() => serverRequest(origin, '/execution-targets', scope),
        result => result.executors.some(item => item.executor_id === enrollment.executor_id && item.connected), `${name} connects`)
      const folder = path.join(directory, `existing-${name}`)
      await mkdir(folder)
      const { workspace } = await serverRequest(origin, '/workspaces', { ...scope, body: {
        project_id: identity.personal_project_id, name: `Existing ${name}`, placement: 'local_node',
        executor_id: enrollment.executor_id, path: folder,
      } })
      nodes.push({ name, id: enrollment.executor_id, process: node, workspace })
    }
    const [alpha, beta] = nodes
    const session = await serverRequest(origin, '/sessions', { ...scope, body: { workspace_id: beta.workspace.workspace_id } })
    beta.sessionId = session.identity.session_id
    await serverRequest(origin, `/sessions/${beta.sessionId}/queue`, { ...scope,
      body: { content: { kind: 'prompt', input: '/code "beta-history"' } } })
    await until(() => serverRequest(origin, `/sessions/${beta.sessionId}/events`, scope),
      events => events.some(event => event.type === 'turn_finished'), 'beta has visible session history')
    const folders = [path.join(directory, 'opened-on-alpha'), path.join(directory, 'opened-on-beta')]
    await Promise.all(folders.map(folder => mkdir(folder)))

    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
    const context = await browser.newContext({ locale: 'zh-CN', viewport: { width: 1365, height: 900 }, serviceWorkers: 'block' })
    page = await context.newPage()
    page.on('pageerror', error => errors.push(`Page: ${error.message}`))
    page.on('console', message => { if (message.type() === 'error') errors.push(`Console: ${message.text()}`) })
    page.on('request', request => requests.push({ method: request.method(), pathname: new URL(request.url()).pathname }))
    page.on('requestfailed', request => {
      if (request.failure()?.errorText !== 'net::ERR_ABORTED') errors.push(`${request.failure()?.errorText}: ${new URL(request.url()).pathname}`)
    })
    page.on('response', response => {
      if (response.status() >= 400) errors.push(`HTTP ${response.status()}: ${new URL(response.url()).pathname}`)
    })
    await page.goto(origin)
    await login(page, server.owner)
    await selectBetaSession(page, beta)
    await verifyIndependentLoading(page, beta, alpha, artifacts, evidence)
    await selectBetaSession(page, beta)
    const activeUrl = page.url()
    await chooseComputer(page, `node:${alpha.id}`)
    assert.equal(page.url(), activeUrl, 'sidebar filtering does not change the current beta session')
    let dialog = await openPicker(page)
    assert.equal(await dialog.getByLabel('运行电脑', { exact: true }).getAttribute('data-choice-value'), alpha.id)
    if (artifacts) await page.screenshot({ path: path.join(artifacts, 'workspace-default-alpha.png'), fullPage: true, animations: 'disabled' })
    evidence.workspaces.push(await openFolder(page, dialog, alpha, folders[0], origin, scope))
    evidence.checks.push('current beta session + sidebar alpha defaults to alpha and creates an alpha workspace')

    await selectBetaSession(page, beta)
    await chooseComputer(page, `node:${alpha.id}`)
    dialog = await openPicker(page)
    const computer = dialog.getByLabel('运行电脑', { exact: true })
    assert.equal(await computer.getAttribute('data-choice-value'), alpha.id)
    await selectChoice(computer, beta.id)
    const refreshed = page.waitForResponse(response => new URL(response.url()).pathname === '/api/v1/execution-targets')
    await dialog.getByRole('button', { name: '刷新', exact: true }).click()
    assert.equal((await refreshed).status(), 200)
    await until(() => dialog.getAttribute('aria-busy'), busy => busy !== 'true', 'manual choice refresh settles')
    assert.equal(await computer.getAttribute('data-choice-value'), beta.id, 'refresh must retain the manual computer choice')
    if (artifacts) await page.screenshot({ path: path.join(artifacts, 'workspace-manual-beta.png'), fullPage: true, animations: 'disabled' })
    evidence.workspaces.push(await openFolder(page, dialog, beta, folders[1], origin, scope))
    evidence.checks.push('manual beta survives refresh and creates a beta workspace')

    await chooseComputer(page, 'all')
    dialog = await openPicker(page)
    assert.equal(await dialog.getByLabel('运行电脑', { exact: true }).getAttribute('data-choice-value'), beta.id,
      'all computers keeps the current-workspace default')
    await closePicker(dialog)
    evidence.checks.push('all computers retains the previous default rule')

    await chooseComputer(page, `node:${alpha.id}`)
    for (const locale of ['zh', 'en', 'ko']) {
      await page.evaluate(locale => localStorage.setItem('ternilo.locale', locale), locale)
      await page.reload()
      dialog = await openPicker(page, locale)
      assert.equal(await dialog.getByRole('combobox', { name: labels[locale].computer, exact: true }).getAttribute('data-choice-value'), alpha.id)
      if (artifacts) await page.screenshot({ path: path.join(artifacts, `workspace-default-${locale}.png`), fullPage: true, animations: 'disabled' })
      await closePicker(dialog, locale)
    }
    evidence.checks.push('Chinese, English and Korean show the selected default after reload')
    await page.evaluate(() => localStorage.setItem('ternilo.locale', 'zh'))
    await page.reload()
    await page.getByRole('button', { name: '添加工作区', exact: true }).waitFor()

    await verifyConfigurationRetry(page, beta, errors, artifacts, evidence)
    await verifyVersionSummaries(page, alpha, beta, scope, artifacts, evidence)

    const teamScope = { ...scope, tenantId: team.tenant_id }
    assert.equal((await serverRequest(origin, '/state', teamScope)).workspaces.length, 0)
    const teamTargets = (await serverRequest(origin, '/execution-targets', teamScope)).executors
    const expectedTeamTarget = teamTargets.find(item => item.connected) ?? teamTargets[0]
    const previousComputer = expectedTeamTarget?.executor_id === alpha.id ? beta : alpha
    await chooseComputer(page, `node:${previousComputer.id}`)
    await selectSpace(page, team.tenant_id)
    assert.equal(await page.locator('[data-sidebar-computer-picker]').getAttribute('data-choice-value'), 'all')
    dialog = await openPicker(page)
    if (expectedTeamTarget) {
      assert.equal(await dialog.getByLabel('运行电脑', { exact: true }).getAttribute('data-choice-value'), expectedTeamTarget.executor_id,
        'a new space uses its own available-target default')
    } else {
      assert.equal(await dialog.locator('#workspace-executor').count(), 0)
      await dialog.getByRole('button', { name: '接入电脑', exact: true }).waitFor()
    }
    await closePicker(dialog)
    evidence.checks.push('switching space clears the previous sidebar preference and uses scoped targets')

    await selectSpace(page, scope.tenantId)
    await selectBetaSession(page, beta)
    await chooseComputer(page, `node:${alpha.id}`)
    await stopProcess(alpha.process)
    await until(() => serverRequest(origin, '/execution-targets', scope),
      result => result.executors.some(item => item.executor_id === alpha.id && !item.connected), 'alpha goes offline')
    await until(() => page.locator('[data-sidebar-computer-picker]').textContent(), text => text.includes('离线'), 'sidebar shows offline alpha')
    const offlineRequests = requests.length
    dialog = await openPicker(page)
    assert.equal(await dialog.getByLabel('运行电脑', { exact: true }).getAttribute('data-choice-value'), alpha.id,
      'offline alpha must not silently change to online beta')
    assert.equal(await dialog.getByRole('button', { name: '选择文件夹', exact: true }).isDisabled(), true)
    assert.equal(await dialog.getByRole('button', { name: '打开并开始会话', exact: true }).isDisabled(), true)
    await dialog.getByRole('alert').filter({ hasText: '这台电脑当前离线' }).waitFor()
    assert.equal(requests.slice(offlineRequests).some(request => request.pathname.includes('/directories')), false,
      'offline selection never attempts directory browsing')
    if (artifacts) await page.screenshot({ path: path.join(artifacts, 'workspace-default-offline-alpha.png'), fullPage: true, animations: 'disabled' })
    await closePicker(dialog)
    evidence.checks.push('offline alpha remains selected, warns, and disables directory browsing and submission')
    await page.goto(`${origin}/settings/computers`)
    const offlineCard = page.locator(`[data-platform-computer="${alpha.id}"]`)
    // Maintenance has its own 30-second lease, independent of WebSocket disconnect.
    await offlineCard.locator('[data-computer-version-state="offline"]').waitFor({ timeout: 40_000 })
    assert.equal(await offlineCard.getByRole('img', { name: '维护通道离线，连接后才能更新', exact: true }).count(), 1)
    assert.ok((await offlineCard.textContent()).includes('维护通道离线，连接后才能更新'))
    if (artifacts) await page.screenshot({ path: path.join(artifacts, 'computer-versions-offline.png'), fullPage: true, animations: 'disabled' })
    await page.goto(origin)
    evidence.checks.push('real offline computer shows an X and an explicit reason instead of a stale check')

    // Finish initial metadata reads before deliberately revoking this browser token.
    await page.waitForLoadState('networkidle')
    await page.getByRole('button', { name: '退出登录', exact: true }).click()
    await login(page, member)
    await until(() => page.evaluate(() => localStorage.getItem('ternilo.current-tenant')),
      id => id === memberIdentity.personal_tenant_id, 'second account loads its own space in the same tab')
    assert.equal(await page.locator('[data-sidebar-computer-picker]').getAttribute('data-choice-value'), 'all')
    const accountRequests = requests.length
    dialog = await openPicker(page)
    assert.equal(await dialog.locator('#workspace-executor').count(), 0)
    await dialog.getByRole('button', { name: '接入电脑', exact: true }).waitFor()
    assert.equal(await dialog.getByRole('button', { name: '选择文件夹', exact: true }).isDisabled(), true)
    assert.equal(requests.slice(accountRequests).some(request => request.pathname.startsWith(`/api/v1/executors/${alpha.id}/`)), false)
    assert.equal(requests.slice(accountRequests).some(request => request.pathname.startsWith(`/api/v1/executors/${beta.id}/`)), false)
    await closePicker(dialog)
    evidence.checks.push('same-tab logout and login do not reuse the previous account computer')
    assert.deepEqual(errors, [])
  } catch (error) {
    evidence.failure = error.stack
    if (page && artifacts) await page.screenshot({ path: path.join(artifacts, 'workspace-default-computer-failure.png'), fullPage: true, animations: 'disabled' }).catch(() => {})
    throw new Error(`${error.stack}\n${server?.diagnostics() ?? ''}\n${processes.map(process => process.diagnostics()).join('\n')}`)
  } finally {
    await browser?.close()
    for (const process of processes) await stopProcess(process)
    await stopProcess(server)
    await rm(directory, { recursive: true, force: true })
    if (artifacts) await writeFile(path.join(artifacts, 'workspace-default-computer-result.json'), `${JSON.stringify(evidence, null, 2)}\n`)
  }
})
