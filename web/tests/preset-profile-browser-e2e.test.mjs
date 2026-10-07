import assert from 'node:assert/strict'
import { mkdir, mkdtemp, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { chromium } from 'playwright'
import { freePort, repository, startProcess, stopProcess, waitForHttp } from './platform-e2e-fixture.mjs'
import { localApi } from './model-device-fixture.mjs'
import { selectChoice } from './browser-select-fixture.mjs'

test('preset switches retain their shape and complete previews preserve editable inheritance', { timeout: 120_000 }, async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-preset-profile-'))
  const artifacts = process.env.TERNILO_E2E_ARTIFACT_DIR ?? directory
  await mkdir(artifacts, { recursive: true })
  const binary = process.env.TERNILO_E2E_NODE_BINARY ?? path.join(repository, 'target/debug/ternilo')
  const origin = `http://127.0.0.1:${await freePort()}`
  const app = startProcess(binary, ['serve', '--listen', new URL(origin).host, '--data-dir', path.join(directory, 'data')])
  let browser, page
  const errors = []
  try {
    await waitForHttp(origin, app)
    const api = await localApi(origin)
    const folder = path.join(directory, 'workspace')
    await mkdir(folder)
    const workspace = await api('/workspaces', { body: { path: folder } })
    await api('/sessions', { body: { workspace_id: workspace.workspace_id, agent_preset: 'standard' } })
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
    page = await browser.newPage({ locale: 'zh-CN', viewport: { width: 1440, height: 900 }, colorScheme: 'light', serviceWorkers: 'block' })
    page.on('pageerror', error => errors.push(error.message))
    page.on('console', message => { if (message.type() === 'error') errors.push(message.text()) })
    page.on('response', response => { if (response.status() >= 400) errors.push(`HTTP ${response.status()} ${new URL(response.url()).pathname}`) })
    await page.goto(origin)
    await page.getByRole('button', { name: '设置', exact: true }).click()
    const settings = page.getByRole('dialog').filter({ has: page.locator('[data-settings-toolbar]') })
    await settings.getByRole('button', { name: 'Agent 预设', exact: true }).click()
    await settings.getByRole('button', { name: '复制预设: 标准模式', exact: true }).click()
    const copy = page.getByRole('dialog', { name: '复制预设 · 标准模式', exact: true })
    await copy.locator('#preset-copy-id').fill('profile-ui-fixture')
    await copy.locator('#preset-copy-name').fill('Profile UI fixture')
    await copy.getByRole('button', { name: '创建预设', exact: true }).click()
    await copy.waitFor({ state: 'detached' })
    const before = await api('/agent-presets/profile-ui-fixture')
    assert.ok(before.base_profile.plugins.length > 1)
    assert.equal(before.profile.plugins.length, 1)
    const inherited = before.base_profile.plugins.find(entry => !before.profile.plugins.some(override => override.id === entry.id))
    const card = settings.locator('article').filter({ hasText: 'profile-ui-fixture' })
    await card.getByRole('button', { name: '编辑: Profile UI fixture', exact: true }).click()
    let editor = page.getByRole('dialog', { name: '编辑 Profile UI fixture', exact: true })
    await editor.locator('details > summary').click()
    const raw = editor.locator('#preset-edit-profile')
    const complete = editor.locator('#preset-effective-profile')
    assert.equal(await complete.getAttribute('readonly'), '')
    assert.deepEqual(JSON.parse(await raw.inputValue()), before.profile)
    assert.ok(JSON.parse(await complete.inputValue()).plugins.length > before.profile.plugins.length)
    const toggle = editor.locator(`[data-plugin-id="${inherited.id}"] [role="switch"]`)
    const checkSwitch = async element => {
      await element.scrollIntoViewIfNeeded()
      const root = await element.boundingBox()
      const track = await element.locator('[data-slot="switch-track"]').boundingBox()
      assert.ok(root && root.width >= 40 && root.height >= 40, `Switch target: ${JSON.stringify(root)}`)
      assert.ok(track && Math.abs(track.width - 36) < 1 && Math.abs(track.height - 20) < 1)
      assert.equal(await element.evaluate(node => getComputedStyle(node).backgroundColor), 'rgba(0, 0, 0, 0)')
    }
    await checkSwitch(toggle)
    await toggle.click()
    assert.equal(await toggle.getAttribute('aria-checked'), 'false')
    assert.equal(JSON.parse(await complete.inputValue()).plugins.find(entry => entry.id === inherited.id).enabled, false)
    await toggle.focus()
    await page.keyboard.press('Space')
    assert.equal(await toggle.getAttribute('aria-checked'), 'true')
    await checkSwitch(toggle)
    const mcp = { id: 'mcp-preview-fixture', kind: 'ternilo.mcp.stdio', enabled: false,
      config: { server_name: 'fixture', command: 'unused-mcp-fixture', args: [] } }
    const overrides = JSON.parse(await raw.inputValue())
    overrides.plugins.push(mcp)
    await raw.fill(JSON.stringify(overrides, null, 2))
    assert.deepEqual(JSON.parse(await complete.inputValue()).plugins.find(entry => entry.id === mcp.id), mcp)
    await page.screenshot({ path: path.join(artifacts, 'preset-overrides-desktop.png') })
    await editor.getByRole('button', { name: '保存预设', exact: true }).click()
    await editor.waitFor({ state: 'detached' })
    const saved = await api('/agent-presets/profile-ui-fixture')
    assert.deepEqual(saved.profile, overrides)
    assert.equal(saved.profile.plugins.length, before.profile.plugins.length + 2)
    assert.deepEqual(saved.base_profile, before.base_profile)
    for (const [locale, theme, width] of [['zh', 'light', 1440], ['en', 'dark', 1280], ['zh', 'light', 700], ['en', 'dark', 390], ['zh', 'light', 320]]) {
      await settings.getByRole('button', { name: /^(通用|General)$/ }).click()
      await selectChoice(settings.getByLabel(/^(语言|Language)$/), locale)
      await selectChoice(settings.getByLabel(/^(界面主题|Theme)$/), theme)
      await settings.getByRole('button', { name: /^(Agent 预设|Agent presets)$/ }).click()
      await page.setViewportSize({ width, height: 900 })
      await card.getByRole('button', { name: /^(编辑|Edit): Profile UI fixture$/ }).click()
      editor = page.getByRole('dialog', { name: /^(编辑|Edit) Profile UI fixture$/ })
      const switchRoot = editor.locator(`[data-plugin-id="${inherited.id}"] [role="switch"]`)
      await checkSwitch(switchRoot)
      await switchRoot.click()
      assert.equal(await switchRoot.getAttribute('aria-checked'), 'false')
      await checkSwitch(switchRoot)
      await page.screenshot({ path: path.join(artifacts, `preset-switches-${locale}-${theme}-${width}.png`) })
      await editor.locator('details > summary').click()
      assert.ok(JSON.parse(await editor.locator('#preset-effective-profile').inputValue()).plugins.length > saved.profile.plugins.length)
      await editor.locator('#preset-effective-profile').scrollIntoViewIfNeeded()
      assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true)
      assert.equal(await editor.locator('[data-preset-editor-scroll]').evaluate(element =>
        element.scrollWidth <= element.clientWidth && element.scrollLeft === 0), true)
      await page.screenshot({ path: path.join(artifacts, `preset-profile-${locale}-${theme}-${width}.png`) })
      await editor.getByRole('button', { name: /^(取消|Cancel)$/ }).click()
      await editor.waitFor({ state: 'detached' })
    }
    assert.deepEqual((await api('/agent-presets/profile-ui-fixture')).profile, saved.profile)
    assert.deepEqual(errors, [])
  } catch (error) {
    await page?.screenshot({ path: path.join(artifacts, 'preset-profile-failure.png') }).catch(() => {})
    error.message += `\n${app.diagnostics()}`
    throw error
  } finally {
    await browser?.close()
    await stopProcess(app)
    await rm(directory, { recursive: true, force: true })
  }
})
