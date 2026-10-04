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
    child.once('error', reject)
    child.once('exit', code => reject(new Error(`Ternilo exited with ${code}: ${diagnostics}`)))
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

async function setVisibleViewport(page, height, offsetTop = 0) {
  await page.evaluate(({ height, offsetTop }) => {
    window.__setTerniloTestVisualViewport({ height, offsetTop, scale: 1 })
  }, { height, offsetTop })
  await page.waitForFunction(({ height, offsetTop }) => (
    getComputedStyle(document.documentElement).getPropertyValue('--ternilo-visual-viewport-height').trim() === `${height}px`
    && getComputedStyle(document.documentElement).getPropertyValue('--ternilo-visual-viewport-top').trim() === `${offsetTop}px`
  ), { height, offsetTop })
}

async function assertNestedSettingsFocus(page, mobile) {
  if (mobile) {
    await page.getByRole('button', { name: '打开侧边栏' }).click()
    await page.locator('[data-app-frame][data-mobile-sidebar-open]').waitFor()
    const mobileClose = page.locator('[data-mobile-sidebar-close]')
    await page.waitForFunction(() => document.activeElement?.hasAttribute('data-mobile-sidebar-close'))
    assert.equal(await mobileClose.evaluate(element => element === document.activeElement), true)
  }
  const settingsTrigger = page.getByRole('button', { name: '设置', exact: true })
  await settingsTrigger.focus()
  await page.keyboard.press('Enter')
  const settings = page.getByRole('dialog', { name: '设置' })
  await settings.waitFor()
  await page.waitForFunction(() => document.activeElement?.getAttribute('aria-label') === '关闭设置')
  await settings.getByRole('button', { name: 'Agent 预设', exact: true }).click()

  const viewTrigger = settings.getByRole('button', { name: '查看: 标准模式' })
  await viewTrigger.waitFor()
  await viewTrigger.focus()
  await page.keyboard.press('Enter')
  const nested = page.getByRole('dialog', { name: '查看 标准模式' })
  await nested.waitFor()
  assert.equal(await nested.evaluate(element => element.contains(document.activeElement)), true)
  await page.keyboard.press('Escape')
  await nested.waitFor({ state: 'detached' })
  await page.waitForFunction(() => document.activeElement?.getAttribute('aria-label') === '查看: 标准模式')

  await page.keyboard.press('Escape')
  await settings.waitFor({ state: 'detached' })
  await page.waitForFunction(() => document.activeElement?.getAttribute('aria-label') === '设置')
  assert.equal(await settingsTrigger.evaluate(element => element === document.activeElement), true)
}

