import assert from 'node:assert/strict'
import { mkdir, mkdtemp, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import { spawn } from 'node:child_process'
import test from 'node:test'
import { chromium } from 'playwright'
import { until } from './model-device-fixture.mjs'

const webRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..')
const repository = path.resolve(webRoot, '..')
const binary = process.env.TERNILO_E2E_NODE_BINARY ?? path.join(repository, 'target', 'debug', 'ternilo')

function startTernilo(dataDirectory) {
  const child = spawn(binary, ['serve', '--listen', '127.0.0.1:0', '--data-dir', dataDirectory], {
    cwd: repository,
    stdio: ['ignore', 'pipe', 'pipe'],
  })
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
  return { child, origin }
}

async function stopProcess(child) {
  if (child.exitCode !== null) return
  child.kill('SIGINT')
  await Promise.race([
    new Promise(resolve => child.once('exit', resolve)),
    new Promise(resolve => setTimeout(resolve, 5_000)).then(() => child.kill('SIGKILL')),
  ])
}

async function apiRequest(page, pathValue, method, bodyValue) {
  return page.evaluate(async ({ pathValue, method, bodyValue }) => {
    const token = window.__TERNILO_BOOT__?.apiToken
    const response = await fetch(`/api/v1${pathValue}`, {
      method,
      headers: {
        'content-type': 'application/json',
        ...(token ? { authorization: `Bearer ${token}` } : {}),
      },
      body: bodyValue === undefined ? undefined : JSON.stringify(bodyValue),
    })
    if (!response.ok) throw new Error(`${method} ${pathValue}: ${response.status} ${await response.text()}`)
    return response.status === 204 ? null : response.json()
  }, { pathValue, method, bodyValue })
}

async function registerWorkspace(page, workspace) {
  await page.getByRole('button', { name: '选择工作文件夹' }).first().click()
  const dialog = page.getByRole('dialog', { name: '选择工作文件夹' })
  await dialog.getByRole('button', { name: '编辑文件夹路径' }).click()
  const editor = dialog.getByRole('textbox', { name: '编辑文件夹路径' })
  await editor.fill(workspace)
  await editor.press('Enter')
  await dialog.getByRole('list', { name: `目录 ${workspace}`, exact: true }).waitFor()
  await dialog.getByRole('button', { name: '打开所选文件夹' }).click()
  await dialog.waitFor({ state: 'detached' })
}

async function ensureMobileSidebarOpen(page) {
  const sidebar = page.getByRole('complementary', { name: '会话侧边栏' })
  await page.waitForTimeout(200)
  const initial = await sidebar.boundingBox()
  if (!initial || initial.x < -1) await page.getByRole('button', { name: '打开侧边栏' }).click()
  await page.waitForFunction(() => {
    const x = document.querySelector('.app-sidebar')?.getBoundingClientRect().x
    return x !== undefined && Math.abs(x) < 0.5
  })
  return sidebar
}

function sessionRows(group) {
  return group.locator('[data-sidebar-session-row]')
}

function establishedRows(group, page) {
  return sessionRows(group).filter({ has: page.locator('[data-sidebar-session-time]') })
}

test('one provisional blank does not consume the five established sidebar rows', { timeout: 90_000 }, async () => {
  const dataDirectory = await mkdtemp(path.join(tmpdir(), 'ternilo-sidebar-folding-e2e-'))
  const workspace = path.join(dataDirectory, 'folding-workspace')
  await mkdir(workspace)
  const ternilo = startTernilo(dataDirectory)
  let browser
  try {
    const origin = await ternilo.origin
    browser = await chromium.launch({ headless: true })
    const page = await browser.newPage({ locale: 'zh-CN', viewport: { width: 1440, height: 900 } })
    const pageErrors = []
    page.on('pageerror', error => pageErrors.push(error.message))
    await page.goto(origin, { waitUntil: 'networkidle' })

    // Opening a workspace already creates its first provisional session.
    await registerWorkspace(page, workspace)
    await page.locator('[data-sidebar-session-row]').filter({ hasText: '新会话' }).first().waitFor()
    await page.waitForFunction(() => localStorage.getItem('ternilo.current-session') !== null)
    const sourceSessionId = await page.evaluate(() => localStorage.getItem('ternilo.current-session'))
    assert.ok(sourceSessionId)

    await apiRequest(
      page,
      `/sessions/${encodeURIComponent(sourceSessionId)}/turns`,
      'POST',
      { input: '/code "established root"' },
    )
    // Turn submission acknowledges before execution finishes; forks must copy a completed turn.
    await until(() => apiRequest(page, `/sessions/${encodeURIComponent(sourceSessionId)}/events`), events => events.some(event => event.type === 'turn_finished'), 'source turn completed before forking')
    for (let index = 0; index < 5; index += 1) {
      await apiRequest(page, `/sessions/${encodeURIComponent(sourceSessionId)}/fork`, 'POST', {})
    }

    await page.reload({ waitUntil: 'domcontentloaded' })
    let group = page.locator('[data-sidebar-workspace-group]').filter({ hasText: 'folding-workspace' })
    await group.waitFor()
    assert.equal(await establishedRows(group, page).count(), 5)
    assert.equal(await sessionRows(group).count(), 5)
    const establishedMore = group.getByRole('button', { name: '其余 1 个会话' })
    assert.equal(await establishedMore.getAttribute('aria-expanded'), 'false')

    await page.locator('[data-sidebar-new-session]').click()
    group = page.locator('[data-sidebar-workspace-group]').filter({ hasText: 'folding-workspace' })
    const blank = group.locator('[data-sidebar-session-row]').filter({ hasText: '新会话', hasNot: page.locator('[data-sidebar-session-time]') })
    await blank.waitFor()
    assert.equal(await blank.locator('[data-sidebar-session-time]').count(), 0)
    assert.equal(await blank.getByRole('button', { name: /的操作$/ }).count(), 0)
    assert.equal(await establishedRows(group, page).count(), 5)
    assert.equal(await sessionRows(group).count(), 6)
    const more = group.getByRole('button', { name: '其余 1 个会话' })
    assert.equal(await more.getAttribute('aria-expanded'), 'false')

    await more.click()
    assert.equal(await sessionRows(group).count(), 7)
    const collapseSessions = group.getByRole('button', { name: '收起' })
    assert.equal(await collapseSessions.getAttribute('aria-expanded'), 'true')
    await collapseSessions.click()
    assert.equal(await sessionRows(group).count(), 6)
    await group.getByRole('button', { name: '其余 1 个会话' }).waitFor()

    const workspaceToggle = group.locator('[data-sidebar-workspace-button]')
    assert.equal(await workspaceToggle.getAttribute('aria-expanded'), 'true')
    await workspaceToggle.click()
    assert.equal(await workspaceToggle.getAttribute('aria-expanded'), 'false')
    assert.equal(await sessionRows(group).count(), 0)
    await workspaceToggle.click()
    assert.equal(await workspaceToggle.getAttribute('aria-expanded'), 'true')
    assert.equal(await sessionRows(group).count(), 6)

    await group.getByRole('button', { name: '其余 1 个会话' }).click()
    assert.equal(await sessionRows(group).count(), 7)
    await page.reload({ waitUntil: 'domcontentloaded' })
    group = page.locator('[data-sidebar-workspace-group]').filter({ hasText: 'folding-workspace' })
    await group.waitFor()
    assert.equal(await group.locator('[data-sidebar-workspace-button]').getAttribute('aria-expanded'), 'true')
    assert.equal(await establishedRows(group, page).count(), 5)
    assert.equal(await sessionRows(group).count(), 6)
    assert.equal(await group.getByRole('button', { name: '其余 1 个会话' }).getAttribute('aria-expanded'), 'false')

    await page.setViewportSize({ width: 390, height: 844 })
    const sidebar = await ensureMobileSidebarOpen(page)
    group = sidebar.locator('[data-sidebar-workspace-group]').filter({ hasText: 'folding-workspace' })
    assert.equal(await establishedRows(group, page).count(), 5)
    assert.equal(await sessionRows(group).count(), 6)
    const mobileMore = group.getByRole('button', { name: '其余 1 个会话' })
    const mobileMoreBox = await mobileMore.boundingBox()
    assert.ok(mobileMoreBox)
    assert.equal(mobileMoreBox.height >= 40, true)
    await mobileMore.click()
    assert.equal(await sessionRows(group).count(), 7)
    await group.getByRole('button', { name: '收起' }).click()
    assert.equal(await sessionRows(group).count(), 6)
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth), 390)

    assert.deepEqual(pageErrors, [])
  } finally {
    await browser?.close()
    await stopProcess(ternilo.child)
    await rm(dataDirectory, { recursive: true, force: true })
  }
})
