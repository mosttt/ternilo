import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { mkdir, mkdtemp, readFile, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { chromium } from 'playwright'
import { freePort, repository, serverRequest, startProcess, stopProcess, waitForHttp } from './platform-e2e-fixture.mjs'

async function until(read, ready) {
  const deadline = Date.now() + 30_000
  while (Date.now() < deadline) {
    const value = await read()
    if (ready(value)) return value
    await new Promise(resolve => setTimeout(resolve, 100))
  }
  throw new Error('Workbench state did not settle')
}

test('real workbench commands preserve drafts, use keyboard highlights, freeze presets and keep navigation fixed on desktop and mobile', { timeout: 180_000 }, async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-workbench-commands-'))
  const workspacePath = path.join(directory, 'workspace')
  const artifacts = process.env.TERNILO_E2E_ARTIFACT_DIR
  const origin = `http://127.0.0.1:${await freePort()}`
  await mkdir(workspacePath)
  const service = startProcess(process.env.TERNILO_E2E_LOCAL_BINARY ?? path.join(repository, 'target/debug/ternilo'), [
    'serve', '--listen', new URL(origin).host, '--data-dir', path.join(directory, 'local'),
  ], { XDG_STATE_HOME: path.join(directory, 'state') })
  let browser, page
  const errors = []
  try {
    await waitForHttp(origin, service)
    const html = await (await fetch(origin)).text()
    const token = JSON.parse(html.match(/window\.__TERNILO_BOOT__\s*=\s*(\{[^<]+\});/)[1]).apiToken
    const local = (resource, options = {}) => serverRequest(origin, resource, { token, ...options })
    for (const asset of ['app.js', 'app.css']) {
      const served = Buffer.from(await (await fetch(`${origin}/assets/${asset}`)).arrayBuffer())
      assert.equal(createHash('sha256').update(served).digest('hex'), createHash('sha256').update(await readFile(path.join(repository, 'web/dist/assets', asset))).digest('hex'), 'browser uses the actual embedded build')
    }
    const workspace = await local('/workspaces', { body: { path: workspacePath } })
    const session = await local('/sessions', { body: { workspace_id: workspace.workspace_id } })
    const sessionId = session.identity.session_id
    const resource = `/sessions/${sessionId}`
    await local(resource, { method: 'PATCH', body: { agent_preset: 'minimal' } })
    await local(resource, { method: 'PATCH', body: { agent_preset: 'standard' } })
    for (let index = 0; index < 16; index++) {
      await local(`${resource}/queue`, { body: { content: { kind: 'prompt', input: `/code "Navigation turn ${index + 1}"` } } })
      await until(() => local(`${resource}/queue`), inbox => !inbox.active_run_id && inbox.items.length === 0)
    }
    await assert.rejects(local(resource, { method: 'PATCH', body: { agent_preset: 'minimal' } }), /preset is locked/)
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE })
    page = await browser.newPage({ viewport: { width: 1500, height: 950 }, serviceWorkers: 'block' })
    page.on('pageerror', error => errors.push(error.message))
    page.on('console', message => { if (message.type() === 'error') errors.push(message.text()) })
    page.on('response', response => { if (response.status() >= 400) errors.push(`${response.status()} ${new URL(response.url()).pathname}`) })
    await page.goto(origin)
    await page.locator(`[data-sidebar-session-row][data-session-id="${sessionId}"]`).locator('button').first().click()
    const input = page.locator('[data-composer-input]')
    await input.waitFor()
    const header = page.locator('.session-header')
    assert.equal(await header.getByRole('button', { name: /^Agent 预设:/ }).isDisabled(), true)
    assert.equal(await header.locator('button[aria-label="切换到计划模式"]').count(), 0)
    assert.equal(await header.locator('.lucide-files').count(), 0)
    assert.equal(await page.locator('[data-session-workspace]').innerText(), workspacePath)
    assert.ok((await header.boundingBox()).height < 95)

    await input.fill('/')
    const menu = page.locator('[data-composer-menu]')
    await menu.getByRole('option').first().waitFor()
    assert.deepEqual(await menu.locator('code').allTextContents(), ['/compact', '/export', '/feedback', '/goal', '/permission', '/plan', '/model'])
    const initial = await menu.locator('[aria-selected="true"]').getAttribute('id')
    await input.press('ArrowDown')
    assert.notEqual(await input.getAttribute('aria-activedescendant'), initial)
    assert.equal(await input.getAttribute('aria-activedescendant'), await menu.locator('[aria-selected="true"]').getAttribute('id'))
    const colors = await menu.locator('[aria-selected="true"]').evaluate(element => ({ selected: getComputedStyle(element.parentElement).backgroundColor, menu: getComputedStyle(element.closest('[data-composer-menu]')).backgroundColor }))
    assert.equal(colors.menu, 'rgb(53, 54, 56)')
    assert.equal(colors.selected, 'rgba(255, 255, 255, 0.08)')
    await input.press('Escape')
    assert.equal(await input.inputValue(), '/')
    await input.fill('.')
    await menu.getByRole('option').first().waitFor()
    assert.ok((await menu.locator('code').allTextContents()).every(value => value.startsWith('.')))
    const scroller = page.locator('[data-conversation-scroll]')
    const scrollBefore = await scroller.evaluate(element => element.scrollTop)
    for (let index = 0; index < 14; index++) await input.press('ArrowDown')
    const geometry = await menu.locator('[aria-selected="true"]').evaluate(element => {
      const viewport = element.closest('[role="listbox"]')
      const selected = element.getBoundingClientRect(), bounds = viewport.getBoundingClientRect()
      return { visible: selected.top >= bounds.top - 1 && selected.bottom <= bounds.bottom + 1, scrollTop: viewport.scrollTop }
    })
    assert.equal(geometry.visible, true)
    assert.ok(geometry.scrollTop > 0)
    assert.equal(await scroller.evaluate(element => element.scrollTop), scrollBefore)
    await input.press('Escape')
    for (const text of ['.env', './folder', '../folder']) {
      await input.fill(text)
      assert.equal(await menu.count(), 0)
    }

    const beforeActions = await local(`${resource}/events`)
    await input.fill('An independent unsent draft')
    await page.getByRole('button', { name: '会话指令（/）', exact: true }).click()
    await menu.getByRole('option').filter({ hasText: '/model' }).click()
    await page.getByRole('menu').waitFor()
    assert.equal(await input.inputValue(), 'An independent unsent draft')
    await page.keyboard.press('Escape')
    await input.fill('/permission')
    await input.press('Enter')
    await page.getByRole('menuitemradio', { name: '只读', exact: true }).waitFor()
    await page.keyboard.press('Escape')
    await input.fill('/plan')
    await input.press('Enter')
    await until(() => local('/state'), state => state.sessions.find(item => item.identity.session_id === sessionId)?.mode === 'plan')
    await input.fill('/plan')
    await menu.getByRole('option').filter({ hasText: '切换到执行模式' }).waitFor()
    await input.press('Enter')
    await until(() => local('/state'), state => state.sessions.find(item => item.identity.session_id === sessionId)?.mode === 'execute')
    assert.equal((await local(`${resource}/events`)).filter(event => event.type === 'user_message').length, beforeActions.filter(event => event.type === 'user_message').length)
    await input.fill('.agents')
    await input.press('Tab')
    assert.equal(await menu.count(), 0)
    const request = page.waitForRequest(request => request.method() === 'POST' && new URL(request.url()).pathname.endsWith('/queue'))
    await input.press('Enter')
    assert.equal((await request).postDataJSON().content.input, '/agents')
    await until(() => local(`${resource}/queue`), inbox => !inbox.active_run_id && inbox.items.length === 0)

    const rail = page.locator('[data-turn-navigator] nav')
    await rail.waitFor()
    const railBefore = await rail.boundingBox()
    const column = await page.locator('.conversation-column').boundingBox()
    assert.ok(Math.abs(column.x + column.width - railBefore.x - railBefore.width - 12) < 2, JSON.stringify({ column, railBefore }))
    const scrollBounds = await scroller.boundingBox()
    await page.mouse.move(scrollBounds.x + 100, scrollBounds.y + 100)
    await page.mouse.wheel(0, -100000)
    await until(() => scroller.evaluate(element => element.scrollTop), value => value < 1)
    const railAfter = await rail.boundingBox()
    assert.ok(Math.abs(railBefore.y - railAfter.y) < 2)
    await until(() => rail.locator('[aria-current="true"]').getAttribute('aria-label'), value => value === '跳转到第 1 轮')
    await until(() => rail.locator('[aria-current="true"]').evaluate(element => getComputedStyle(element, '::before').width), value => value === '20px')
    await rail.locator('[data-turn-navigator-mark]').nth(4).hover()
    await page.locator('[data-turn-navigator-preview]').waitFor()
    await rail.locator('[data-turn-navigator-mark]').nth(4).click()
    await until(() => rail.locator('[aria-current="true"]').getAttribute('aria-label'), value => value === '跳转到第 5 轮')
    if (artifacts) { await mkdir(artifacts, { recursive: true }); await page.screenshot({ path: path.join(artifacts, 'workbench-desktop.png') }) }
    await page.setViewportSize({ width: 390, height: 844 })
    await page.waitForFunction(() => document.querySelector('[data-app-frame][data-mobile="true"]'))
    await page.keyboard.press('Escape')
    await page.waitForFunction(() => !document.querySelector('[data-app-frame][data-mobile-sidebar-open]'))
    const tabs = page.locator('.session-header [role="tablist"]')
    assert.equal(await tabs.isVisible(), true)
    assert.equal(await page.locator('[data-session-workspace]').isVisible(), true)
    assert.ok((await tabs.boundingBox()).y > (await page.locator('[data-session-workspace]').boundingBox()).y)
    await page.locator('#conversation-view-trajectory-tab').click()
    await page.locator('[data-conversation-view="trajectory"]:not([hidden])').waitFor()
    await page.locator('#conversation-view-chat-tab').click()
    assert.equal(await rail.isVisible(), false)
    assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth))
    if (artifacts) await page.screenshot({ path: path.join(artifacts, 'workbench-mobile.png') })
    await page.reload()
    await input.waitFor()
    await assert.rejects(local(resource, { method: 'PATCH', body: { agent_preset: 'creative' } }), /preset is locked/)
    await page.evaluate(() => localStorage.setItem('ternilo.theme', 'light'))
    await page.reload()
    await input.fill('/')
    await menu.getByRole('option').first().waitFor()
    assert.equal(await menu.evaluate(element => getComputedStyle(element).backgroundColor), 'rgb(255, 255, 255)')
    assert.equal(await menu.locator('[aria-selected="true"]').evaluate(element => getComputedStyle(element.parentElement).backgroundColor), 'rgba(38, 49, 72, 0.06)')
    assert.equal(await page.getByRole('button', { name: '回到底部', exact: true }).isVisible(), false)
    if (artifacts) await page.screenshot({ path: path.join(artifacts, 'workbench-mobile-light.png') })
    assert.deepEqual(errors, [])
  } catch (error) {
    if (page && artifacts) { await mkdir(artifacts, { recursive: true }); await page.screenshot({ path: path.join(artifacts, 'workbench-failure.png') }).catch(() => {}) }
    throw new Error(`${error.stack}\n${service.diagnostics()}`)
  } finally {
    await browser?.close()
    await stopProcess(service)
    await rm(directory, { recursive: true, force: true })
  }
})