test('visual viewport keeps the mobile composer visible and nested Settings restores focus', { timeout: 60_000 }, async () => {
  const dataDirectory = await mkdtemp(path.join(tmpdir(), 'ternilo-mobile-viewport-'))
  const workspace = path.join(dataDirectory, 'workspace')
  await mkdir(workspace)
  const app = startTernilo(dataDirectory)
  let browser, page
  try {
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
    const context = await browser.newContext({ locale: 'zh-CN', viewport: { width: 1440, height: 900 }, serviceWorkers: 'block' })
    await context.addInitScript(() => {
      const events = new EventTarget()
      const metrics = { height: window.innerHeight, offsetTop: 0, scale: 1 }
      Object.defineProperties(events, {
        height: { configurable: true, get: () => metrics.height },
        offsetTop: { configurable: true, get: () => metrics.offsetTop },
        scale: { configurable: true, get: () => metrics.scale },
      })
      Object.defineProperty(window, 'visualViewport', { configurable: true, value: events })
      window.__setTerniloTestVisualViewport = next => {
        Object.assign(metrics, next)
        events.dispatchEvent(new Event('resize'))
        events.dispatchEvent(new Event('scroll'))
      }
    })
    page = await context.newPage()
    const pageErrors = []
    const consoleErrors = []
    const failedRequests = []
    const serverErrors = []
    page.on('pageerror', error => pageErrors.push(error.message))
    page.on('console', message => {
      if (message.type() === 'error') consoleErrors.push(message.text())
    })
    page.on('requestfailed', request => failedRequests.push(`${request.method()} ${request.url()}: ${request.failure()?.errorText}`))
    page.on('response', response => {
      if (response.status() >= 500) serverErrors.push(`${response.status()} ${response.request().method()} ${response.url()}`)
    })
    await page.goto(await app.origin, { waitUntil: 'networkidle' })
    assert.match(await page.locator('meta[name="viewport"]').getAttribute('content'), /interactive-widget=resizes-content/)
    await assertNestedSettingsFocus(page, false)

    await page.setViewportSize({ width: 390, height: 844 })
    await setVisibleViewport(page, 844)
    await page.waitForFunction(() => document.querySelector('[data-app-frame]')?.hasAttribute('data-mobile'))
    await assertNestedSettingsFocus(page, true)

    await page.locator('.app-sidebar').getByRole('button', { name: '添加工作区' }).click()
    const picker = page.getByRole('dialog', { name: '选择工作文件夹' })
    await picker.getByRole('button', { name: '编辑文件夹路径' }).click()
    const pathEditor = picker.getByRole('textbox', { name: '编辑文件夹路径' })
    await pathEditor.fill(workspace)
    await pathEditor.press('Enter')
    await picker.getByRole('button', { name: '打开所选文件夹' }).click()
    await picker.waitFor({ state: 'detached' })
    const composer = page.locator('.composer-shell')
    await composer.waitFor()
    await page.locator('[data-new-session-hero]').waitFor()
    await page.getByRole('textbox', { name: '输入任务' }).focus()

    await setVisibleViewport(page, 430)
    await page.locator('[data-new-session-hero]').waitFor()
    const keyboardGeometry = await page.evaluate(() => {
      const frame = document.querySelector('[data-app-frame]').getBoundingClientRect()
      const composer = document.querySelector('.composer-shell').getBoundingClientRect()
      const hero = document.querySelector('[data-new-session-hero]').getBoundingClientRect()
      return {
        frame: { top: frame.top, bottom: frame.bottom, height: frame.height },
        composer: { top: composer.top, bottom: composer.bottom },
        hero: { top: hero.top, bottom: hero.bottom },
        documentWidth: document.documentElement.scrollWidth,
        viewportWidth: document.documentElement.clientWidth,
      }
    })
    assert.deepEqual(keyboardGeometry.frame, { top: 0, bottom: 430, height: 430 })
    assert.equal(keyboardGeometry.composer.top >= 0 && keyboardGeometry.composer.bottom <= 430, true, JSON.stringify(keyboardGeometry))
    assert.equal(keyboardGeometry.hero.top >= 0 && keyboardGeometry.hero.bottom <= 430, true, JSON.stringify(keyboardGeometry))
    assert.equal(keyboardGeometry.documentWidth, keyboardGeometry.viewportWidth)

    await setVisibleViewport(page, 430, 72)
    const pannedGeometry = await page.evaluate(() => {
      const frame = document.querySelector('[data-app-frame]').getBoundingClientRect()
      const composer = document.querySelector('.composer-shell').getBoundingClientRect()
      return { frame: { top: frame.top, bottom: frame.bottom }, composer: { top: composer.top, bottom: composer.bottom } }
    })
    assert.deepEqual(pannedGeometry.frame, { top: 72, bottom: 502 })
    assert.equal(pannedGeometry.composer.top >= 72 && pannedGeometry.composer.bottom <= 502, true, JSON.stringify(pannedGeometry))

    await setVisibleViewport(page, 844)
    assert.deepEqual(pageErrors, [])
    assert.deepEqual(consoleErrors, [])
    assert.deepEqual(failedRequests, [])
    assert.deepEqual(serverErrors, [])
  } catch (error) {
    if (page && process.env.TERNILO_E2E_ARTIFACT_DIR) {
      await mkdir(process.env.TERNILO_E2E_ARTIFACT_DIR, { recursive: true })
      await page.screenshot({ path: path.join(process.env.TERNILO_E2E_ARTIFACT_DIR, 'mobile-viewport-failure.png') }).catch(() => {})
    }
    throw new Error(`${error.stack}\nTernilo diagnostics:\n${app.diagnostics()}`, { cause: error })
  } finally {
    await browser?.close()
    await stopProcess(app.child)
    await rm(dataDirectory, { recursive: true, force: true })
  }
})
