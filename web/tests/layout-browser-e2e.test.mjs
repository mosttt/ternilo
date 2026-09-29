import { selectChoice } from './browser-select-fixture.mjs'
import assert from 'node:assert/strict'
import { mkdtemp, rm } from 'node:fs/promises'
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
    child.once('error', reject)
    child.once('exit', code => reject(new Error(`Ternilo web exited with ${code}: ${diagnostics}`)))
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

async function dragSeparator(page, label, dx) {
  const separator = page.getByRole('separator', { name: label })
  const box = await separator.boundingBox()
  assert.ok(box)
  const x = box.x + box.width / 2
  const y = box.y + Math.min(160, box.height / 2)
  await page.mouse.move(x, y)
  await page.mouse.down()
  await page.mouse.move(x + dx, y, { steps: 4 })
  await page.mouse.up()
}

test('Workbench shell owns desktop columns, theme projection, and mobile sidebar drawer', { timeout: 60_000 }, async () => {
  const dataDirectory = await mkdtemp(path.join(tmpdir(), 'ternilo-layout-browser-'))
  const server = startTernilo(dataDirectory)
  const browser = await chromium.launch({ headless: true })
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } })
  const pageErrors = []
  page.on('pageerror', error => pageErrors.push(error.message))

  try {
    await page.goto(await server.origin, { waitUntil: 'domcontentloaded' })
    const frame = page.locator('[data-app-frame]')
    await frame.waitFor()
    await page.locator('[data-app-sidebar-column]').waitFor()
    await page.getByRole('status').filter({ hasText: '正在连接 Ternilo 内核…' }).waitFor({ state: 'hidden' })

    const columns = await page.evaluate(() => {
      const sidebar = document.querySelector('[data-app-sidebar-column]')
      const center = document.querySelector('[data-app-center-column]')
      const details = document.querySelector('[data-app-details-column]')
      if (!(sidebar instanceof HTMLElement) || !(center instanceof HTMLElement) || !(details instanceof HTMLElement)) return null
      const sidebarBox = sidebar.getBoundingClientRect()
      const centerBox = center.getBoundingClientRect()
      return {
        count: [sidebar, center, details].length,
        sidebarWidth: sidebarBox.width,
        detailsWidth: details.getBoundingClientRect().width,
        sidebarHit: document.elementFromPoint(sidebarBox.left + 20, 120)?.closest('[data-app-sidebar-column]') === sidebar,
        centerHit: document.elementFromPoint(centerBox.left + 40, 120)?.closest('[data-app-center-column]') === center,
      }
    })
    assert.deepEqual(columns, {
      count: 3,
      sidebarWidth: 280,
      detailsWidth: 0,
      sidebarHit: true,
      centerHit: true,
    })

    await dragSeparator(page, '调整侧边栏宽度', 60)
    await page.waitForFunction(() => Math.abs(document.querySelector('[data-app-sidebar-column]').getBoundingClientRect().width - 340) < 1)
    await page.getByRole('button', { name: '收起侧边栏' }).click()
    await page.waitForFunction(() => Math.abs(document.querySelector('[data-app-sidebar-column]').getBoundingClientRect().width - 56) < 1)
    assert.equal(await page.getByRole('separator', { name: '调整侧边栏宽度' }).count(), 0)
    await page.getByRole('button', { name: '展开侧边栏' }).click()
    await page.waitForFunction(() => Math.abs(document.querySelector('[data-app-sidebar-column]').getBoundingClientRect().width - 280) < 1)

    await page.getByRole('button', { name: '设置', exact: true }).click()
    const settings = page.getByRole('dialog', { name: '设置' })
    await settings.waitFor()
    const theme = settings.getByRole('combobox', { name: '界面主题' })
    assert.equal(await page.evaluate(() => document.documentElement.dataset.theme), 'system')
    assert.equal(await theme.getAttribute('data-choice-value'), 'system')
    await page.emulateMedia({ colorScheme: 'light' })
    await page.waitForFunction(() => document.documentElement.style.colorScheme === 'light')
    await page.emulateMedia({ colorScheme: 'dark' })
    await page.waitForFunction(() => document.documentElement.style.colorScheme === 'dark')
    await selectChoice(theme, 'light')
    await page.waitForFunction(() => !document.documentElement.classList.contains('dark') && !document.documentElement.classList.contains('dark'))
    const lightBackground = await frame.evaluate(element => getComputedStyle(element).backgroundColor)
    assert.equal(await page.evaluate(() => document.documentElement.style.colorScheme), 'light')

    await selectChoice(theme, 'dark')
    await page.waitForFunction(() => document.documentElement.classList.contains('dark') && document.documentElement.classList.contains('dark'))
    const darkBackground = await frame.evaluate(element => getComputedStyle(element).backgroundColor)
    assert.equal(await page.evaluate(() => document.documentElement.style.colorScheme), 'dark')
    assert.notEqual(lightBackground, darkBackground)
    await page.keyboard.press('Escape')
    await settings.waitFor({ state: 'detached' })
    await page.reload({ waitUntil: 'domcontentloaded' })
    await frame.waitFor()
    await page.waitForFunction(() => document.documentElement.classList.contains('dark') && document.documentElement.classList.contains('dark'))
    assert.equal(await page.evaluate(() => localStorage.getItem('ternilo.theme')), 'dark')
    assert.equal(await page.evaluate(() => document.documentElement.style.colorScheme), 'dark')

    await page.setViewportSize({ width: 390, height: 844 })
    await page.waitForFunction(() => document.querySelector('[data-app-frame]')?.hasAttribute('data-mobile'))
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth), 390)
    const sidebarColumn = page.locator('[data-app-sidebar-column]')
    await page.waitForFunction(() => document.querySelector('[data-app-sidebar-column]').getBoundingClientRect().right <= 0)
    await page.getByRole('button', { name: '打开侧边栏' }).click()
    await page.waitForFunction(() => document.querySelector('[data-app-sidebar-column]').getBoundingClientRect().x >= -1)
    const sidebarBox = await sidebarColumn.boundingBox()
    assert.ok(sidebarBox)
    assert.equal(Math.abs(sidebarBox.x) < 1, true)
    assert.equal(await page.evaluate(() => document.elementFromPoint(100, 120)?.closest('[data-app-sidebar-column]') !== null), true)
    assert.equal(await page.getByRole('separator').count(), 0)
    await page.screenshot({ path: '/tmp/ternilo-shell-mobile.png', fullPage: true })

    await page.locator('[data-mobile-sidebar-backdrop]').click({ position: { x: 350, y: 400 } })
    await page.waitForFunction(() => document.querySelector('[data-app-sidebar-column]').getBoundingClientRect().right <= 0)
    assert.deepEqual(pageErrors, [])
  } finally {
    await browser.close()
    await stopProcess(server.child)
    await rm(dataDirectory, { recursive: true, force: true })
  }
})
