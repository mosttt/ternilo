import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { chromium } from 'playwright'
import { selectChoice } from './browser-select-fixture.mjs'
import { freePort, repository, startProcess, stopProcess, waitForHttp } from './platform-e2e-fixture.mjs'

import { nativeFixture, finalAnswer, settleNativeViewport } from './native-model-fixture.mjs'

async function api(page, endpoint, method = 'GET', body) {
  return page.evaluate(async ({ endpoint, method, body }) => {
    const token = window.__TERNILO_BOOT__?.apiToken
    const response = await fetch(`/api/v1${endpoint}`, {
      method, headers: { ...(token ? { authorization: `Bearer ${token}` } : {}), 'content-type': 'application/json' },
      body: body === undefined ? undefined : JSON.stringify(body),
    })
    if (!response.ok) throw new Error(`${endpoint}: ${response.status} ${await response.text()}`)
    return response.status === 204 ? null : response.json()
  }, { endpoint, method, body })
}

for (const protocol of ['google-gemini', 'anthropic-messages']) {
  test(`${protocol}: shared Provider editor, native discovery, thinking tool loop and mobile persistence`, { timeout: 120_000 }, async context => {
    const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-native-browser-'))
    const workspace = path.join(directory, 'workspace')
    await mkdir(workspace)
    await writeFile(path.join(workspace, 'fixture.txt'), 'Native fixture file')
    const model = await nativeFixture(protocol)
    const origin = `http://127.0.0.1:${await freePort()}`
    const app = startProcess(process.env.TERNILO_E2E_LOCAL_BINARY ?? process.env.TERNILO_E2E_NODE_BINARY ?? path.join(repository, 'target/debug/ternilo'), [
      'serve', '--listen', new URL(origin).host, '--data-dir', path.join(directory, 'data'),
    ], { XDG_STATE_HOME: path.join(directory, 'state') })
    let browser, page
    const errors = [], network = []
    try {
      await waitForHttp(origin, app)
      for (const asset of ['app.js', 'app.css']) {
        const actual = Buffer.from(await (await fetch(`${origin}/assets/${asset}`)).arrayBuffer())
        const expected = await readFile(path.join(repository, 'web/dist/assets', asset))
        assert.equal(createHash('sha256').update(actual).digest('hex'), createHash('sha256').update(expected).digest('hex'))
      }
      browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
      page = await browser.newPage({ locale: 'zh-CN', viewport: { width: 1280, height: 850 }, hasTouch: true, serviceWorkers: 'block' })
      page.on('pageerror', error => errors.push(error.message))
      page.on('console', message => { if (message.type() === 'error') errors.push(message.text()) })
      page.on('response', response => {
        const pathname = new URL(response.url()).pathname
        if (pathname.startsWith('/api/')) network.push({ method: response.request().method(), path: pathname, status: response.status() })
        if (response.status() >= 400) errors.push(`${response.status()} ${pathname}`)
      })
      await page.goto(origin)
      const record = await api(page, '/workspaces', 'POST', { path: workspace })
      const session = await api(page, '/sessions', 'POST', { workspace_id: record.workspace_id, agent_preset: 'standard' })
      const sessionId = session.identity.session_id
      await page.evaluate(id => localStorage.setItem('ternilo.current-session', id), sessionId)
      await page.reload()
      await page.getByRole('button', { name: '设置', exact: true }).click()
      const settings = page.getByRole('dialog', { name: '设置', exact: true })
      await settings.getByRole('button', { name: '模型', exact: true }).click()
      await settings.getByRole('button', { name: '添加 Provider', exact: true }).first().click()
      const editor = settings.locator('[data-provider-editor="new"]')
      await editor.getByLabel('Provider ID', { exact: true }).fill('native-test')
      await editor.getByLabel('显示名称', { exact: true }).fill('Native test')
      await selectChoice(editor.getByLabel('API 协议', { exact: true }), protocol)
      assert.equal(await editor.getByLabel('API 地址', { exact: true }).inputValue(), protocol === 'google-gemini' ? 'https://generativelanguage.googleapis.com/v1beta' : 'https://api.anthropic.com/v1')
      await editor.getByLabel('API 地址', { exact: true }).fill(model.baseUrl)
      await editor.getByLabel('API Key', { exact: true }).fill('native-fixture-key')
      await editor.getByRole('switch', { name: '启用 Provider 默认模型设置 的推理强度' }).click()
      await selectChoice(editor.locator('[id$="-provider-defaults-default-effort"]'), 'high')
      for (const width of [1280, 390, 320]) {
        await page.setViewportSize({ width, height: 850 })
        await editor.getByLabel('API 协议', { exact: true }).scrollIntoViewIfNeeded()
        const bounds = await editor.boundingBox()
        assert.ok(bounds && bounds.x >= 0 && bounds.x + bounds.width <= width + 1)
        assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true)
        if (process.env.TERNILO_E2E_ARTIFACT_DIR) {
          await mkdir(process.env.TERNILO_E2E_ARTIFACT_DIR, { recursive: true })
          await editor.getByLabel('API 协议', { exact: true }).click()
          await page.screenshot({ path: path.join(process.env.TERNILO_E2E_ARTIFACT_DIR, `${protocol}-editor-${width}.png`), fullPage: true, mask: [editor.getByLabel('API Key', { exact: true })] })
          await page.keyboard.press('Escape')
        }
      }
      await editor.getByRole('button', { name: '获取可用模型', exact: true }).click()
      await page.getByRole('dialog', { name: '选择要添加的模型' }).getByRole('button', { name: '应用所选', exact: true }).click()
      assert.equal(await editor.getByLabel('模型 ID 1', { exact: true }).inputValue(), 'native-fixture')
      const saving = page.waitForResponse(response => new URL(response.url()).pathname === '/api/v1/providers' && response.request().method() === 'POST')
      await editor.getByRole('button', { name: '添加 Provider', exact: true }).click()
      assert.equal((await saving).status(), 200)
      await editor.waitFor({ state: 'hidden' })
      assert.equal((await api(page, '/providers')).find(provider => provider.id === 'native-test').protocol, protocol)
      await settings.getByRole('button', { name: '关闭设置', exact: true }).click()
      await api(page, `/sessions/${sessionId}`, 'PATCH', { model: { provider: 'named_provider', provider_id: 'native-test', model: 'native-fixture' } })
      await page.reload()
      await page.getByRole('textbox', { name: '输入任务', exact: true }).fill('读取 fixture.txt')
      await page.getByRole('button', { name: '发送', exact: true }).click()
      await page.getByText(finalAnswer, { exact: true }).waitFor()
      assert.equal(model.requests.length, 2)
      assert.deepEqual(model.failures, [])
      assert.ok(model.requests.every(request => protocol === 'google-gemini' ? request.generationConfig.thinkingConfig.thinkingLevel === 'high' : request.output_config.effort === 'high'))
      await page.reload()
      await page.getByText(finalAnswer, { exact: true }).waitFor()
      for (const width of [1280, 390]) {
        await page.setViewportSize({ width, height: 850 })
        await settleNativeViewport(page)
        assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true)
        if (process.env.TERNILO_E2E_ARTIFACT_DIR) await page.screenshot({ path: path.join(process.env.TERNILO_E2E_ARTIFACT_DIR, `${protocol}-local-answer-${width}.png`), fullPage: true })
      }
      assert.deepEqual(errors, [])
      context.diagnostic(`${protocol}: native auth, real file tool call, opaque signatures, persistence and 1280/390/320 px passed`)
    } catch (error) {
      if (process.env.TERNILO_E2E_ARTIFACT_DIR) await page?.screenshot({ path: path.join(process.env.TERNILO_E2E_ARTIFACT_DIR, `${protocol}-local-failure.png`), fullPage: true }).catch(() => {})
      error.message += `\nFixture failures: ${JSON.stringify(model.failures)}\n${app.diagnostics()}`
      throw error
    } finally {
      if (process.env.TERNILO_E2E_ARTIFACT_DIR) {
        await mkdir(process.env.TERNILO_E2E_ARTIFACT_DIR, { recursive: true })
        await writeFile(path.join(process.env.TERNILO_E2E_ARTIFACT_DIR, `${protocol}-local-network.json`), JSON.stringify({ errors, network, requests: model.requests, discoveries: model.discoveries, signatureChecks: model.signatureChecks, failures: model.failures }, null, 2))
        await writeFile(path.join(process.env.TERNILO_E2E_ARTIFACT_DIR, `${protocol}-local-process.log`), app.diagnostics())
      }
      await browser?.close()
      await stopProcess(app)
      await model.close()
      await rm(directory, { recursive: true, force: true })
    }
  })
}
