import { selectChoice } from './browser-select-fixture.mjs'
import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { chromium } from 'playwright'
import { freePort, repository, serverRequest, startProcess, stopProcess, waitForHttp } from './platform-e2e-fixture.mjs'

test('tool call limits persist in copied presets and independent session overrides', { timeout: 120_000 }, async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-tool-limit-browser-'))
  const artifacts = process.env.TERNILO_E2E_ARTIFACT_DIR ?? directory
  await mkdir(artifacts, { recursive: true })
  const errors = [], assetHashes = {}
  let local, browser, page
  try {
    const origin = `http://127.0.0.1:${await freePort()}`
    local = startProcess(path.join(repository, 'target/debug/ternilo'), ['serve', '--listen', new URL(origin).host, '--data-dir', path.join(directory, 'data')])
    await waitForHttp(origin, local)
    for (const asset of ['app.js', 'app.css']) {
      const response = await fetch(`${origin}/assets/${asset}`)
      assert.equal(response.status, 200)
      const digest = bytes => createHash('sha256').update(bytes).digest('hex')
      assetHashes[asset] = digest(Buffer.from(await response.arrayBuffer()))
      assert.equal(assetHashes[asset], digest(await readFile(path.join(repository, 'web/dist/assets', asset))))
    }
    const html = await (await fetch(origin)).text()
    const token = JSON.parse(html.match(/window\.__TERNILO_BOOT__\s*=\s*(\{[^<]+\});/)[1]).apiToken
    const api = (resource, options = {}) => serverRequest(origin, resource, { token, ...options })
    const workspacePath = path.join(directory, 'workspace')
    await mkdir(workspacePath)
    const workspace = await api('/workspaces', { body: { path: workspacePath } })
    const session = await api('/sessions', { body: { workspace_id: workspace.workspace_id } })
    const sessionId = session.identity.session_id
    const storedSession = async () => (await api('/state')).sessions.find(value => value.identity.session_id === sessionId)
    const catalog = await api(`/catalog?session_id=${sessionId}`)
    const schema = catalog.plugins.find(plugin => plugin.kind === 'ternilo.agent.react').config_schema
    assert.equal(schema.properties.max_tool_calls.default, 512)
    assert.equal(schema.properties.max_tool_calls.minimum, 0)
    assert.equal(catalog.host_limits.max_tool_calls, 0)
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
    page = await browser.newPage({ locale: 'zh-CN', viewport: { width: 1440, height: 960 }, serviceWorkers: 'block' })
    page.on('pageerror', error => errors.push(error.message))
    page.on('console', message => { if (message.type() === 'error') errors.push(message.text()) })
    page.on('response', response => { if (response.status() >= 400) errors.push(`HTTP ${response.status()} ${new URL(response.url()).pathname}`) })
    page.on('requestfailed', request => { if (!request.failure()?.errorText.includes('ERR_ABORTED')) errors.push(request.failure()?.errorText) })
    await page.goto(origin)
    const workspaceToggle = page.locator('[data-sidebar-workspace-button]').first()
    await workspaceToggle.waitFor()
    if (await workspaceToggle.getAttribute('aria-expanded') !== 'true') await workspaceToggle.click()
    await page.locator(`[data-sidebar-session-row][data-session-id="${sessionId}"] [data-sidebar-session-button]`).click()
    async function settings(section) {
      const dialog = page.getByRole('dialog', { name: '设置', exact: true })
      if (!await dialog.isVisible()) await page.getByRole('button', { name: '设置', exact: true }).click()
      await dialog.getByRole('button', { name: section, exact: true }).click()
      return dialog
    }
    async function expandLoop(container) {
      const card = container.locator('[data-plugin-id="agent-loop"]')
      const toggle = card.locator('button[aria-expanded]')
      await toggle.waitFor()
      if (await toggle.getAttribute('aria-expanded') !== 'true') await toggle.click()
      await card.getByLabel('每轮工具调用上限', { exact: true }).waitFor()
      return card
    }
    let settingsDialog = await settings('Agent 预设')
    assert.equal(await settingsDialog.getByRole('button', { name: '编辑: 标准模式', exact: true }).count(), 0)
    await settingsDialog.getByRole('button', { name: '复制预设: 标准模式', exact: true }).click()
    const copying = page.getByRole('dialog', { name: /复制预设/ })
    await copying.locator('#preset-copy-id').fill('tool-limit-browser')
    await copying.locator('#preset-copy-name').fill('Tool Limit Browser')
    await copying.getByRole('button', { name: '创建预设', exact: true }).click()
    const presetCard = () => settingsDialog.locator('article').filter({ hasText: 'tool-limit-browser' })
    await presetCard().getByRole('button', { name: '编辑: Tool Limit Browser', exact: true }).click()
    let editor = page.getByRole('dialog', { name: '编辑 Tool Limit Browser', exact: true })
    let loop = await expandLoop(editor)
    let input = loop.getByLabel('每轮工具调用上限', { exact: true })
    assert.equal(await input.inputValue(), '512')
    await input.fill('0')
    await loop.locator('[data-tool-call-limit]').getByText('实际生效：不限制工具调用次数。', { exact: true }).waitFor()
    await loop.locator('[data-tool-call-limit]').scrollIntoViewIfNeeded()
    await page.screenshot({ path: path.join(artifacts, 'tool-limit-preset-desktop.png'), animations: 'disabled' })
    await loop.getByRole('button', { name: '应用到草稿', exact: true }).click()
    await editor.getByRole('button', { name: '保存预设', exact: true }).click()
    await editor.waitFor({ state: 'hidden' })
    const savedPreset = () => api(`/agent-presets/tool-limit-browser?session_id=${sessionId}`)
    assert.equal((await savedPreset()).profile.plugins.find(plugin => plugin.id === 'agent-loop').config.max_tool_calls, 0)

    await page.reload()
    settingsDialog = await settings('Agent 预设')
    await presetCard().getByRole('button', { name: '编辑: Tool Limit Browser', exact: true }).click()
    editor = page.getByRole('dialog', { name: '编辑 Tool Limit Browser', exact: true })
    loop = await expandLoop(editor)
    assert.equal(await loop.getByLabel('每轮工具调用上限', { exact: true }).inputValue(), '0')
    await editor.getByRole('button', { name: '取消', exact: true }).click()
    await presetCard().getByRole('button', { name: '使用: Tool Limit Browser', exact: true }).click()
    await presetCard().getByText('当前使用', { exact: true }).waitFor()
    settingsDialog = await settings('插件')
    loop = await expandLoop(settingsDialog)
    input = loop.getByLabel('每轮工具调用上限', { exact: true })
    assert.equal(await input.inputValue(), '0')
    await input.fill('-1')
    assert.equal(await loop.getByRole('button', { name: '保存', exact: true }).isDisabled(), true)
    await input.fill('2')
    await loop.locator('[data-tool-call-limit]').getByText('实际生效：每轮最多 2 次。', { exact: true }).waitFor()
    await loop.getByRole('button', { name: '保存', exact: true }).click()
    await loop.getByText('会话覆盖', { exact: true }).waitFor()
    assert.equal((await storedSession()).profile_plugins.find(plugin => plugin.id === 'agent-loop').config.max_tool_calls, 2)
    assert.equal((await savedPreset()).profile.plugins.find(plugin => plugin.id === 'agent-loop').config.max_tool_calls, 0)

    await page.reload()
    settingsDialog = await settings('插件')
    loop = await expandLoop(settingsDialog)
    input = loop.getByLabel('每轮工具调用上限', { exact: true })
    assert.equal(await input.inputValue(), '2')
    await page.setViewportSize({ width: 390, height: 844 })
    await input.scrollIntoViewIfNeeded()
    const bounds = await input.boundingBox()
    assert.ok(bounds.x >= 0 && bounds.x + bounds.width <= 390 && bounds.height >= 40)
    assert.equal(await settingsDialog.evaluate(element => element.scrollWidth <= element.clientWidth), true)
    await page.screenshot({ path: path.join(artifacts, 'tool-limit-session-mobile.png'), animations: 'disabled' })
    const resetResponse = page.waitForResponse(response => response.request().method() === 'PATCH'
      && new URL(response.url()).pathname === `/api/v1/sessions/${sessionId}`)
    await loop.getByRole('button', { name: '移除会话覆盖', exact: true }).click()
    assert.equal((await resetResponse).ok(), true)
    await loop.getByRole('button', { name: '展开: agent-loop', exact: true }).waitFor()
    assert.equal((await storedSession()).profile_plugins.some(plugin => plugin.id === 'agent-loop'), false)
    const effectiveProfile = await api(`/sessions/${sessionId}/plugins`)
    assert.equal(effectiveProfile.plugins.find(plugin => plugin.id === 'agent-loop').config.max_tool_calls, 0)
    loop = await expandLoop(settingsDialog)
    assert.equal(await loop.getByLabel('每轮工具调用上限', { exact: true }).inputValue(), '0')

    await page.setViewportSize({ width: 1440, height: 960 })
    settingsDialog = await settings('通用')
    await selectChoice(settingsDialog.getByLabel('语言', { exact: true }), 'en')
    const english = page.getByRole('dialog', { name: 'Settings', exact: true })
    await english.getByRole('button', { name: 'Plugins', exact: true }).click()
    const englishLoop = english.locator('[data-plugin-id="agent-loop"]')
    await englishLoop.getByRole('button', { name: 'Expand: agent-loop', exact: true }).click()
    assert.equal(await englishLoop.getByLabel('Tool calls per turn', { exact: true }).inputValue(), '0')
    await englishLoop.locator('[data-tool-call-limit]').getByText('Effective limit: no tool call limit.', { exact: true }).waitFor()
    await englishLoop.locator('[data-tool-call-limit]').scrollIntoViewIfNeeded()
    await page.screenshot({ path: path.join(artifacts, 'tool-limit-session-english.png'), animations: 'disabled' })
    assert.deepEqual(errors, [])
    await writeFile(path.join(artifacts, 'tool-limit-observations.json'), JSON.stringify({ assetHashes, errors,
      hostToolCallLimit: catalog.host_limits.max_tool_calls, preset: 0, sessionOverride: 2, resetInherited: 0,
    }, null, 2))
  } catch (error) {
    await page?.screenshot({ path: path.join(artifacts, 'tool-limit-failure.png'), animations: 'disabled' }).catch(() => {})
    process.stderr.write(local?.diagnostics() ?? '')
    throw error
  } finally {
    await browser?.close()
    await stopProcess(local)
    await rm(directory, { recursive: true, force: true })
  }
})
