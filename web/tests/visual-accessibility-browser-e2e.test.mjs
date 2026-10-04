import assert from 'node:assert/strict'
import { mkdir, mkdtemp, rm } from 'node:fs/promises'
import { createServer } from 'node:http'
import { tmpdir } from 'node:os'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import { spawn } from 'node:child_process'
import test from 'node:test'
import axe from 'axe-core'
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

function sse(payload) { return `data: ${JSON.stringify(payload)}\n\n` }

async function startModelFixture() {
  let requestCount = 0
  const server = createServer((request, response) => {
    if (request.method !== 'POST' || request.url !== '/v1/responses') {
      response.writeHead(404).end()
      return
    }
    request.resume()
    request.on('end', () => {
      response.writeHead(200, { 'content-type': 'text/event-stream', 'cache-control': 'no-cache' })
      if (requestCount++ === 0) {
        const call = {
          type: 'function_call',
          call_id: 'visual-focus-echo',
          name: 'echo',
          arguments: '{"text":"visual accessibility ready"}',
        }
        response.end([
          sse({ type: 'response.output_item.added', output_index: 0, item: call }),
          sse({
            type: 'response.completed',
            response: { status: 'completed', output: [call], usage: { input_tokens: 10, output_tokens: 2 } },
          }),
        ].join(''))
        return
      }
      response.end([
        sse({ type: 'response.output_text.delta', output_index: 0, content_index: 0, delta: 'Visual accessibility ready.' }),
        sse({
          type: 'response.completed',
          response: { status: 'completed', output: [], usage: { input_tokens: 6, output_tokens: 4 } },
        }),
      ].join(''))
    })
  })
  await new Promise((resolve, reject) => {
    server.once('error', reject)
    server.listen(0, '127.0.0.1', resolve)
  })
  return {
    baseUrl: `http://127.0.0.1:${server.address().port}/v1`,
    close: () => new Promise((resolve, reject) => server.close(error => error ? reject(error) : resolve())),
  }
}

async function apiRequest(page, pathValue, method, bodyValue) {
  return page.evaluate(async ({ pathValue, method, bodyValue }) => {
    const token = window.__TERNILO_BOOT__?.apiToken
    const response = await fetch(`/api/v1${pathValue}`, {
      method,
      headers: { 'content-type': 'application/json', ...(token ? { authorization: `Bearer ${token}` } : {}) },
      body: bodyValue === undefined ? undefined : JSON.stringify(bodyValue),
    })
    if (!response.ok) throw new Error(`${method} ${pathValue}: ${response.status} ${await response.text()}`)
    return response.status === 204 ? null : response.json()
  }, { pathValue, method, bodyValue })
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
}

async function configureProvider(page, baseUrl) {
  await apiRequest(page, '/credentials', 'POST', {
    name: 'TERNILO_PROVIDER_VISUAL_API_KEY', value: 'visual-key',
  })
  await apiRequest(page, '/providers', 'POST', {
    id: 'visual-fixture',
    display_name: 'Visual Fixture',
    base_url: baseUrl,
    protocol: 'openai-responses',
    api_key_ref: 'TERNILO_PROVIDER_VISUAL_API_KEY',
    defaults: { context_window: 32_000, max_output_tokens: 2_048 },
    models: [{ id: 'visual-model', display_name: 'Visual Model', settings: { mode: 'inherit' } }],
    timeout_ms: 30_000,
    max_attempts: 1,
    retry_base_delay_ms: 10,
  })
  const sessionId = await page.evaluate(() => localStorage.getItem('ternilo.current-session'))
  await apiRequest(page, `/sessions/${encodeURIComponent(sessionId)}`, 'PATCH', {
    model: { provider: 'named_provider', provider_id: 'visual-fixture', model: 'visual-model' },
  })
  await page.reload({ waitUntil: 'domcontentloaded' })
  await page.getByRole('button', { name: /Visual Model/ }).waitFor()
}

async function axeSeriousCritical(page, state) {
  if (!await page.evaluate(() => Boolean(window.axe))) await page.addScriptTag({ content: axe.source })
  const violations = await page.evaluate(async () => {
    const result = await window.axe.run(document)
    return result.violations.filter(violation => ['serious', 'critical'].includes(violation.impact)).map(violation => ({
      id: violation.id,
      impact: violation.impact,
      targets: violation.nodes.map(node => node.target),
    }))
  })
  assert.deepEqual(violations, [], `${state} axe violations:\n${JSON.stringify(violations, null, 2)}`)
}

