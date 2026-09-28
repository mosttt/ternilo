import assert from 'node:assert/strict'
import { mkdir, mkdtemp, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import { spawn } from 'node:child_process'
import test from 'node:test'
import { chromium } from 'playwright'

const webRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..')
const repository = path.resolve(webRoot, '..')
const binary = process.env.TERNILO_E2E_LOCAL_BINARY ?? path.join(repository, 'target', 'debug', 'ternilo')

function startTernilo(dataDirectory) {
  const child = spawn(binary, ['serve', '--listen', '127.0.0.1:0', '--data-dir', dataDirectory], {
    cwd: repository, stdio: ['ignore', 'pipe', 'pipe'],
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

async function chooseWorkspace(page, workspace) {
  await page.getByRole('button', { name: '选择工作文件夹' }).first().click()
  const dialog = page.getByRole('dialog', { name: '选择工作文件夹' })
  await dialog.getByRole('button', { name: '编辑文件夹路径' }).click()
  const editor = dialog.getByRole('textbox', { name: '编辑文件夹路径' })
  await editor.fill(workspace)
  await editor.press('Enter')
  await dialog.getByRole('list', { name: `目录 ${workspace}`, exact: true }).waitFor()
  await dialog.getByRole('button', { name: '打开所选文件夹' }).click()
  await dialog.waitFor({ state: 'detached' })
  const createdResponse = page.waitForResponse(response => response.request().method() === 'POST'
    && new URL(response.url()).pathname === '/api/v1/sessions')
  await page.locator('[data-sidebar-new-session]').click()
  const created = await (await createdResponse).json()
  await page.waitForFunction(sessionId => localStorage.getItem('ternilo.current-session') === sessionId, created.identity.session_id)
  await page.getByRole('textbox', { name: '输入任务' }).waitFor()
  await page.locator('[data-session-state="empty"]').waitFor()
}

async function openDirectory(page, trigger = '/') {
  const input = page.getByRole('textbox', { name: '输入任务' })
  await input.fill(trigger)
  const menu = page.locator('[data-composer-menu]')
  await menu.waitFor()
  await menu.getByRole('option').first().waitFor()
  return menu
}

async function switchPreset(page, name) {
  await page.getByRole('textbox', { name: '输入任务' }).fill('')
  await page.locator('[data-composer-menu]').waitFor({ state: 'detached' })
  await page.getByRole('button', { name: /新会话 Agent：/ }).click()
  await page.getByRole('menuitem', { name: new RegExp(`^${name}`) }).click()
  await page.getByRole('button', { name: `新会话 Agent：${name}` }).waitFor()
  return page.evaluate(async () => {
    const sessionId = localStorage.getItem('ternilo.current-session')
    const token = window.__TERNILO_BOOT__?.apiToken
    const response = await fetch(`/api/v1/sessions/${encodeURIComponent(sessionId)}/commands`, {
      headers: token ? { authorization: `Bearer ${token}` } : {},
    })
    if (!response.ok) throw new Error(await response.text())
    return response.json()
  })
}

test('runtime command directory follows the active preset without cross-Session cache leakage', { timeout: 60_000 }, async () => {
  const dataDirectory = await mkdtemp(path.join(tmpdir(), 'ternilo-command-catalog-'))
  const workspace = path.join(dataDirectory, 'workspace')
  await mkdir(workspace, { recursive: true })
  const ternilo = startTernilo(dataDirectory)
  let browser
  try {
    const origin = await ternilo.origin
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE })
    const page = await browser.newPage({ viewport: { width: 1280, height: 800 } })
    const pageErrors = []
    page.on('pageerror', error => pageErrors.push(error.message))
    await page.goto(origin, { waitUntil: 'networkidle' })
    await chooseWorkspace(page, workspace)

    let menu = await openDirectory(page)
    await menu.getByText('/goal', { exact: true }).waitFor()
    menu = await openDirectory(page, '.')
    await menu.getByText('.jobs', { exact: true }).waitFor()
    const overflow = await menu.getByRole('option').evaluateAll(rows => {
      const menuElement = rows[0]?.closest('[data-composer-menu]')
      if (!menuElement) return true
      const menuBox = menuElement.getBoundingClientRect()
      return rows.some(row => {
        const rowBox = row.getBoundingClientRect()
        const description = row.querySelector('span:last-of-type')
        return rowBox.right > menuBox.right + 1
          || (description && getComputedStyle(description).textOverflow !== 'ellipsis')
      })
    })
    assert.equal(overflow, false, await menu.innerText())

    const minimal = await switchPreset(page, '极简模式')
    assert.equal(minimal.commands.some(command => command.name === 'goal'), false)
    assert.equal(minimal.commands.some(command => command.name === 'read'), true)
    menu = await openDirectory(page)
    assert.equal(await menu.getByText('/goal', { exact: true }).count(), 0, await menu.innerText())
    menu = await openDirectory(page, '.')
    await menu.getByText('.read', { exact: true }).waitFor()
    assert.equal(await menu.getByText('.jobs', { exact: true }).count(), 0, await menu.innerText())

    const standard = await switchPreset(page, '标准模式')
    assert.equal(standard.commands.some(command => command.name === 'goal'), true)
    assert.equal(standard.commands.every(command => command.description.length <= 64), true)
    menu = await openDirectory(page)
    await menu.getByText('/goal', { exact: true }).waitFor()
    menu = await openDirectory(page, '.')
    await menu.getByText('.jobs', { exact: true }).waitFor()

    assert.deepEqual(pageErrors, [])
    assert.equal(ternilo.diagnostics().includes('panicked'), false, ternilo.diagnostics())
  } finally {
    await browser?.close()
    await stopProcess(ternilo.child)
    await rm(dataDirectory, { recursive: true, force: true })
  }
})
