import { selectChoice } from './browser-select-fixture.mjs'
import assert from 'node:assert/strict'
import { mkdir, mkdtemp, rm } from 'node:fs/promises'
import { createServer } from 'node:http'
import { tmpdir } from 'node:os'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import test from 'node:test'
import { chromium } from 'playwright'
import { freePort, startProcess, stopProcess, waitForHttp } from './platform-e2e-fixture.mjs'

const webRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..')
const repository = path.resolve(webRoot, '..')
const binary = process.env.TERNILO_E2E_NODE_BINARY ?? path.join(repository, 'target', 'debug', 'ternilo')

async function startTernilo(dataDirectory) {
  const origin = `http://127.0.0.1:${await freePort()}`
  const application = startProcess(binary, ['serve', '--listen', new URL(origin).host, '--data-dir', dataDirectory])
  await waitForHttp(origin, application)
  return { ...application, origin }
}

function sse(payload) {
  return `data: ${JSON.stringify(payload)}\n\n`
}

async function startModelFixture() {
  const requests = []
  const waiters = new Map()
  const server = createServer((incoming, response) => {
    if (incoming.method !== 'POST' || incoming.url !== '/v1/responses') {
      response.writeHead(404, { 'content-type': 'application/json' })
      response.end('{"error":"not found"}')
      return
    }
    const chunks = []
    incoming.on('data', chunk => chunks.push(chunk))
    incoming.on('end', () => {
      const body = JSON.parse(Buffer.concat(chunks).toString('utf8'))
      response.writeHead(200, { 'content-type': 'text/event-stream', 'cache-control': 'no-cache' })
      if (typeof body.instructions === 'string' && body.instructions.includes('You name software-agent conversations')) {
        response.end([
          sse({ type: 'response.output_text.delta', output_index: 0, content_index: 0, delta: 'Agent mode acceptance' }),
          sse({
            type: 'response.completed',
            response: { status: 'completed', output: [], usage: { input_tokens: 4, output_tokens: 3 } },
          }),
        ].join(''))
        return
      }
      const index = requests.push(body) - 1
      waiters.get(index)?.(body)
      waiters.delete(index)
      const answer = `mode-${index}-complete`
      response.end([
        sse({ type: 'response.output_text.delta', output_index: 0, content_index: 0, delta: answer }),
        sse({
          type: 'response.completed',
          response: { status: 'completed', output: [], usage: { input_tokens: 10, output_tokens: 3 } },
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
    request(index) {
      if (requests[index]) return Promise.resolve(requests[index])
      return new Promise((resolve, reject) => {
        const timeout = setTimeout(() => {
          waiters.delete(index)
          reject(new Error(`model request ${index} was not received`))
        }, 20_000)
        waiters.set(index, request => {
          clearTimeout(timeout)
          resolve(request)
        })
      })
    },
    close: () => new Promise((resolve, reject) => server.close(error => error ? reject(error) : resolve())),
  }
}

async function api(page, pathValue, { method = 'GET', body } = {}) {
  return page.evaluate(async ({ pathValue, method, body }) => {
    const token = window.__TERNILO_BOOT__?.apiToken
    const response = await fetch(`/api/v1${pathValue}`, {
      method,
      headers: {
        ...(token ? { authorization: `Bearer ${token}` } : {}),
        ...(body === undefined ? {} : { 'content-type': 'application/json' }),
      },
      body: body === undefined ? undefined : JSON.stringify(body),
    })
    if (!response.ok) throw new Error(`${method} ${pathValue}: ${response.status} ${await response.text()}`)
    return response.status === 204 ? null : response.json()
  }, { pathValue, method, body })
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
  await api(page, '/providers', { method: 'POST', body: {
    id: 'mode-fixture',
    display_name: 'Mode Fixture',
    base_url: baseUrl,
    protocol: 'openai-responses',
    api_key_ref: null,
    defaults: { context_window: 128_000, max_output_tokens: 4_096 },
    models: [{ id: 'mode-model', display_name: 'Mode Model', settings: { mode: 'inherit' } }],
    timeout_ms: 30_000,
    max_attempts: 1,
    retry_base_delay_ms: 10,
  } })
  const sessionId = await page.evaluate(() => localStorage.getItem('ternilo.current-session'))
  const selection = { provider: 'named_provider', provider_id: 'mode-fixture', model: 'mode-model' }
  await api(page, `/sessions/${encodeURIComponent(sessionId)}`, { method: 'PATCH', body: { model: selection } })
  await api(page, '/default-model', { method: 'PUT', body: selection })
  await page.reload({ waitUntil: 'domcontentloaded' })
  const picker = page.locator('[data-input-bar] [data-model-picker]')
  await picker.filter({ hasText: 'Mode Model' }).waitFor()
  assert.match(await picker.textContent(), /Mode Model/)
}

async function selectHeroPreset(page, label) {
  const trigger = page.getByRole('button', { name: /新会话 Agent：/ })
  await trigger.click()
  await page.getByRole('menuitem').filter({ has: page.getByText(label, { exact: true }) }).click()
  await page.getByRole('button', { name: `新会话 Agent：${label}` }).waitFor()
}

async function startModeSession(page, label) {
  await page.locator('[data-sidebar-new-session]').click()
  await page.getByRole('textbox', { name: '输入任务' }).waitFor()
  await selectHeroPreset(page, label)
}

async function runMode(page, model, requestIndex, task) {
  const input = page.getByRole('textbox', { name: '输入任务' })
  await input.fill(task)
  await page.getByRole('button', { name: '发送' }).click()
  const request = await model.request(requestIndex)
  await page.getByText(`mode-${requestIndex}-complete`, { exact: true }).waitFor()
  return request
}

function toolNames(request) {
  return (request.tools ?? []).map(tool => tool.name).filter(Boolean).sort()
}

test('the four shared Agent modes are bilingual and change the real model surface', { timeout: 120_000 }, async () => {
  const dataDirectory = await mkdtemp(path.join(tmpdir(), 'ternilo-agent-presets-browser-'))
  const workspace = path.join(dataDirectory, 'workspace')
  await mkdir(workspace, { recursive: true })
  const model = await startModelFixture()
  const ternilo = await startTernilo(dataDirectory)
  let browser
  try {
    const origin = await ternilo.origin
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
    const page = await browser.newPage({ locale: 'zh-CN', viewport: { width: 1440, height: 900 } })
    const pageErrors = []
    const consoleErrors = []
    const httpErrors = []
    page.on('pageerror', error => pageErrors.push(error.message))
    page.on('console', message => { if (message.type() === 'error') consoleErrors.push(message.text()) })
    page.on('response', response => {
      if (response.status() >= 400) httpErrors.push(`${response.request().method()} ${response.status()} ${response.url()}`)
    })
    await page.goto(origin, { waitUntil: 'networkidle' })
    await chooseWorkspace(page, workspace)
    await configureProvider(page, model.baseUrl)

    await page.getByRole('button', { name: '设置', exact: true }).click()
    let settings = page.getByRole('dialog', { name: '设置' })
    await settings.getByRole('button', { name: 'Agent 预设', exact: true }).click()
    for (const [name, description] of [
      ['标准模式', '完整的软件 Agent，同时提供原生工具与 Rhai Code Mode SDK。'],
      ['PTC 模式', '保留标准模式的全部能力，但只向模型呈现 Rhai Code Mode SDK。'],
      ['极简模式', '以工作区文件与 shell 为核心的精简软件 Agent。'],
      ['创意模式', '保留标准模式的全部能力，并为预设、插件和运行时实验提供指导。'],
    ]) {
      const card = settings.locator('article').filter({ hasText: name })
      await card.getByText(description, { exact: true }).waitFor()
      assert.equal(await card.getByRole('button', { name: new RegExp(`^编辑: ${name}$`) }).count(), 0)
      assert.equal(await card.getByRole('button', { name: new RegExp(`^删除: ${name}$`) }).count(), 0)
    }

    await settings.getByRole('button', { name: '复制预设: 标准模式' }).click()
    const chineseCopy = page.getByRole('dialog', { name: /复制预设/ })
    await chineseCopy.locator('#preset-copy-id').fill('localized-zh-preset')
    assert.equal(await chineseCopy.locator('#preset-copy-name').inputValue(), '标准模式 · 自定义')
    const chineseCopyWrites = Promise.all([
      page.waitForResponse(response => response.request().method() === 'POST'
        && new URL(response.url()).pathname === '/api/v1/agent-presets'),
      page.waitForResponse(response => response.request().method() === 'PUT'
        && new URL(response.url()).pathname === '/api/v1/agent-presets/localized-zh-preset'),
    ])
    await chineseCopy.getByRole('button', { name: '创建预设' }).click()
    assert.deepEqual((await chineseCopyWrites).map(response => response.status()), [201, 200])
    const chineseCustom = settings.locator('article').filter({ hasText: 'localized-zh-preset' })
    await chineseCustom.getByText('标准模式 · 自定义', { exact: true }).waitFor()
    await chineseCustom.getByText('完整的软件 Agent，同时提供原生工具与 Rhai Code Mode SDK。', { exact: true }).waitFor()

    await settings.getByRole('button', { name: '通用', exact: true }).click()
    await selectChoice(settings.getByLabel('语言'), 'en')
    settings = page.getByRole('dialog', { name: 'Settings' })
    await settings.getByRole('button', { name: 'Agent presets', exact: true }).click()
    for (const name of ['Standard Mode', 'PTC Mode', 'Minimal Mode', 'Creative Mode']) {
      await settings.getByText(name, { exact: true }).waitFor()
    }
    const durableChineseCustom = settings.locator('article').filter({ hasText: 'localized-zh-preset' })
    await durableChineseCustom.getByText('标准模式 · 自定义', { exact: true }).waitFor()
    await durableChineseCustom.getByText('完整的软件 Agent，同时提供原生工具与 Rhai Code Mode SDK。', { exact: true }).waitFor()

    await settings.getByRole('button', { name: 'Copy preset: Standard Mode' }).click()
    const englishCopy = page.getByRole('dialog', { name: /Duplicate preset/ })
    await englishCopy.locator('#preset-copy-id').fill('localized-en-preset')
    assert.equal(await englishCopy.locator('#preset-copy-name').inputValue(), 'Standard Mode · Custom')
    const englishCopyWrites = Promise.all([
      page.waitForResponse(response => response.request().method() === 'POST'
        && new URL(response.url()).pathname === '/api/v1/agent-presets'),
      page.waitForResponse(response => response.request().method() === 'PUT'
        && new URL(response.url()).pathname === '/api/v1/agent-presets/localized-en-preset'),
    ])
    await englishCopy.getByRole('button', { name: 'Create preset' }).click()
    assert.deepEqual((await englishCopyWrites).map(response => response.status()), [201, 200])
    const englishCustom = settings.locator('article').filter({ hasText: 'localized-en-preset' })
    await englishCustom.getByText('Standard Mode · Custom', { exact: true }).waitFor()
    await englishCustom.getByText('A full software Agent with both native tools and the Rhai Code Mode SDK.', { exact: true }).waitFor()

    const englishDocumentBefore = await api(page, '/agent-presets/localized-en-preset')
    const codeModeEntryBefore = englishDocumentBefore.profile.plugins.find(entry => entry.kind === 'ternilo.tools.code_mode')
      ?? englishDocumentBefore.base_profile.plugins.find(entry => entry.kind === 'ternilo.tools.code_mode')
    assert.ok(codeModeEntryBefore, 'the copied standard preset must contain the Code Mode plugin')
    await englishCustom.getByRole('button', { name: 'Edit: Standard Mode · Custom' }).click()
    const editor = page.getByRole('dialog', { name: 'Edit Standard Mode · Custom' })
    await editor.locator('[data-preset-plugin-editor]').waitFor()
    const advancedProfile = editor.locator('details')
    assert.equal(await advancedProfile.evaluate(element => element.open), false)
    await advancedProfile.getByText('Plugin Profile (advanced)', { exact: true }).waitFor()
    const codeModeCard = editor.locator(`article[data-plugin-id="${codeModeEntryBefore.id}"]`)
    await codeModeCard.getByRole('button', { name: `Expand: ${codeModeEntryBefore.id}` }).click()
    const maxSubcalls = codeModeCard.locator(`#plugin-${codeModeEntryBefore.id}-max_subcalls`)
    await maxSubcalls.fill('64')
    await codeModeCard.getByRole('button', { name: 'Apply to draft' }).click()
    const presetUpdate = page.waitForResponse(response => response.request().method() === 'PUT'
      && new URL(response.url()).pathname === '/api/v1/agent-presets/localized-en-preset')
    await editor.getByRole('button', { name: 'Save preset' }).click()
    assert.equal((await presetUpdate).status(), 200)
    await editor.waitFor({ state: 'detached' })
    const englishDocumentAfter = await api(page, '/agent-presets/localized-en-preset')
    assert.equal(
      englishDocumentAfter.profile.plugins.find(entry => entry.id === codeModeEntryBefore.id)?.config.max_subcalls,
      64,
    )
    await page.setViewportSize({ width: 390, height: 844 })
    await englishCustom.getByRole('button', { name: 'Edit: Standard Mode · Custom' }).click()
    const mobileEditor = page.getByRole('dialog', { name: 'Edit Standard Mode · Custom' })
    await mobileEditor.locator('[data-preset-plugin-editor]').waitFor()
    const mobileBox = await mobileEditor.boundingBox()
    assert.ok(mobileBox && mobileBox.x >= -1 && mobileBox.x + mobileBox.width <= 391)
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth + 1), true)
    await page.keyboard.press('Escape')
    await mobileEditor.waitFor({ state: 'detached' })
    await page.setViewportSize({ width: 1440, height: 900 })

    const localizedRoster = await api(page, '/agent-presets')
    assert.deepEqual(
      localizedRoster.presets
        .filter(preset => preset.id.startsWith('localized-'))
        .map(preset => [preset.id, preset.display_name, preset.description]),
      [
        ['localized-en-preset', 'Standard Mode · Custom', 'A full software Agent with both native tools and the Rhai Code Mode SDK.'],
        ['localized-zh-preset', '标准模式 · 自定义', '完整的软件 Agent，同时提供原生工具与 Rhai Code Mode SDK。'],
      ],
    )
    await settings.getByRole('button', { name: 'General', exact: true }).click()
    const defaultUpdate = page.waitForResponse(response => (
      response.request().method() === 'PUT'
      && new URL(response.url()).pathname === '/api/v1/agent-presets/creative/default'
    ))
    await selectChoice(settings.getByLabel('Agent preset for new sessions'), 'creative')
    assert.equal((await defaultUpdate).status(), 200)
    await selectChoice(settings.getByLabel('Language'), 'zh')
    settings = page.getByRole('dialog', { name: '设置' })
    await settings.getByRole('button', { name: 'Agent 预设', exact: true }).click()
    const durableEnglishCustom = settings.locator('article').filter({ hasText: 'localized-en-preset' })
    await durableEnglishCustom.getByText('Standard Mode · Custom', { exact: true }).waitFor()
    await durableEnglishCustom.getByText('A full software Agent with both native tools and the Rhai Code Mode SDK.', { exact: true }).waitFor()
    await page.keyboard.press('Escape')
    await settings.waitFor({ state: 'detached' })

    const createRequest = page.waitForRequest(request => (
      request.method() === 'POST'
      && request.url().endsWith('/api/v1/sessions')
    ))
    await page.locator('[data-sidebar-new-session]').click()
    assert.equal((await createRequest).postDataJSON().agent_preset, 'creative')
    await page.getByRole('button', { name: '新会话 Agent：创意模式' }).waitFor()
    await selectHeroPreset(page, 'PTC 模式')

    const ptc = await runMode(page, model, 0, '验证 PTC 模式')
    assert.deepEqual(toolNames(ptc), ['run_code'])
    assert.match(ptc.instructions, /# Code Mode \(Rhai\)/)
    assert.match(ptc.instructions, /`run_code` is the only tool callable directly/)

    await startModeSession(page, '极简模式')
    const minimal = await runMode(page, model, 1, '验证极简模式')
    const minimalTools = toolNames(minimal)
    assert.equal(minimalTools.includes('run_code'), false)
    assert.equal(minimalTools.includes('read_file'), true)
    assert.equal(minimalTools.includes('write_file'), true)
    assert.equal(minimalTools.includes('search_files'), true)
    assert.equal(minimalTools.includes('shell'), true)
    assert.doesNotMatch(minimal.instructions, /# Code Mode \(Rhai\)/)

    await startModeSession(page, '创意模式')
    const creative = await runMode(page, model, 2, '验证创意模式')
    const creativeTools = toolNames(creative)
    assert.equal(creativeTools.includes('run_code'), true)
    assert.equal(creativeTools.includes('read_file'), true)
    assert.match(creative.instructions, /# Code Mode \(Rhai\)/)
    assert.match(creative.instructions, /Creative mode is active/)

    await startModeSession(page, '标准模式')
    const standard = await runMode(page, model, 3, '验证标准模式')
    const standardTools = toolNames(standard)
    assert.equal(standardTools.includes('run_code'), true)
    assert.equal(standardTools.includes('read_file'), true)
    assert.match(standard.instructions, /# Code Mode \(Rhai\)/)
    assert.doesNotMatch(standard.instructions, /Creative mode is active/)

    assert.deepEqual(pageErrors, [])
    assert.deepEqual(httpErrors, [])
    assert.deepEqual(consoleErrors, [], httpErrors.join('\n'))
    assert.equal(ternilo.diagnostics().includes('panicked'), false, ternilo.diagnostics())
  } catch (error) {
    throw new Error(`Agent preset browser acceptance failed: ${ternilo.diagnostics()}`, { cause: error })
  } finally {
    await browser?.close()
    await stopProcess(ternilo)
    await model.close()
    await rm(dataDirectory, { recursive: true, force: true })
  }
})
