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

function gate() {
  let release
  return {
    promise: new Promise(resolve => { release = resolve }),
    release: () => release(),
  }
}

test('fork exposes shared two-stage progress from message, sidebar, desktop and mobile', { timeout: 120_000 }, async () => {
  const dataDirectory = await mkdtemp(path.join(tmpdir(), 'ternilo-fork-progress-'))
  const workspacePath = path.join(dataDirectory, 'workspace')
  await mkdir(workspacePath)
  const ternilo = startTernilo(dataDirectory)
  let browser
  try {
    const origin = await ternilo.origin
    browser = await chromium.launch({ headless: true })
    const context = await browser.newContext({ viewport: { width: 1280, height: 820 } })
    const page = await context.newPage()
    const pageErrors = []
    page.on('pageerror', error => pageErrors.push(error.message))
    await page.goto(origin, { waitUntil: 'networkidle' })

    const workspace = await api(page, '/workspaces', { method: 'POST', body: { path: workspacePath } })
    const source = await api(page, '/sessions', { method: 'POST', body: {
      workspace_id: workspace.workspace_id,
      agent_preset: 'standard',
    } })
    const sourceId = source.identity.session_id
    await api(page, `/sessions/${encodeURIComponent(sourceId)}/turns`, {
      method: 'POST', body: { run_id: 'fork-progress-turn', input: 'progress source', attachments: [] },
    })
    let nextForkGate = null
    let nextHydrationGate = null
    await page.routeWebSocket('**/api/v1/live', liveSocket => {
      const server = liveSocket.connectToServer()
      server.onMessage(async message => {
        const frame = typeof message === 'string' ? JSON.parse(message) : null
        const blocked = frame?.type === 'event_batch' && frame.reset ? nextHydrationGate : null
        if (blocked) {
          nextHydrationGate = null
          await blocked.promise
        }
        liveSocket.send(message)
      })
    })
    await page.evaluate(id => localStorage.setItem('ternilo.current-session', id), sourceId)
    await page.reload({ waitUntil: 'domcontentloaded' })
    const sourceAssistant = page.locator('article[data-role="assistant"]').last()
    await sourceAssistant.waitFor()
    const sourceTurnTail = page.locator('[data-turn-tail]').last()
    await sourceTurnTail.waitFor()

    await page.route('**/api/v1/sessions/*/fork', async route => {
      const blocked = nextForkGate
      nextForkGate = null
      if (blocked) await blocked.promise
      await route.continue()
    })
    const creating = gate()
    const hydrating = gate()
    nextForkGate = creating
    nextHydrationGate = hydrating
    await sourceTurnTail.getByRole('button', { name: '在新对话中分支' }).click()

    const creatingState = page.locator('[data-history-state="forking"]')
    await creatingState.getByText('正在创建分叉…', { exact: true }).waitFor()
    assert.match(await creatingState.textContent(), /正在复制完整历史并建立新会话/)
    assert.equal(await page.locator('[data-fork-progress="creating"]').count(), 0, 'current Session must not duplicate the center progress state')

    creating.release()
    const hydrationState = page.locator('[data-history-state="loading"]')
    await hydrationState.getByText('正在打开分叉会话…', { exact: true }).waitFor()
    assert.match(await hydrationState.textContent(), /不会重新运行模型/)
    assert.equal(await page.locator('[data-model-onboarding]').count(), 0, 'Provider loading must not compete with history hydration')

    hydrating.release()
    await page.locator('article[data-role="assistant"]').last().waitFor()
    await hydrationState.waitFor({ state: 'detached' })
    const firstChildId = await page.evaluate(() => localStorage.getItem('ternilo.current-session'))
    assert.ok(firstChildId)
    assert.notEqual(firstChildId, sourceId)

    const secondCreating = gate()
    nextForkGate = secondCreating
    const sourceRow = page.locator(`[data-sidebar-session-row][data-session-id="${sourceId}"]`)
    await sourceRow.hover()
    await sourceRow.getByRole('button', { name: /的操作$/ }).click()
    await page.getByRole('menuitem', { name: '分叉会话' }).click()

    const globalProgress = page.locator('[data-fork-progress="creating"]')
    await globalProgress.getByText('正在创建分叉…', { exact: true }).waitFor()
    await page.setViewportSize({ width: 390, height: 844 })
    const mobileBox = await globalProgress.boundingBox()
    assert.ok(mobileBox)
    assert.equal(mobileBox.x >= 0 && mobileBox.x + mobileBox.width <= 390, true)
    assert.equal(mobileBox.y >= 0 && mobileBox.y + mobileBox.height <= 844, true)

    secondCreating.release()
    await globalProgress.waitFor({ state: 'detached' })
    await page.locator('article[data-role="assistant"]').last().waitFor()
    assert.deepEqual(pageErrors, [])
    console.log('Fork two-stage progress desktop/mobile acceptance passed')
  } finally {
    if (browser) await browser.close()
    await stopProcess(ternilo.child)
    await rm(dataDirectory, { recursive: true, force: true })
  }
})
