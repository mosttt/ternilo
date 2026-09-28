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
const binary = path.join(repository, 'target', 'debug', 'ternilo')

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
  await page.locator('[data-sidebar-new-session]').click()
  await page.getByRole('textbox', { name: '输入任务' }).waitFor()
  await page.locator('[data-session-state="empty"]').waitFor()
}

async function submitDirect(page, command) {
  const complete = await page.locator('[data-chat-event="command"][data-state="complete"]').count()
  const input = page.getByRole('textbox', { name: '输入任务' })
  await input.fill('/go')
  await page.locator('[data-composer-menu] [role="option"]').filter({ hasText: /^\/goal/ }).waitFor()
  await input.fill(command)
  await page.getByRole('button', { name: '发送' }).click()
  await page.waitForFunction(expected => (
    document.querySelectorAll('[data-chat-event="command"][data-state="complete"]').length > expected
    && !document.querySelector('[data-composer-card][data-busy]')
  ), complete)
}

async function currentMode(page) {
  return page.evaluate(async () => {
    const sessionId = localStorage.getItem('ternilo.current-session')
    const token = window.__TERNILO_BOOT__?.apiToken
    const response = await fetch('/api/v1/state', { headers: token ? { authorization: `Bearer ${token}` } : {} })
    const state = await response.json()
    return state.sessions.find(session => session.identity.session_id === sessionId)?.mode
  })
}

test('Plan and Goal direct actions use the real Local command plane on desktop and touch layout', { timeout: 60_000 }, async () => {
  const dataDirectory = await mkdtemp(path.join(tmpdir(), 'ternilo-plan-goal-'))
  const workspace = path.join(dataDirectory, 'workspace')
  await mkdir(workspace, { recursive: true })
  const ternilo = startTernilo(dataDirectory)
  let browser
  try {
    const origin = await ternilo.origin
    browser = await chromium.launch({ headless: true })
    const page = await browser.newPage({ viewport: { width: 1280, height: 800 } })
    const pageErrors = []
    page.on('pageerror', error => pageErrors.push(error.message))
    await page.goto(origin, { waitUntil: 'networkidle' })
    await chooseWorkspace(page, workspace)

    await submitDirect(page, '/goal 交付 Web 直接操作')
    const dock = page.locator('[data-projection-dock]')
    await dock.getByText('交付 Web 直接操作', { exact: true }).waitFor()

    const enterPlan = page.getByRole('button', { name: '切换到计划模式' })
    await enterPlan.click()
    const leavePlan = page.getByRole('button', { name: '切换到执行模式' })
    await leavePlan.waitFor()
    assert.equal(await currentMode(page), 'plan', await page.locator('body').innerText())
    await leavePlan.click()
    await enterPlan.waitFor()
    assert.equal(await currentMode(page), 'execute')

    await dock.getByRole('button', { name: '编辑目标' }).click()
    const objective = dock.getByRole('textbox', { name: '目标内容' })
    await objective.fill('交付全部 Web')
    await dock.getByRole('button', { name: '保存目标' }).click()
    await dock.getByText('交付全部 Web', { exact: true }).waitFor()

    await page.setViewportSize({ width: 390, height: 844 })
    for (const action of ['标记目标受阻', '编辑目标', '完成目标']) {
      const box = await dock.getByRole('button', { name: action }).boundingBox()
      assert.equal(box.width >= 40 && box.height >= 40, true, `${action}: ${JSON.stringify(box)}`)
    }
    await dock.getByRole('button', { name: '标记目标受阻' }).click()
    await dock.getByText('目标受阻', { exact: true }).waitFor()
    await page.reload({ waitUntil: 'domcontentloaded' })
    await page.locator('[data-session-state="ready"]').waitFor()
    await dock.getByText('目标受阻', { exact: true }).waitFor()
    await dock.getByText('交付全部 Web', { exact: true }).waitFor()
    const resume = dock.getByRole('button', { name: '继续目标' })
    const resumeBox = await resume.boundingBox()
    assert.equal(resumeBox.width >= 40 && resumeBox.height >= 40, true, JSON.stringify(resumeBox))
    await resume.click()
    await dock.getByText('目标', { exact: true }).waitFor()

    await dock.getByRole('button', { name: '完成目标' }).click()
    await dock.waitFor({ state: 'detached' })
    assert.deepEqual(pageErrors, [])
    assert.equal(ternilo.diagnostics().includes('panicked'), false, ternilo.diagnostics())
  } finally {
    await browser?.close()
    await stopProcess(ternilo.child)
    await rm(dataDirectory, { recursive: true, force: true })
  }
})
