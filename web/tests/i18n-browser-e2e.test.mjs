import { selectChoice } from './browser-select-fixture.mjs'
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

async function api(page, endpoint, init = {}) {
  return page.evaluate(async ({ endpoint, init }) => {
    const token = window.__TERNILO_BOOT__?.apiToken ?? ''
    const response = await fetch(`/api/v1${endpoint}`, {
      ...init,
      headers: {
        ...(token ? { authorization: `Bearer ${token}` } : {}),
        ...(init.body === undefined ? {} : { 'content-type': 'application/json' }),
      },
      body: init.body === undefined ? undefined : JSON.stringify(init.body),
    })
    if (!response.ok) throw new Error(`${endpoint}: ${response.status} ${await response.text()}`)
    return response.status === 204 ? null : response.json()
  }, { endpoint, init })
}

test('language preference switches the core workbench, persists across reload, and restores Chinese', { timeout: 90_000 }, async () => {
  const dataDirectory = await mkdtemp(path.join(tmpdir(), 'ternilo-i18n-e2e-'))
  const workspacePath = path.join(dataDirectory, 'workspace')
  await mkdir(workspacePath)
  const ternilo = startTernilo(dataDirectory)
  let browser
  try {
    const origin = await ternilo.origin
    browser = await chromium.launch({ headless: true })
    const context = await browser.newContext({
      viewport: { width: 1280, height: 800 },
      serviceWorkers: 'block',
    })
    const page = await context.newPage()
    const errors = []
    page.on('pageerror', error => errors.push(error.message))
    await page.goto(origin, { waitUntil: 'networkidle' })

    const workspace = await api(page, '/workspaces', { method: 'POST', body: { path: workspacePath } })
    await api(page, '/sessions', { method: 'POST', body: { workspace_id: workspace.workspace_id, agent_preset: 'standard' } })
    let useHistoryFixture = true
    let historyRetryAttempt = 0
    let releaseHistoryRetry
    const historyRetryGate = new Promise(resolve => { releaseHistoryRetry = resolve })
    await page.routeWebSocket('**/api/v1/live', liveSocket => {
      if (!useHistoryFixture) {
        const server = liveSocket.connectToServer()
        liveSocket.onMessage(message => server.send(message))
        server.onMessage(message => liveSocket.send(message))
        return
      }
      liveSocket.onMessage(async message => {
        if (typeof message !== 'string') return
        const frame = JSON.parse(message)
        if (frame.type === 'hello') {
          liveSocket.send(JSON.stringify({ type: 'ready', protocol_version: 1 }))
          return
        }
        if (frame.type !== 'subscribe') return
        historyRetryAttempt += 1
        if (historyRetryAttempt === 1) {
          liveSocket.send(JSON.stringify({
            type: 'error', subscription_id: frame.subscription_id,
            code: 'unavailable', message: 'history fixture unavailable',
          }))
          return
        }
        await historyRetryGate
        liveSocket.send(JSON.stringify({
          type: 'event_batch', subscription_id: frame.subscription_id,
          session_id: frame.session_id, reset: true, complete: true, events: [], next_seq: 0,
        }))
        const profile = await api(page, `/sessions/${frame.session_id}/plugins`)
        liveSocket.send(JSON.stringify({
          type: 'session_metadata', subscription_id: frame.subscription_id, session_id: frame.session_id,
          metadata: { read: { inbox: false, stats: false, projection: false, questions: false, profile: true, agent_team: false }, profile },
        }))
      })
    })
    await page.reload({ waitUntil: 'domcontentloaded' })
    await page.getByRole('textbox', { name: '输入任务' }).waitFor()
    const historyFailure = page.locator('[data-history-state="error"]')
    await historyFailure.getByText(/history fixture unavailable/).waitFor()
    await historyFailure.getByRole('button', { name: '重新加载历史' }).click()
    const retryDeadline = Date.now() + 10_000
    while (historyRetryAttempt < 2 && Date.now() < retryDeadline) await new Promise(resolve => setTimeout(resolve, 10))
    assert.equal(historyRetryAttempt, 2, 'retry starts another history subscription without discarding the cached view')
    await historyFailure.waitFor({ state: 'hidden' })
    releaseHistoryRetry()
    await page.locator('[data-new-session-hero]').waitFor()
    assert.equal(historyRetryAttempt, 2)
    useHistoryFixture = false
    assert.equal(await page.locator('html').getAttribute('lang'), 'zh-CN')
    assert.equal(await page.getByText('让想法，动起来', { exact: true }).isVisible(), true)

    await page.getByRole('button', { name: '设置', exact: true }).click()
    let dialog = page.getByRole('dialog', { name: '设置' })
    await selectChoice(dialog.locator('[role="combobox"][aria-label="语言"]'), 'en')
    dialog = page.getByRole('dialog', { name: 'Settings' })
    for (const label of ['General', 'Models', 'Plugins', 'Agent presets', 'Credentials & sign-in', 'About & diagnostics']) {
      assert.equal(await dialog.getByRole('button', { name: label, exact: true }).isVisible(), true)
    }
    await dialog.getByRole('button', { name: 'Close settings' }).click()
    await dialog.waitFor({ state: 'detached' })

    assert.equal(await page.locator('[data-sidebar-new-session]').getAttribute('aria-label'), 'New Session')
    assert.equal(await page.getByRole('button', { name: 'Settings', exact: true }).isVisible(), true)
    assert.equal(await page.getByText('Put ideas in motion', { exact: true }).isVisible(), true)
    assert.equal(await page.getByRole('button', { name: 'Files', exact: true }).isVisible(), true)
    const input = page.getByRole('textbox', { name: 'Enter task' })
    assert.equal(await input.getAttribute('placeholder'), 'Describe what you want to build')
    await page.setViewportSize({ width: 390, height: 430 })
    const onboarding = page.locator('[data-model-onboarding][data-state="empty"]')
    await onboarding.waitFor()
    assert.equal(await onboarding.getByText('Configure a model to continue', { exact: true }).isVisible(), true)
    await input.fill('keep this draft')
    assert.equal(await page.getByRole('button', { name: 'Send' }).isDisabled(), true)
    assert.equal(await page.getByRole('button', { name: 'Send' }).evaluate(button => button.getBoundingClientRect().height >= 40), true)
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth), 390)
    await input.fill('')
    await page.setViewportSize({ width: 1280, height: 800 })
    const model = page.locator('[data-input-bar] [data-model-picker]')
    await model.click()
    assert.equal(await page.getByText('Model for this session', { exact: true }).isVisible(), true)
    await page.keyboard.press('Escape')

    await page.getByRole('button', { name: /^(Choose working folder|Switch workspace for the new session)$/ }).first().click()
    const directoryDialog = page.getByRole('dialog', { name: 'Choose a working folder' })
    await directoryDialog.waitFor()
    assert.equal(await directoryDialog.getByRole('button', { name: 'Edit folder path' }).isVisible(), true)
    assert.equal(await directoryDialog.getByRole('button', { name: 'New folder' }).isVisible(), true)
    assert.equal(await directoryDialog.getByRole('button', { name: 'Show hidden folders' }).isVisible(), true)
    assert.equal(await directoryDialog.getByRole('button', { name: 'Open selected folder' }).isVisible(), true)
    await directoryDialog.getByRole('button', { name: 'Cancel' }).click()
    await directoryDialog.waitFor({ state: 'detached' })

    await page.reload({ waitUntil: 'domcontentloaded' })
    await page.getByRole('textbox', { name: 'Enter task' }).waitFor()
    assert.equal(await page.locator('html').getAttribute('lang'), 'en')
    await page.getByText('Put ideas in motion', { exact: true }).waitFor()
    assert.equal(await page.evaluate(() => localStorage.getItem('ternilo.locale')), 'en')

    await page.getByRole('button', { name: 'Settings', exact: true }).click()
    dialog = page.getByRole('dialog', { name: 'Settings' })
    await selectChoice(dialog.locator('[role="combobox"][aria-label="Language"]'), 'zh')
    dialog = page.getByRole('dialog', { name: '设置' })
    await dialog.getByRole('button', { name: '关闭设置' }).click()
    assert.equal(await page.locator('html').getAttribute('lang'), 'zh-CN')
    assert.equal(await page.getByText('让想法，动起来', { exact: true }).isVisible(), true)
    assert.deepEqual(errors, [])
    console.log('i18n browser acceptance: zh → en → reload → zh passed')
    await context.close()
  } finally {
    if (browser) await browser.close()
    await stopProcess(ternilo.child)
    await rm(dataDirectory, { recursive: true, force: true })
  }
})
