import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { mkdir, mkdtemp, readFile, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import { spawn } from 'node:child_process'
import test from 'node:test'
import { chromium } from 'playwright'

const webRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..')
const repository = path.resolve(webRoot, '..')
const binary = path.join(repository, 'target', 'debug', 'ternilo')

function startTernilo(dataDirectory) {
  const child = spawn(binary, [
    'serve', '--listen', '127.0.0.1:0', '--data-dir', dataDirectory,
  ], { cwd: repository, stdio: ['ignore', 'pipe', 'pipe'] })
  let diagnostics = ''
  child.stderr.setEncoding('utf8')
  child.stderr.on('data', chunk => { diagnostics += chunk })
  const origin = new Promise((resolve, reject) => {
    let output = ''
    child.stdout.setEncoding('utf8')
    child.stdout.on('data', chunk => {
      output += chunk
      const match = output.match(/Ternilo local web: (http:\/\/[^\s]+)/)
      if (match) resolve(match[1])
    })
    child.once('exit', code => reject(new Error(`Ternilo web exited with ${code}: ${diagnostics}`)))
    child.once('error', reject)
  })
  return { child, origin, diagnostics: () => diagnostics }
}

async function stopProcess(child) {
  if (child.exitCode !== null) return
  child.kill('SIGINT')
  await Promise.race([
    new Promise(resolve => child.once('exit', resolve)),
    new Promise(resolve => setTimeout(resolve, 5_000)).then(() => child.kill('SIGKILL')),
  ])
}

async function assertHeroCentered(page, viewport) {
  await page.setViewportSize(viewport)
  await page.waitForFunction(mobile => document.querySelector('[data-app-frame]')?.hasAttribute('data-mobile') === mobile, viewport.width <= 760)
  await page.waitForFunction(() => {
    const available = document.querySelector('[data-conversation-scroll]')?.getBoundingClientRect()
    const hero = document.querySelector('[data-composer-seat]')?.getBoundingClientRect()
    if (!available || !hero) return false
    return Math.abs(
      (available.top + available.height / 2) - (hero.top + hero.height / 2),
    ) <= 2
  })
  const geometry = await page.evaluate(() => {
    const available = document.querySelector('[data-conversation-scroll]').getBoundingClientRect()
    const hero = document.querySelector('[data-composer-seat]').getBoundingClientRect()
    return {
      available: {
        top: available.top,
        bottom: available.bottom,
        center: available.top + available.height / 2,
      },
      hero: {
        top: hero.top,
        bottom: hero.bottom,
        center: hero.top + hero.height / 2,
      },
      viewportWidth: document.documentElement.clientWidth,
      documentWidth: document.documentElement.scrollWidth,
    }
  })
  const label = `${viewport.width}x${viewport.height}`
  assert.equal(Math.abs(geometry.available.center - geometry.hero.center) <= 2, true, `${label}: ${JSON.stringify(geometry)}`)
  assert.equal(geometry.hero.top >= geometry.available.top - 1, true, `${label}: ${JSON.stringify(geometry)}`)
  assert.equal(geometry.hero.bottom <= geometry.available.bottom + 1, true, `${label}: ${JSON.stringify(geometry)}`)
  assert.equal(geometry.documentWidth, geometry.viewportWidth, `${label}: horizontal overflow`)
  if (viewport.width <= 760) {
    const menu = await page.getByRole('button', { name: '打开侧边栏' }).boundingBox()
    assert.ok(menu && menu.x <= 12 && menu.y <= 12, `${label}: mobile menu is not anchored to the viewport corner: ${JSON.stringify(menu)}`)
  }
  return { label, ...geometry }
}

async function assertWorkspaceSelectionCentered(page, viewport) {
  await page.setViewportSize(viewport)
  await page.waitForFunction(mobile => document.querySelector('[data-app-frame]')?.hasAttribute('data-mobile') === mobile, viewport.width <= 760)
  await page.waitForFunction(() => {
    const selection = document.querySelector('[data-empty-workspace-selection]')?.getBoundingClientRect()
    const content = document.querySelector('[data-empty-workspace-selection]')?.firstElementChild?.getBoundingClientRect()
    if (!selection || !content) return false
    return Math.abs((selection.left + selection.width / 2) - (content.left + content.width / 2)) <= 2
      && Math.abs((selection.top + selection.height / 2) - (content.top + content.height / 2)) <= 2
  })
  const geometry = await page.locator('[data-empty-workspace-selection]').evaluate(element => {
    const available = element.getBoundingClientRect()
    const content = element.firstElementChild.getBoundingClientRect()
    return {
      available: {
        centerX: available.left + available.width / 2,
        centerY: available.top + available.height / 2,
      },
      content: {
        centerX: content.left + content.width / 2,
        centerY: content.top + content.height / 2,
      },
      viewportWidth: document.documentElement.clientWidth,
      documentWidth: document.documentElement.scrollWidth,
    }
  })
  const label = `${viewport.width}x${viewport.height}`
  assert.equal(Math.abs(geometry.available.centerX - geometry.content.centerX) <= 2, true, `${label}: ${JSON.stringify(geometry)}`)
  assert.equal(Math.abs(geometry.available.centerY - geometry.content.centerY) <= 2, true, `${label}: ${JSON.stringify(geometry)}`)
  assert.equal(geometry.documentWidth, geometry.viewportWidth, `${label}: horizontal overflow`)
  if (viewport.width <= 760) {
    const menu = await page.getByRole('button', { name: '打开侧边栏' }).boundingBox()
    assert.ok(menu && menu.x <= 12 && menu.y <= 12, `${label}: mobile menu is not anchored to the viewport corner: ${JSON.stringify(menu)}`)
  }
  return { label, ...geometry }
}

test('Add workspace opens its first Session and keeps both empty surfaces centered', { timeout: 60_000 }, async () => {
  const dataDirectory = await mkdtemp(path.join(tmpdir(), 'ternilo-new-session-e2e-'))
  const workspacePath = path.join(dataDirectory, 'selected-workspace')
  await mkdir(workspacePath)
  const ternilo = startTernilo(dataDirectory)
  let browser
  try {
    const origin = await ternilo.origin
    browser = await chromium.launch({
      headless: true,
      executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined,
    })
    const page = await browser.newPage({ viewport: { width: 1440, height: 900 } })
    const pageErrors = []
    const consoleErrors = []
    page.on('pageerror', error => pageErrors.push(error.message))
    page.on('console', message => {
      if (message.type() === 'error') consoleErrors.push(message.text())
    })
    await page.goto(origin, { waitUntil: 'networkidle' })

    const selectionGeometry = []
    for (const viewport of [
      { width: 1440, height: 900 },
      { width: 390, height: 844 },
      { width: 844, height: 390 },
    ]) selectionGeometry.push(await assertWorkspaceSelectionCentered(page, viewport))
    process.stdout.write(`Workspace selection geometry: ${JSON.stringify(selectionGeometry)}\n`)

    await page.setViewportSize({ width: 1440, height: 900 })
    await page.getByRole('button', { name: '添加工作区' }).click()
    const picker = page.getByRole('dialog', { name: '选择工作文件夹' })
    await picker.waitFor()
    await picker.getByRole('button', { name: '编辑文件夹路径' }).click()
    const editor = picker.getByRole('textbox', { name: '编辑文件夹路径' })
    await editor.fill(workspacePath)
    await editor.press('Enter')
    await picker.getByRole('list', { name: `目录 ${workspacePath}`, exact: true }).waitFor()
    const workspaceResponsePromise = page.waitForResponse(response => (
      response.url().endsWith('/api/v1/workspaces')
      && response.request().method() === 'POST'
    ))
    const firstSessionRequestPromise = page.waitForRequest(request => (
      request.url().endsWith('/api/v1/sessions')
      && request.method() === 'POST'
    ))
    await picker.getByRole('button', { name: '打开所选文件夹' }).click()
    const [workspaceResponse, firstSessionRequest] = await Promise.all([
      workspaceResponsePromise,
      firstSessionRequestPromise,
    ])
    const workspacePayload = await workspaceResponse.json()
    const workspaceId = workspacePayload.workspace_id ?? workspacePayload.workspace?.workspace_id
    assert.equal(firstSessionRequest.postDataJSON().workspace_id, workspaceId)
    await picker.waitFor({ state: 'detached' })
    await page.locator('[data-new-session-hero]').waitFor()

    const heroGeometry = []
    for (const viewport of [
      { width: 1440, height: 900 },
      { width: 390, height: 844 },
      { width: 844, height: 390 },
    ]) heroGeometry.push(await assertHeroCentered(page, viewport))
    process.stdout.write(`Empty Hero geometry: ${JSON.stringify(heroGeometry)}\n`)

    await page.setViewportSize({ width: 1440, height: 900 })
    const selectedWorkspaceId = await page.evaluate(() => localStorage.getItem('ternilo.current-workspace'))
    assert.equal(selectedWorkspaceId, workspaceId)
    const previousSessionId = await page.evaluate(() => localStorage.getItem('ternilo.current-session'))
    const directSessionRequestPromise = page.waitForRequest(request => (
      request.url().endsWith('/api/v1/sessions')
      && request.method() === 'POST'
    ))
    await page.locator('[data-sidebar-new-session]').click()
    const directSessionRequest = await directSessionRequestPromise
    assert.equal(directSessionRequest.postDataJSON().workspace_id, workspaceId)
    assert.equal(await page.getByRole('dialog', { name: '选择工作文件夹' }).count(), 0)
    await page.waitForFunction(previous => localStorage.getItem('ternilo.current-session') !== previous, previousSessionId)

    const workspaceGroup = page.locator('[data-sidebar-workspace-group]').filter({ hasText: 'selected-workspace' })
    await workspaceGroup.locator('[data-sidebar-workspace-row]').hover()
    const rowSessionRequestPromise = page.waitForRequest(request => (
      request.url().endsWith('/api/v1/sessions')
      && request.method() === 'POST'
    ))
    await workspaceGroup.getByRole('button', { name: '在“selected-workspace”中新建会话' }).click()
    assert.equal((await rowSessionRequestPromise).postDataJSON().workspace_id, workspaceId)

    assert.deepEqual(pageErrors, [])
    assert.deepEqual(consoleErrors, [])
  } finally {
    await browser?.close()
    await stopProcess(ternilo.child)
    await rm(dataDirectory, { recursive: true, force: true })
  }
})

test('changing folders opens a new session and every creation entry keeps its selected workspace', { timeout: 90_000 }, async context => {
  const dataDirectory = await mkdtemp(path.join(tmpdir(), 'ternilo-folder-session-e2e-'))
  const folders = Object.fromEntries(['Folder A', 'Folder B', 'Folder C'].map(name => [name, path.join(dataDirectory, name)]))
  await Promise.all(Object.values(folders).map(folder => mkdir(folder)))
  const app = startTernilo(dataDirectory)
  let browser
  try {
    const origin = await app.origin
    const hashes = {}
    for (const asset of ['app.js', 'app.css']) {
      const response = await fetch(`${origin}/assets/${asset}`)
      assert.equal(response.status, 200)
      const hash = bytes => createHash('sha256').update(bytes).digest('hex')
      hashes[asset] = hash(Buffer.from(await response.arrayBuffer()))
      assert.equal(hashes[asset], hash(await readFile(path.join(webRoot, 'dist/assets', asset))), `Local embeds current ${asset}`)
    }
    context.diagnostic(`Current Local assets: ${JSON.stringify(hashes)}`)
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
    const page = await browser.newPage({ viewport: { width: 1440, height: 900 } })
    const errors = []
    const sessionRequests = []
    page.on('pageerror', error => errors.push(error.message))
    page.on('console', message => { if (message.type() === 'error') errors.push(message.text()) })
    page.on('response', response => { if (response.status() >= 400) errors.push(`${response.status()} ${response.url()}`) })
    page.on('requestfailed', request => { if (request.failure()?.errorText !== 'net::ERR_ABORTED') errors.push(`${request.url()}: ${request.failure()?.errorText}`) })
    page.on('request', request => {
      if (request.url().endsWith('/api/v1/sessions') && request.method() === 'POST') sessionRequests.push(request.postDataJSON())
    })
    await page.goto(origin, { waitUntil: 'networkidle' })
    const readState = () => page.evaluate(async () => {
      const response = await fetch('/api/v1/state', { headers: { authorization: `Bearer ${window.__TERNILO_BOOT__?.apiToken ?? ''}` } })
      if (!response.ok) throw new Error(`state failed: ${response.status}`)
      return response.json()
    })
    const waitForSelection = session => page.waitForFunction(expected => (
      localStorage.getItem('ternilo.current-workspace') === expected.workspace_id
      && localStorage.getItem('ternilo.current-session') === expected.identity.session_id
    ), session)
    const chooseFolder = async name => {
      const before = await readState()
      const picker = page.getByRole('dialog', { name: '选择工作文件夹' })
      await picker.getByRole('button', { name: '编辑文件夹路径' }).click()
      const editor = picker.getByRole('textbox', { name: '编辑文件夹路径' })
      await editor.fill(folders[name])
      await editor.press('Enter')
      await picker.getByRole('list', { name: `目录 ${folders[name]}`, exact: true }).waitFor()
      const responsePromise = page.waitForResponse(response => response.url().endsWith('/api/v1/sessions') && response.request().method() === 'POST')
      await picker.getByRole('button', { name: '打开所选文件夹' }).click()
      const response = await responsePromise
      assert.equal(response.status(), 201)
      const created = await response.json()
      assert.equal(created.workspace_path, folders[name])
      assert.equal(response.request().postDataJSON().workspace_id, created.workspace_id)
      await picker.waitFor({ state: 'detached' })
      await waitForSelection(created)
      await page.locator('[data-new-session-hero]').waitFor()
      assert.equal((await readState()).sessions.length, before.sessions.length + 1)
      return created
    }
    const group = name => page.locator('[data-sidebar-workspace-group]').filter({
      has: page.locator('[data-sidebar-workspace-title]', { hasText: new RegExp(`^${name}$`) }),
    })
    const createFrom = async (name, click) => {
      const responsePromise = page.waitForResponse(response => response.url().endsWith('/api/v1/sessions') && response.request().method() === 'POST')
      await click()
      const response = await responsePromise
      assert.equal(response.status(), 201)
      const created = await response.json()
      assert.equal(created.workspace_path, folders[name])
      assert.equal(response.request().postDataJSON().workspace_id, created.workspace_id)
      await waitForSelection(created)
      return created
    }

    await page.getByRole('button', { name: '选择工作文件夹', exact: true }).click()
    const firstA = await chooseFolder('Folder A')
    await page.getByRole('button', { name: '切换新会话工作区' }).click()
    const firstB = await chooseFolder('Folder B')
    assert.notEqual(firstA.identity.session_id, firstB.identity.session_id)
    await page.getByRole('button', { name: '添加工作区' }).click()
    const firstC = await chooseFolder('Folder C')

    for (const name of ['Folder A', 'Folder B', 'Folder C']) {
      await group(name).locator('[data-sidebar-workspace-row]').hover()
      await createFrom(name, () => group(name).getByRole('button', { name: `在“${name}”中新建会话` }).click())
    }
    await group('Folder A').locator('[data-sidebar-workspace-button]').click()
    await page.waitForFunction(id => localStorage.getItem('ternilo.current-workspace') === id, firstA.workspace_id)
    await createFrom('Folder A', () => page.locator('[data-sidebar-new-session]').click())
    await page.locator('[data-model-onboarding][data-state="empty"]').waitFor()
    if (process.env.TERNILO_E2E_ARTIFACT_DIR) {
      await page.mouse.move(1400, 850)
      await page.keyboard.press('Escape')
      await page.waitForFunction(() => !document.querySelector('button[aria-label^="复制工作区完整路径："]'))
      await mkdir(process.env.TERNILO_E2E_ARTIFACT_DIR, { recursive: true })
      await page.screenshot({ path: path.join(process.env.TERNILO_E2E_ARTIFACT_DIR, 'folder-sessions-desktop.png') })
    }

    await page.setViewportSize({ width: 390, height: 844 })
    await page.getByRole('button', { name: '切换新会话工作区' }).click()
    await chooseFolder('Folder B')
    await page.getByRole('button', { name: '打开侧边栏' }).click()
    await group('Folder C').locator('[data-sidebar-workspace-button]').click()
    await page.waitForFunction(id => localStorage.getItem('ternilo.current-workspace') === id, firstC.workspace_id)
    const last = await createFrom('Folder C', () => page.locator('[data-sidebar-new-session]').click())
    await page.waitForFunction(() => !document.querySelector('[data-app-frame]')?.hasAttribute('data-mobile-sidebar-open'))
    await page.waitForFunction(() => (document.querySelector('.app-sidebar')?.getBoundingClientRect().right ?? 0) <= 0)
    await page.locator('[data-model-onboarding][data-state="empty"]').waitFor()
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth === document.documentElement.clientWidth), true)

    await page.reload({ waitUntil: 'networkidle' })
    await waitForSelection(last)
    await page.locator('[data-model-onboarding][data-state="empty"]').waitFor()
    if (process.env.TERNILO_E2E_ARTIFACT_DIR) await page.screenshot({ path: path.join(process.env.TERNILO_E2E_ARTIFACT_DIR, 'folder-sessions-mobile.png') })
    const state = await readState()
    assert.equal(state.sessions.length, 9)
    assert.equal(sessionRequests.length, 9)
    assert.equal(state.workspaces.length, 3)
    for (const name of Object.keys(folders)) {
      const workspace = state.workspaces.find(item => item.path === folders[name])
      const sessions = state.sessions.filter(item => item.workspace_id === workspace.workspace_id)
      assert.equal(sessions.length, 3, name)
      assert.ok(sessions.every(item => item.workspace_path === folders[name]))
    }
    assert.deepEqual(state.sessions.find(item => item.identity.session_id === firstA.identity.session_id), firstA)
    assert.deepEqual(errors, [])
  } finally {
    await browser?.close()
    await stopProcess(app.child)
    await rm(dataDirectory, { recursive: true, force: true })
  }
})