async function focusEvidence(locator) {
  return locator.evaluate(element => {
    const style = getComputedStyle(element)
    return {
      active: document.activeElement === element,
      focusVisible: element.matches(':focus-visible'),
      outlineStyle: style.outlineStyle,
      outlineWidth: style.outlineWidth,
      boxShadow: style.boxShadow,
      highlighted: element.hasAttribute('data-highlighted'),
      backgroundColor: style.backgroundColor,
    }
  })
}

function assertVisibleFocus(evidence, label) {
  assert.equal(evidence.active, true, `${label} does not own document focus`)
  assert.equal(evidence.focusVisible, true, `${label} does not match :focus-visible`)
  const outlined = evidence.outlineStyle !== 'none' && Number.parseFloat(evidence.outlineWidth) >= 1
  const shadowed = evidence.boxShadow !== 'none'
  const highlighted = evidence.highlighted && !['transparent', 'rgba(0, 0, 0, 0)'].includes(evidence.backgroundColor)
  assert.equal(outlined || shadowed || highlighted, true, `${label} has no visible focus treatment: ${JSON.stringify(evidence)}`)
}

function transparent(value) {
  return value === 'transparent' || /^rgba\([^)]*,\s*0\)$/.test(value)
}

async function settleAnimations(locator) {
  await locator.evaluate(async element => {
    await Promise.all(element.getAnimations({ subtree: true }).map(animation => animation.finished))
  })
}

