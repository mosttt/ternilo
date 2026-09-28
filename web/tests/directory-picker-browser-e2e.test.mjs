import assert from 'node:assert/strict'
import { mkdir, mkdtemp, rm, stat } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import { spawn } from 'node:child_process'
import test from 'node:test'
import { chromium } from 'playwright'

const webRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..')
const repository = path.resolve(webRoot, '..')
const binary = process.env.TERNILO_E2E_NODE_BINARY ?? path.join(repository, 'target', 'debug', 'ternilo')

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
  await new Promise(resolve => {
    const timeout = setTimeout(() => child.kill('SIGKILL'), 5_000)
    child.once('exit', () => {
      clearTimeout(timeout)
      resolve()
    })
  })
}

test('local directory picker creates and opens a real folder across responsive layouts', { timeout: 60_000 }, async () => {
  const dataDirectory = await mkdtemp(path.join(tmpdir(), 'ternilo-directory-e2e-'))
  const workspace = path.join(dataDirectory, 'workspace')
  await Promise.all([
    mkdir(path.join(workspace, 'docs'), { recursive: true }),
    mkdir(path.join(workspace, 'crates', 'nested'), { recursive: true }),
    mkdir(path.join(workspace, '.hidden'), { recursive: true }),
  ])
  const app = startTernilo(dataDirectory)
  let browser
  try {
    const origin = await app.origin
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
    const page = await browser.newPage({ viewport: { width: 1440, height: 900 } })
    const pageErrors = []
    page.on('pageerror', error => pageErrors.push(error.message))
    await page.goto(origin, { waitUntil: 'networkidle' })

    assert.equal(await page.title(), 'Ternilo')
    assert.equal(await page.locator('meta[name="description"]').getAttribute('content'), '本地、远程与云端共享的 Agent 工作台')
    assert.equal((await page.request.get(`${origin}/manifest.webmanifest`)).ok(), true)
    assert.equal((await page.request.get(`${origin}/service-worker.js`)).ok(), true)

    await page.getByRole('button', { name: '选择工作文件夹' }).first().click()
    const dialog = page.getByRole('dialog', { name: '选择工作文件夹' })
    const directoryList = directoryPath => dialog.getByRole('list', { name: `目录 ${directoryPath}`, exact: true })
    await dialog.getByRole('button', { name: '编辑文件夹路径' }).click()
    const initialPathEditor = dialog.getByRole('textbox', { name: '编辑文件夹路径' })
    await initialPathEditor.fill(workspace)
    await initialPathEditor.press('Enter')
    await directoryList(workspace).waitFor()
    assert.equal(await dialog.getByRole('list').count(), 2)
    assert.equal(await dialog.getByRole('button', { name: /\.hidden/ }).count(), 0)

    await dialog.getByRole('button', { name: '显示隐藏目录' }).click()
    assert.equal(await dialog.getByRole('button', { name: /\.hidden/ }).isVisible(), true)
    await directoryList(workspace).getByRole('button', { name: /crates/ }).click()
    await directoryList(path.join(workspace, 'crates')).getByRole('button', { name: /nested/ }).waitFor()
    await dialog.getByRole('button', { name: path.basename(workspace), exact: true }).click()
    await directoryList(workspace).waitFor()

    await dialog.getByRole('button', { name: '编辑文件夹路径' }).click()
    const pathEditor = dialog.getByRole('textbox', { name: '编辑文件夹路径' })
    await pathEditor.fill(path.join(workspace, 'do'))
    await directoryList(workspace).getByRole('button', { name: /crates/ }).waitFor({ state: 'detached' })
    assert.equal(await directoryList(workspace).getByRole('button', { name: /docs/ }).isVisible(), true)
    await pathEditor.fill(path.join(workspace, 'missing-prefix'))
    await directoryList(workspace).getByRole('button', { name: /crates/ }).waitFor()
    await pathEditor.fill(workspace)
    await pathEditor.press('Enter')
    await directoryList(workspace).waitFor()

    await dialog.getByRole('button', { name: '新建文件夹' }).click()
    const createDialog = page.getByRole('dialog', { name: '新建文件夹' })
    const createdName = 'browser child '
    const createdPath = path.join(workspace, createdName)
    await createDialog.getByRole('textbox', { name: '文件夹名称' }).fill(createdName)
    await createDialog.getByRole('button', { name: '创建并选择' }).click()
    await createDialog.waitFor({ state: 'detached' })
    assert.equal((await stat(createdPath)).isDirectory(), true)
    await directoryList(createdPath).waitFor()
    const selectedLabel = await directoryList(workspace).locator('button[aria-current="true"]').textContent()
    assert.equal(selectedLabel?.includes(createdName), true)

    for (const viewport of [
      { width: 390, height: 844 },
      { width: 390, height: 430 },
      { width: 844, height: 390 },
    ]) {
      await page.setViewportSize(viewport)
      const geometry = await dialog.evaluate(element => ({
        documentWidth: document.documentElement.scrollWidth,
        viewportWidth: document.documentElement.clientWidth,
        dialog: element.getBoundingClientRect().toJSON(),
        shortButtons: [...element.querySelectorAll('button')]
          .filter(button => button.getClientRects().length > 0)
          .filter(button => button.getBoundingClientRect().height < 40)
          .map(button => button.getAttribute('aria-label') || button.textContent?.trim()),
      }))
      assert.equal(geometry.documentWidth, geometry.viewportWidth, `page overflow at ${viewport.width}x${viewport.height}`)
      assert.ok(geometry.dialog.left >= 0 && geometry.dialog.right <= viewport.width)
      assert.deepEqual(geometry.shortButtons, [])
    }

    await page.setViewportSize({ width: 1440, height: 900 })
    await dialog.getByRole('button', { name: '打开所选文件夹' }).click()
    await dialog.waitFor({ state: 'detached' })
    const state = await page.evaluate(async () => {
      const token = window.__TERNILO_BOOT__?.apiToken ?? ''
      const response = await fetch('/api/v1/state', {
        headers: token ? { authorization: `Bearer ${token}` } : {},
      })
      if (!response.ok) throw new Error(`state failed: ${response.status}`)
      return response.json()
    })
    const createdWorkspace = state.workspaces.find(item => item.path === createdPath)
    assert.ok(createdWorkspace, JSON.stringify(state.workspaces))
    await page.locator('[data-sidebar-new-session]').click()
    await page.getByRole('textbox', { name: '输入任务' }).waitFor()
    assert.equal(pageErrors.length, 0, pageErrors.join('\n'))
  } finally {
    if (browser) await browser.close()
    await stopProcess(app.child)
    await rm(dataDirectory, { recursive: true, force: true })
  }
})