test('Ternilo visual tokens, scrollbars, focus return, and serious accessibility hold in Chromium', { timeout: 180_000 }, async () => {
  const dataDirectory = await mkdtemp(path.join(tmpdir(), 'ternilo-visual-accessibility-'))
  const workspace = path.join(dataDirectory, 'workspace')
  await mkdir(workspace, { recursive: true })
  const model = await startModelFixture()
  const ternilo = startTernilo(dataDirectory)
  let browser
  try {
    const origin = await ternilo.origin
    browser = await chromium.launch({ headless: true })
    const page = await browser.newPage({ locale: 'zh-CN', viewport: { width: 1440, height: 900 } })
    const pageErrors = []
    page.on('pageerror', error => pageErrors.push(error.message))
    await page.goto(origin, { waitUntil: 'networkidle' })
    await chooseWorkspace(page, workspace)
    await configureProvider(page, model.baseUrl)

    const input = page.getByRole('textbox', { name: '输入任务' })
    await input.fill('验证视觉和键盘契约')
    await page.getByRole('button', { name: '发送' }).click()
    await page.getByText('Visual accessibility ready.', { exact: true }).waitFor()
    await axeSeriousCritical(page, 'desktop conversation')

    const semanticSurfaceContract = await page.evaluate(() => {
      const body = getComputedStyle(document.body)
      const read = (selector, property) => getComputedStyle(document.querySelector(selector))[property]
      const resolveColor = token => {
        const probe = document.createElement('span')
        probe.style.color = `var(${token})`
        document.body.append(probe)
        const value = getComputedStyle(probe).color
        probe.remove()
        return value
      }
      return {
        bodyBackgroundMatchesBase: body.backgroundColor === resolveColor('--background'),
        bodyColorMatchesPrimaryLabel: body.color === resolveColor('--foreground'),
        sidebarBackgroundMatchesToken: read('.app-sidebar', 'backgroundColor') === resolveColor('--sidebar'),
        composerBackgroundMatchesToken: read('[data-composer-card]', 'backgroundColor') === resolveColor('--surface-input'),
        composerRadius: read('[data-composer-card]', 'borderRadius'),
        sidebarFontSize: read('.app-sidebar', 'fontSize'),
      }
    })
    assert.deepEqual(semanticSurfaceContract, {
      bodyBackgroundMatchesBase: true,
      bodyColorMatchesPrimaryLabel: true,
      sidebarBackgroundMatchesToken: true,
      composerBackgroundMatchesToken: true,
      composerRadius: '18px',
      sidebarFontSize: '14px',
    })

    const settingsTrigger = page.getByRole('button', { name: '设置', exact: true })
    await settingsTrigger.focus()
    await page.keyboard.press('Enter')
    const settingsDialog = page.getByRole('dialog', { name: '设置' })
    const settingsClose = settingsDialog.getByRole('button', { name: '关闭设置' })
    await settingsClose.waitFor()
    await settleAnimations(settingsDialog)
    await page.waitForFunction(() => document.activeElement?.getAttribute('aria-label') === '关闭设置')
    assertVisibleFocus(await focusEvidence(settingsClose), 'Settings close button')
    const settingsSurface = await settingsDialog.evaluate(element => {
      const style = getComputedStyle(element)
      const probe = document.createElement('span')
      probe.style.color = 'var(--surface-layer-2)'
      document.body.append(probe)
      const layer2 = getComputedStyle(probe).color
      probe.remove()
      return {
        backgroundMatchesLayer2: style.backgroundColor === layer2,
        borderVisible: style.borderTopStyle !== 'none' && Number.parseFloat(style.borderTopWidth) >= 1,
      }
    })
    assert.deepEqual(settingsSurface, { backgroundMatchesLayer2: true, borderVisible: true })
    await axeSeriousCritical(page, 'Settings dialog')
    await page.keyboard.press('Escape')
    await settingsDialog.waitFor({ state: 'detached' })
    await page.waitForFunction(() => document.activeElement?.getAttribute('aria-label') === '设置')
    assertVisibleFocus(await focusEvidence(settingsTrigger), 'Settings trigger after close')

    const menuTrigger = page.getByRole('button', { name: '视图选项' })
    await menuTrigger.focus()
    await page.keyboard.press('Enter')
    const menu = page.getByRole('menu')
    await menu.waitFor()
    await settleAnimations(menu)
    const focusedMenuItem = page.locator('[role="menuitem"][data-highlighted]').first()
    await focusedMenuItem.waitFor()
    assertVisibleFocus(await focusEvidence(focusedMenuItem), 'Dropdown menu item')
    const menuSurface = await menu.evaluate(element => {
      const style = getComputedStyle(element)
      return {
        backgroundOpaque: !['transparent', 'rgba(0, 0, 0, 0)'].includes(style.backgroundColor),
        borderVisible: style.borderTopStyle !== 'none' && Number.parseFloat(style.borderTopWidth) >= 1,
        shadowVisible: style.boxShadow !== 'none',
      }
    })
    assert.deepEqual(menuSurface, { backgroundOpaque: true, borderVisible: true, shadowVisible: true })
    await axeSeriousCritical(page, 'workspace view menu')
    await page.keyboard.press('Escape')
    await menu.waitFor({ state: 'detached' })
    await page.waitForFunction(() => document.activeElement?.getAttribute('aria-label') === '视图选项')
    assertVisibleFocus(await focusEvidence(menuTrigger), 'Dropdown trigger after close')

    const processControl = page.locator('[data-turn-process]').first()
    if (await processControl.count() && await processControl.getAttribute('aria-expanded') === 'false') await processControl.click()
    const inspect = page.locator('[data-tool-call-inspect]').first()
    await inspect.focus()
    await page.keyboard.press('Enter')
    const details = page.getByRole('complementary', { name: '详情' })
    const detailsClose = details.getByRole('button', { name: '关闭详情' })
    await detailsClose.waitFor()
    await page.waitForFunction(() => document.activeElement?.getAttribute('aria-label') === '关闭详情')
    assertVisibleFocus(await focusEvidence(detailsClose), 'Details close button')
    const detailsSurface = await details.evaluate(element => {
      const probe = document.createElement('span')
      probe.style.color = 'var(--surface-layer-1)'
      document.body.append(probe)
      const expected = getComputedStyle(probe).color
      probe.remove()
      return { actual: getComputedStyle(element).backgroundColor, expected }
    })
    assert.equal(detailsSurface.actual, detailsSurface.expected, 'Details owns its layer-1 surface')
    await axeSeriousCritical(page, 'Details panel')
    await page.keyboard.press('Enter')
    await details.waitFor({ state: 'detached' })
    await page.waitForFunction(() => document.activeElement?.hasAttribute('data-tool-call-inspect'))
    assertVisibleFocus(await focusEvidence(inspect), 'Details invoker after close')

    const sourceSession = await page.evaluate(() => localStorage.getItem('ternilo.current-session'))
    for (let index = 0; index < 28; index += 1) {
      await apiRequest(page, `/sessions/${encodeURIComponent(sourceSession)}/fork`, 'POST', {})
    }
    await page.reload({ waitUntil: 'domcontentloaded' })
    await page.getByRole('button', { name: /其余 \d+ 个会话/ }).click()
    await page.locator('[data-sidebar-session-row]').nth(20).waitFor()
    const sidebar = page.getByRole('complementary', { name: '会话侧边栏' })
    const sidebarList = page.getByRole('tree', { name: '工作区与会话' })
    const scrollbarGeometry = await sidebarList.evaluate(element => ({
      cssWidth: getComputedStyle(element, '::-webkit-scrollbar').width,
      cssHeight: getComputedStyle(element, '::-webkit-scrollbar').height,
      occupiedInlineWidth: element.offsetWidth - element.clientWidth,
      overflowing: element.scrollHeight > element.clientHeight,
    }))
    assert.deepEqual(scrollbarGeometry, {
      cssWidth: '8px', cssHeight: '8px', occupiedInlineWidth: 8, overflowing: true,
    })

    await page.mouse.move(900, 400)
    await page.waitForTimeout(2_100)
    const quietThumb = await sidebarList.evaluate(element => getComputedStyle(element, '::-webkit-scrollbar-thumb').backgroundColor)
    assert.equal(transparent(quietThumb), true, `quiet scrollbar thumb is ${quietThumb}`)
    await sidebar.hover()
    const activeThumb = await sidebarList.evaluate(element => getComputedStyle(element, '::-webkit-scrollbar-thumb').backgroundColor)
    assert.equal(transparent(activeThumb), false, `hovered scrollbar thumb is ${activeThumb}`)
    await page.mouse.move(900, 400)
    await page.waitForTimeout(1_850)
    const lingeringThumb = await sidebarList.evaluate(element => getComputedStyle(element, '::-webkit-scrollbar-thumb').backgroundColor)
    assert.equal(lingeringThumb, activeThumb)
    await page.waitForTimeout(350)
    const hiddenThumb = await sidebarList.evaluate(element => getComputedStyle(element, '::-webkit-scrollbar-thumb').backgroundColor)
    assert.equal(transparent(hiddenThumb), true, `hidden scrollbar thumb is ${hiddenThumb}`)

    await page.setViewportSize({ width: 761, height: 900 })
    await page.waitForFunction(() => !document.querySelector('[data-app-frame]')?.hasAttribute('data-mobile'))
    await page.setViewportSize({ width: 760, height: 900 })
    await page.waitForFunction(() => document.querySelector('[data-app-frame]')?.hasAttribute('data-mobile'))
    assert.equal(await page.locator('[data-app-sidebar-column]').getAttribute('inert'), '')

    const mobileTrigger = page.getByRole('button', { name: '打开侧边栏' }).first()
    await mobileTrigger.focus()
    await page.keyboard.press('Enter')
    await page.waitForFunction(() => document.querySelector('[data-app-frame]')?.hasAttribute('data-mobile-sidebar-open'))
    const mobileClose = page.locator('[data-mobile-sidebar-close]')
    await page.waitForFunction(() => document.activeElement?.hasAttribute('data-mobile-sidebar-close'))
    assertVisibleFocus(await focusEvidence(mobileClose), 'Mobile drawer close button')
    assert.equal(await page.locator('[data-app-center-column]').getAttribute('inert'), '')
    assert.equal(await page.locator('[data-app-sidebar-column]').getAttribute('inert'), null)

    const drawerFocusableCount = await page.locator('[data-app-sidebar-column]').evaluate(sidebarElement => {
      const candidates = Array.from(sidebarElement.querySelectorAll(
        'button:not([disabled]), a[href], input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])',
      )).filter(element => !element.hidden && element.getClientRects().length > 0)
      candidates[candidates.length - 1].focus()
      return candidates.length
    })
    assert.ok(drawerFocusableCount > 1)
    await page.keyboard.press('Tab')
    assert.equal(await page.evaluate(() => document.querySelector('[data-app-sidebar-column]')?.contains(document.activeElement)), true)
    await page.keyboard.press('Shift+Tab')
    assert.equal(await page.evaluate(() => document.querySelector('[data-app-sidebar-column]')?.contains(document.activeElement)), true)
    await axeSeriousCritical(page, 'mobile sidebar drawer')
    await page.keyboard.press('Escape')
    await page.waitForFunction(() => !document.querySelector('[data-app-frame]')?.hasAttribute('data-mobile-sidebar-open'))
    await page.waitForFunction(() => document.activeElement?.getAttribute('aria-label') === '打开侧边栏')
    assertVisibleFocus(await focusEvidence(mobileTrigger), 'Mobile drawer trigger after close')
    assert.equal(await page.locator('[data-app-sidebar-column]').getAttribute('inert'), '')
    assert.equal(await page.locator('[data-app-center-column]').getAttribute('inert'), null)
    await axeSeriousCritical(page, 'closed mobile shell')
    assert.deepEqual(pageErrors, [])
  } catch (cause) {
    throw new Error(`${cause instanceof Error ? cause.stack ?? cause.message : String(cause)}\nserver: ${ternilo.diagnostics()}`)
  } finally {
    if (browser) await browser.close()
    await stopProcess(ternilo.child)
    await model.close()
    await rm(dataDirectory, { recursive: true, force: true })
  }
})
