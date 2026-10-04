import { selectChoice } from './browser-select-fixture.mjs'
import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { mkdir, mkdtemp, readFile, rm } from 'node:fs/promises'
import { createServer } from 'node:http'
import { tmpdir } from 'node:os'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import { spawn } from 'node:child_process'
import test from 'node:test'
import { chromium } from 'playwright'

const webRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..')
const repository = path.resolve(webRoot, '..')
const binary = process.env.TERNILO_E2E_LOCAL_BINARY ?? path.join(repository, 'target', 'debug', 'ternilo')

function startTernilo(dataDirectory) {
  const child = spawn(binary, ['serve', '--listen', '127.0.0.1:0', '--data-dir', dataDirectory], {
    cwd: repository, stdio: ['ignore', 'pipe', 'pipe'],
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
    child.once('exit', code => reject(new Error(`Ternilo exited with ${code}: ${diagnostics}`)))
    child.once('error', reject)
  })
  return { child, origin, diagnostics: () => diagnostics }
}

async function stop(child) {
  if (child.exitCode !== null) return
  child.kill('SIGINT')
  await Promise.race([
    new Promise(resolve => child.once('exit', resolve)),
    new Promise(resolve => setTimeout(resolve, 5_000)).then(() => child.kill('SIGKILL')),
  ])
}

async function modelCatalog() {
  const authorizationHeaders = []
  const server = createServer((request, response) => {
    if (request.method === 'GET' && request.url === '/v1/models') {
      authorizationHeaders.push(request.headers.authorization ?? null)
      response.writeHead(200, { 'content-type': 'application/json' })
      response.end(JSON.stringify({ data: [
        { id: 'settings-model', name: 'Settings Model', context_window: 128_000, max_output_tokens: 8_000 },
        { id: 'settings-discovered', name: 'Discovered Model', context_window: 64_000, max_output_tokens: 4_000 },
      ] }))
      return
    }
    response.writeHead(404)
    response.end()
  })
  await new Promise((resolve, reject) => {
    server.once('error', reject)
    server.listen(0, '127.0.0.1', resolve)
  })
  return {
    baseUrl: `http://127.0.0.1:${server.address().port}/v1`,
    authorizationHeaders,
    close: () => new Promise((resolve, reject) => server.close(error => error ? reject(error) : resolve())),
  }
}

async function chooseWorkspace(page, workspace) {
  await page.getByRole('button', { name: '选择工作文件夹' }).first().click()
  const dialog = page.getByRole('dialog', { name: '选择工作文件夹' })
  await dialog.getByRole('button', { name: '编辑文件夹路径' }).click()
  const pathEditor = dialog.getByRole('textbox', { name: '编辑文件夹路径' })
  await pathEditor.fill(workspace)
  await pathEditor.press('Enter')
  await dialog.getByRole('button', { name: '打开所选文件夹' }).click()
  await dialog.waitFor({ state: 'detached' })
  await page.locator('[data-sidebar-new-session]').click()
  await page.getByRole('textbox', { name: '输入任务' }).waitFor()
}

test('Settings actions are real, bilingual, and mobile-safe', async context => {
  const dataDirectory = await mkdtemp(path.join(tmpdir(), 'ternilo-settings-browser-'))
  const workspace = path.join(dataDirectory, 'workspace')
  await mkdir(workspace)
  const catalog = await modelCatalog()
  const app = startTernilo(dataDirectory)
  let browser, page
  try {
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
    page = await browser.newPage({ locale: 'zh-CN', viewport: { width: 1440, height: 900 } })
    const pageErrors = []
    const consoleErrors = []
    const httpErrors = []
    page.on('pageerror', error => pageErrors.push(error.message))
    page.on('console', message => { if (message.type() === 'error') consoleErrors.push(message.text()) })
    page.on('response', response => {
      if (response.status() >= 400) httpErrors.push(`${response.status()} ${response.request().method()} ${response.url()}`)
    })
    const origin = await app.origin
    const hashes = {}
    for (const asset of ['app.js', 'app.css']) {
      const response = await fetch(`${origin}/assets/${asset}`)
      assert.equal(response.status, 200)
      const actual = createHash('sha256').update(Buffer.from(await response.arrayBuffer())).digest('hex')
      assert.equal(actual, createHash('sha256').update(await readFile(path.join(webRoot, 'dist/assets', asset))).digest('hex'), `Local must embed the current ${asset}`)
      hashes[asset] = actual
    }
    context.diagnostic(`Current Local assets: ${JSON.stringify(hashes)}`)
    if (process.env.TERNILO_E2E_ARTIFACT_DIR) await mkdir(process.env.TERNILO_E2E_ARTIFACT_DIR, { recursive: true })
    await page.goto(origin, { waitUntil: 'domcontentloaded' })
    await chooseWorkspace(page, workspace)
    await page.getByRole('button', { name: '设置' }).click()
    let settings = page.getByRole('dialog', { name: '设置' })
    assert.equal(await settings.getByRole('button', { name: '关闭设置' }).evaluate(element => element === document.activeElement), true)
    await settings.getByRole('button', { name: '打开配置目录' }).waitFor()

    await selectChoice(settings.getByLabel('对话正文字号'), '18')
    assert.equal(await page.evaluate(() => getComputedStyle(document.documentElement).getPropertyValue('--ternilo-content-font-size').trim()), '18px')
    assert.equal(await page.evaluate(() => localStorage.getItem('ternilo.content-font-size')), '18')
    await selectChoice(settings.getByLabel('繁忙时 Enter 键行为'), 'steer')
    assert.equal(await page.evaluate(() => localStorage.getItem('ternilo.composer.busy-enter')), 'steer')
    await selectChoice(settings.getByLabel('新会话默认权限'), 'full_access')
    const accessDialog = page.getByRole('dialog', { name: '允许完整访问？' })
    await accessDialog.getByRole('button', { name: '允许完整访问' }).click()
    await accessDialog.waitFor({ state: 'detached' })
    await selectChoice(settings.getByLabel('新会话默认权限'), 'workspace_write')
    await settings.getByRole('button', { name: '通用', exact: true }).click()
    await selectChoice(settings.getByLabel('语言'), 'en')
    settings = page.getByRole('dialog', { name: 'Settings' })
    await settings.getByRole('button', { name: 'Open configuration folder' }).waitFor()
    await settings.getByRole('button', { name: 'Models', exact: true }).waitFor()
    await selectChoice(settings.getByLabel('Language'), 'zh')
    settings = page.getByRole('dialog', { name: '设置' })

    await settings.getByRole('button', { name: '模型', exact: true }).click()
    const configurationButton = settings.getByRole('button', { name: '打开配置目录' })
    const addProviderButton = settings.getByRole('button', { name: '添加 Provider' }).first()
    const [configurationBox, addProviderBox] = await Promise.all([
      configurationButton.boundingBox(),
      addProviderButton.boundingBox(),
    ])
    assert.ok(configurationBox && addProviderBox)
    assert.ok(configurationBox.y + configurationBox.height <= addProviderBox.y, 'settings toolbar must not overlap section actions')
    await settings.getByRole('button', { name: '添加 Provider' }).first().click()
    const editor = settings.locator('[data-provider-editor="new"]')
    await editor.getByLabel('Provider ID').fill('settings-browser')
    await editor.getByLabel('显示名称', { exact: true }).fill('Settings Browser')
    await editor.getByLabel('API Key').fill('settings-browser-secret')
    await editor.getByLabel('API 地址').fill(catalog.baseUrl)
    await editor.locator('[id$="-provider-defaults-context"]').fill('128K')
    await editor.locator('[id$="-provider-defaults-output"]').fill('8K')
    await editor.getByLabel('启用 Provider 默认模型设置 的推理强度').click()
    await editor.getByLabel('high实际推理值', { exact: true }).fill('ultra')
    await editor.getByLabel('模型 ID 1').fill('settings-model')
    await editor.getByLabel('显示名称（可选） 1').fill('Settings Model')
    await editor.getByRole('button', { name: '模型详细设置 1' }).click()
    assert.equal(await editor.getByLabel('上下文窗口来源').getAttribute('data-choice-value'), 'automatic')
    await editor.getByRole('button', { name: '获取可用模型' }).click()
    const draftDiscovery = page.getByRole('dialog', { name: '选择要添加的模型' })
    await draftDiscovery.waitFor()
    await draftDiscovery.getByText('Discovered Model', { exact: true }).waitFor()
    await draftDiscovery.getByRole('button', { name: '应用所选' }).click()
    assert.equal(await editor.getByLabel('模型 ID 2').inputValue(), 'settings-discovered')
    await editor.getByRole('button', { name: '模型详细设置 2' }).click()
    assert.equal(await editor.getByLabel('上下文窗口来源').nth(1).getAttribute('data-choice-value'), 'automatic')
    assert.equal(await editor.locator('[id$="-model-1-context"]').inputValue(), '64K')
    assert.equal(await editor.locator('[id$="-model-1-output"]').inputValue(), '4K')
    await editor.locator('[data-model-settings="settings-discovered"] [data-effective-reasoning]').filter({ hasText: '默认 medium' }).waitFor()
    await selectChoice(editor.getByLabel('上下文窗口来源').nth(1), 'custom')
    await selectChoice(editor.getByLabel('最大输出 token来源').nth(1), 'custom')
    await editor.locator('[id$="-model-1-context"]').fill('80K')
    await editor.locator('[id$="-model-1-output"]').fill('6K')
    await selectChoice(editor.locator('[id$="-model-1-reasoning-source"]'), 'custom')
    await selectChoice(editor.locator('[id$="-model-1-default-effort"]'), 'high')
    await editor.getByLabel('high实际推理值', { exact: true }).nth(1).fill('model-ultra')
    assert.equal(catalog.authorizationHeaders.at(-1), 'Bearer settings-browser-secret')
    await editor.getByRole('button', { name: '添加 Provider', exact: true }).click()
    await settings.getByText('Settings Browser', { exact: true }).waitFor()
    const savedProvider = await page.evaluate(async () => {
      const token = window.__TERNILO_BOOT__?.apiToken
      const response = await fetch('/api/v1/providers', {
        headers: token ? { authorization: `Bearer ${token}` } : {},
      })
      const providers = await response.json()
      return providers.find(provider => provider.id === 'settings-browser')
    })
    assert.deepEqual(savedProvider.defaults, {
      context_window: 128000,
      max_output_tokens: 8000,
      reasoning: {
        default_effort: 'medium',
        efforts: { high: 'ultra', low: 'low', medium: 'medium' },
      },
    })
    assert.deepEqual(savedProvider.models[0].settings, { mode: 'automatic', upstream: { context_window: 128000, max_output_tokens: 8000 }, overrides: {} })
    assert.deepEqual(savedProvider.models[1].settings, {
      mode: 'automatic', upstream: { context_window: 64000, max_output_tokens: 4000 },
      overrides: { context_window: 80000, max_output_tokens: 6000,
        reasoning: { mode: 'enabled', configuration: {
          default_effort: 'high', efforts: { high: 'model-ultra', low: 'low', medium: 'medium' },
        } },
      },
    })
    assert.equal((await settings.textContent()).includes('settings-browser-secret'), false)
    await settings.getByRole('button', { name: '编辑', exact: true }).click()
    const savedEditor = settings.locator('[data-provider-editor="settings-browser"]')
    await savedEditor.locator('summary').filter({ hasText: '自定义设置' }).click()
    assert.equal(await savedEditor.locator('[id$="-provider-defaults-context"]').inputValue(), '128K')
    assert.equal(await savedEditor.locator('[id$="-provider-defaults-output"]').inputValue(), '8K')
    assert.equal(await savedEditor.locator('[id$="-provider-defaults-default-effort"]').getAttribute('data-choice-value'), 'medium')
    await savedEditor.getByRole('button', { name: '模型详细设置 1' }).click()
    assert.equal(await savedEditor.getByLabel('上下文窗口来源').first().getAttribute('data-choice-value'), 'automatic')
    await savedEditor.getByRole('button', { name: '模型详细设置 2' }).click()
    assert.equal(await savedEditor.getByLabel('上下文窗口来源').nth(1).getAttribute('data-choice-value'), 'custom')
    assert.equal(await savedEditor.locator('[id$="-model-1-context"]').inputValue(), '80K')
    assert.equal(await savedEditor.locator('[id$="-model-1-output"]').inputValue(), '6K')
    assert.equal(await savedEditor.locator('[id$="-model-1-default-effort"]').getAttribute('data-choice-value'), 'high')
    assert.equal(await savedEditor.getByLabel('high实际推理值', { exact: true }).nth(1).inputValue(), 'model-ultra')
    const descriptionFontSize = await savedEditor.getByText('各模型未手动指定、且上游未明确提供的字段，继承这里的上下文窗口、最大输出 token 和推理强度。').evaluate(element => parseFloat(getComputedStyle(element).fontSize))
    assert.ok(descriptionFontSize >= 13)
    await savedEditor.getByRole('button', { name: '获取可用模型' }).click()
    const discovery = page.getByRole('dialog', { name: '选择要添加的模型' })
    await discovery.waitFor()
    await discovery.getByText('Discovered Model', { exact: true }).waitFor()
    await discovery.getByRole('button', { name: '应用所选' }).click()
    assert.equal(await savedEditor.getByLabel('模型 ID 2').inputValue(), 'settings-discovered')
    assert.equal(catalog.authorizationHeaders.at(-1), 'Bearer settings-browser-secret')
    await savedEditor.locator('[id$="-provider-name-settings-browser"]').fill('Settings Browser Edited')
    await savedEditor.getByRole('button', { name: '保存', exact: true }).click()
    await settings.getByText('Settings Browser Edited', { exact: true }).first().waitFor()

    await settings.getByRole('button', { name: '添加 Provider' }).first().click()
    const disposableEditor = settings.locator('[data-provider-editor="new"]')
    await disposableEditor.getByLabel('Provider ID').fill('settings-browser-delete')
    await disposableEditor.getByLabel('显示名称', { exact: true }).fill('Disposable Provider')
    await disposableEditor.getByLabel('API 地址').fill(catalog.baseUrl)
    await disposableEditor.getByLabel('模型 ID 1').fill('settings-model')
    await disposableEditor.getByRole('button', { name: '添加 Provider', exact: true }).click()
    await settings.getByRole('button', { name: '删除 Disposable Provider' }).click()
    const deleteProvider = page.getByRole('dialog', { name: '删除这个 Provider？' })
    await deleteProvider.getByRole('button', { name: '删除', exact: true }).click()
    await settings.getByText('Disposable Provider', { exact: true }).waitFor({ state: 'detached' })

    await settings.getByRole('button', { name: '插件', exact: true }).click()
    const configurationTab = settings.getByRole('tab', { name: '插件配置' })
    await configurationTab.focus()
    await configurationTab.press('ArrowRight')
    assert.equal(await settings.getByRole('tab', { name: '插件列表' }).getAttribute('aria-selected'), 'true')
    await configurationTab.click()
    const webFetchPlugin = settings.locator('[data-plugin-id="web-fetch"]')
    await webFetchPlugin.getByRole('switch', { name: '禁用 web-fetch' }).click()
    await webFetchPlugin.getByRole('switch', { name: '启用 web-fetch' }).waitFor()
    await webFetchPlugin.getByRole('switch', { name: '启用 web-fetch' }).click()
    await webFetchPlugin.getByRole('switch', { name: '禁用 web-fetch' }).waitFor()
    const filesPlugin = settings.locator('[data-plugin-id="local-files"]')
    await filesPlugin.getByRole('button', { name: '展开: local-files' }).click()
    const maxRead = filesPlugin.getByLabel(/Max read bytes/i)
    const inherited = await maxRead.inputValue()
    await maxRead.fill('3145728')
    await filesPlugin.getByRole('button', { name: '放弃修改' }).click()
    assert.equal(await maxRead.inputValue(), inherited)
    await maxRead.fill('3145728')
    await filesPlugin.getByRole('button', { name: '保存', exact: true }).click()
    await filesPlugin.getByText('会话覆盖', { exact: true }).waitFor()
    await filesPlugin.getByRole('button', { name: '展开: local-files' }).click()
    await filesPlugin.getByRole('button', { name: '移除会话覆盖' }).click()
    await filesPlugin.getByText('会话覆盖', { exact: true }).waitFor({ state: 'detached' })

    const agentPlugin = settings.locator('[data-plugin-id="agent-loop"]')
    await agentPlugin.getByRole('button', { name: '展开: agent-loop' }).click()
    const maxSteps = agentPlugin.getByLabel('每轮执行步数上限', { exact: true })
    assert.equal(await maxSteps.inputValue(), '0')
    await maxSteps.fill('12')
    await agentPlugin.getByRole('button', { name: '保存', exact: true }).click()
    await agentPlugin.getByText('会话覆盖', { exact: true }).waitFor()
    await agentPlugin.getByRole('button', { name: '展开: agent-loop' }).click()
    assert.equal(await agentPlugin.getByLabel('每轮执行步数上限', { exact: true }).inputValue(), '12')
    await agentPlugin.getByLabel('每轮执行步数上限', { exact: true }).fill('0')
    await agentPlugin.getByRole('button', { name: '保存', exact: true }).click()
    await agentPlugin.getByRole('button', { name: '展开: agent-loop' }).click()
    assert.equal(await agentPlugin.getByLabel('每轮执行步数上限', { exact: true }).inputValue(), '0')
    await agentPlugin.getByRole('button', { name: '移除会话覆盖' }).click()
    await agentPlugin.getByText('会话覆盖', { exact: true }).waitFor({ state: 'detached' })

    await settings.getByRole('tab', { name: '插件列表' }).click()
    await settings.getByRole('searchbox', { name: '搜索插件' }).fill('ternilo.files.local')
    assert.equal(Number(await settings.locator('[data-plugin-count]').textContent()), 1)

    await settings.getByRole('tab', { name: '扩展包' }).click()
    let extensionInstallRequest
    await page.route('**/api/v1/extensions*', async route => {
      if (route.request().method() !== 'POST') return route.continue()
      extensionInstallRequest = route.request().postDataJSON()
      await route.fulfill({ status: 200, contentType: 'application/json', body: '{}' })
    })
    await settings.locator('#plugin-bundle-file').setInputFiles({
      name: 'review.bundle.json',
      mimeType: 'application/json',
      buffer: Buffer.from(JSON.stringify({
        manifest: {
          schema_version: 1,
          package_id: 'dev.ternilo.review',
          version: '1.0.0',
          source: 'https://plugins.example.test/review',
          publisher_key_id: 'review-publisher',
          payload_sha256: '0'.repeat(64),
          runtime: {
            kind: 'wasm-component',
            world: 'ternilo:extension/runtime@1.0.0',
            limits: { fuel: 1, max_memory_bytes: 65536, max_input_bytes: 1, max_output_bytes: 1, max_workspace_read_bytes: 1 },
          },
          config_schema: { type: 'object', additionalProperties: false },
          contributions: { tools: [{
            handler: 'review',
            spec: { name: 'review_tool', description: 'Review capability grants', input_schema: { type: 'object' } },
            output_schema: { type: 'object' },
            effect: 'read_only',
          }], prompt_sections: [], skills: [], hooks: [], commands: [], providers: [] },
          requested_capabilities: ['log', 'workspace_read'],
        },
        payload: { kind: 'base64', content: 'fixture' },
        signature_base64: 'fixture',
      })),
    })
    const extensionReview = page.getByRole('dialog', { name: '审阅并安装扩展包' })
    await extensionReview.waitFor()
    assert.match(await extensionReview.textContent(), /dev\.ternilo\.review/)
    assert.equal(await extensionReview.getByRole('checkbox', { name: /log/ }).isChecked(), true)
    assert.equal(await extensionReview.getByRole('checkbox', { name: /workspace_read/ }).isChecked(), true)
    await extensionReview.getByRole('checkbox', { name: /log/ }).uncheck()
    await extensionReview.getByRole('button', { name: '确认安装' }).click()
    await extensionReview.waitFor({ state: 'detached' })
    assert.equal(extensionInstallRequest.bundle.manifest.package_id, 'dev.ternilo.review')
    assert.deepEqual(extensionInstallRequest.granted_capabilities, ['workspace_read'])
    await page.unroute('**/api/v1/extensions*')

    await settings.getByRole('button', { name: 'Agent 预设', exact: true }).click()
    for (const name of ['标准模式', 'PTC 模式', '极简模式', '创意模式']) {
      await settings.getByText(name, { exact: true }).waitFor()
    }
    const systemPresets = await page.evaluate(async () => {
      const token = window.__TERNILO_BOOT__?.apiToken
      const headers = token ? { authorization: `Bearer ${token}` } : {}
      const [rosterResponse, ptcResponse, creativeResponse] = await Promise.all([
        fetch('/api/v1/agent-presets', { headers }),
        fetch('/api/v1/agent-presets/ptc', { headers }),
        fetch('/api/v1/agent-presets/creative', { headers }),
      ])
      const [roster, ptc, creative] = await Promise.all([
        rosterResponse.json(), ptcResponse.json(), creativeResponse.json(),
      ])
      if (!rosterResponse.ok || !ptcResponse.ok || !creativeResponse.ok) {
        throw new Error(JSON.stringify({ roster, ptc, creative }))
      }
      return { roster, ptc, creative }
    })
    assert.deepEqual(systemPresets.roster.presets.slice(0, 4).map(preset => preset.id), [
      'standard', 'ptc', 'minimal', 'creative',
    ])
    assert.equal(systemPresets.ptc.profile.plugins.find(plugin => plugin.id === 'code-mode')?.config?.mode, 'code')
    assert.equal(systemPresets.creative.profile.plugins.find(plugin => plugin.id === 'creative-guidance')?.kind, 'ternilo.prompt.section')

    await settings.getByRole('button', { name: /^复制预设: 标准模式$/ }).click()
    const copyDialog = page.getByRole('dialog', { name: /复制预设/ })
    await copyDialog.locator('#preset-copy-id').fill('settings-browser-preset')
    await copyDialog.locator('#preset-copy-name').fill('Settings Browser Preset')
    await copyDialog.getByRole('button', { name: '创建预设' }).click()
    const presetCard = settings.locator('article').filter({ hasText: 'settings-browser-preset' })
    await presetCard.waitFor()
    await presetCard.getByRole('button', { name: '设为默认: Settings Browser Preset' }).click()
    await presetCard.getByRole('button', { name: '默认: Settings Browser Preset' }).waitFor()
    await presetCard.getByRole('button', { name: '使用: Settings Browser Preset' }).click()
    await presetCard.getByText('当前使用', { exact: true }).waitFor()
    await presetCard.getByRole('button', { name: '查看: Settings Browser Preset' }).click()
    await page.getByRole('dialog', { name: '查看 Settings Browser Preset' }).getByRole('button', { name: '关闭', exact: true }).first().click()
    await presetCard.getByRole('button', { name: '编辑: Settings Browser Preset' }).click()
    const editPreset = page.getByRole('dialog', { name: '编辑 Settings Browser Preset' })
    await editPreset.locator('#preset-edit-description').fill('Browser-tested preset')
    await editPreset.getByRole('button', { name: '保存预设' }).click()
    await presetCard.getByText('Browser-tested preset', { exact: true }).waitFor()
    await settings.getByRole('button', { name: '凭据与登录', exact: true }).click()
    await settings.getByPlaceholder('MY_SERVICE_TOKEN').fill('SETTINGS_BROWSER_TOKEN')
    await settings.locator('#credential-value').fill('credential-secret')
    const credentialSave = page.waitForResponse(response => (
      response.request().method() === 'POST'
      && new URL(response.url()).pathname === '/api/v1/credentials'
    ))
    await settings.getByRole('button', { name: '保存', exact: true }).click()
    assert.equal((await credentialSave).status(), 204)
    await settings.getByText('SETTINGS_BROWSER_TOKEN', { exact: true }).waitFor()
    await settings.getByRole('button', { name: '删除凭据: SETTINGS_BROWSER_TOKEN' }).click()
    const deleteCredential = page.getByRole('dialog', { name: '删除此凭据？' })
    await deleteCredential.getByRole('button', { name: '删除', exact: true }).click()
    await settings.getByText('SETTINGS_BROWSER_TOKEN', { exact: true }).waitFor({ state: 'detached' })

    await settings.getByRole('button', { name: '通用', exact: true }).click()
    await selectChoice(settings.getByLabel('语言'), 'en')
    settings = page.getByRole('dialog', { name: 'Settings' })
    for (const section of ['General', 'Models', 'Plugins', 'Agent presets', 'Credentials & sign-in', 'About & diagnostics']) {
      await settings.getByRole('button', { name: section, exact: true }).click()
      await settings.getByRole('heading', { name: section, exact: true }).waitFor()
    }
    await settings.getByRole('button', { name: 'Agent presets', exact: true }).click()
    for (const name of ['Standard Mode', 'PTC Mode', 'Minimal Mode', 'Creative Mode']) {
      await settings.getByText(name, { exact: true }).waitFor()
    }

    for (const viewport of [
      { width: 390, height: 844 },
      { width: 390, height: 430 },
      { width: 844, height: 390 },
    ]) {
      await page.setViewportSize(viewport)
      const box = await settings.boundingBox()
      assert.ok(box && box.x >= 5 && box.x + box.width <= viewport.width - 5)
      assert.ok(
        box && box.y >= 5 && box.y + box.height <= viewport.height - 5,
        `settings dialog leaves viewport at ${JSON.stringify({ viewport, box })}`,
      )
      assert.equal(await settings.evaluate(element => element.scrollWidth <= element.clientWidth), true)
      await settings.getByRole('button', { name: 'Models', exact: true }).click()
      const content = settings.locator('[data-settings-content]')
      assert.equal(await content.evaluate(element => element.scrollWidth <= element.clientWidth), true)
      const [toolbarBox, modelsHeadingBox] = await Promise.all([
        settings.locator('[data-settings-toolbar]').boundingBox(),
        settings.getByRole('heading', { name: 'Models', exact: true }).boundingBox(),
      ])
      assert.ok(toolbarBox && modelsHeadingBox)
      assert.ok(toolbarBox.y + toolbarBox.height <= modelsHeadingBox.y, `settings toolbar overlaps Models at ${JSON.stringify(viewport)}`)
      const smallTargets = await settings.locator('button:visible').evaluateAll((elements) => elements
        .map(element => ({ label: element.getAttribute('aria-label') || element.textContent?.trim() || '', box: element.getBoundingClientRect() }))
        .filter(({ box }) => box.width < 39 || box.height < 39)
        .map(({ label, box }) => ({ label, width: box.width, height: box.height })))
      assert.deepEqual(smallTargets, [])
      if (process.env.TERNILO_E2E_ARTIFACT_DIR) await page.screenshot({ path: path.join(process.env.TERNILO_E2E_ARTIFACT_DIR, `local-settings-${viewport.width}x${viewport.height}.png`) })
    }
    await page.keyboard.press('Escape')
    await settings.waitFor({ state: 'detached' })

    const heroPreset = page.getByRole('button', { name: 'New session Agent: Settings Browser Preset' })
    await heroPreset.waitFor()
    await heroPreset.click()
    const standardPatch = page.waitForResponse(response => (
      response.request().method() === 'PATCH'
      && /\/api\/v1\/sessions\/[^/]+$/.test(new URL(response.url()).pathname)
      && response.request().postDataJSON()?.agent_preset === 'standard'
    ))
    await page.getByRole('menuitem').filter({ has: page.getByText('Standard Mode', { exact: true }) }).click()
    assert.equal((await standardPatch).status(), 200)
    await page.getByRole('button', { name: 'New session Agent: Standard Mode' }).waitFor()

    await page.setViewportSize({ width: 390, height: 844 })
    const mobilePreset = page.getByRole('button', { name: 'New session Agent: Standard Mode' })
    await mobilePreset.click()
    const customPatch = page.waitForResponse(response => (
      response.request().method() === 'PATCH'
      && /\/api\/v1\/sessions\/[^/]+$/.test(new URL(response.url()).pathname)
      && response.request().postDataJSON()?.agent_preset === 'settings-browser-preset'
    ))
    const customCommandCatalog = page.waitForResponse(response => (
      response.request().method() === 'GET'
      && /\/api\/v1\/sessions\/[^/]+\/commands$/.test(new URL(response.url()).pathname)
    ))
    await page.getByRole('menuitem').filter({ hasText: /^Settings Browser Preset/ }).click()
    assert.equal((await customPatch).status(), 200)
    const customCommandsResponse = await customCommandCatalog
    assert.equal(customCommandsResponse.status(), 200)
    assert.ok((await customCommandsResponse.json()).commands.some(command => command.name === 'goal'))
    await page.getByRole('button', { name: 'New session Agent: Settings Browser Preset' }).waitFor()

    const input = page.getByRole('textbox', { name: 'Enter task' })
    await input.fill('/go')
    await page.getByRole('option').filter({ hasText: '/goal' }).waitFor()
    await input.fill('/goal verify preset switching')
    const goalQueue = page.waitForResponse(response => (
      response.request().method() === 'POST'
      && /\/api\/v1\/sessions\/[^/]+\/queue$/.test(new URL(response.url()).pathname)
    ))
    await page.getByRole('button', { name: 'Send' }).click()
    assert.equal((await goalQueue).status(), 201)
    await page.locator('[data-projection-dock]').getByText('verify preset switching', { exact: true }).waitFor()
    await page.setViewportSize({ width: 1440, height: 900 })
    const headerActions = page.locator('.session-header-actions')
    const activePreset = headerActions.getByRole('button', { name: 'Agent preset: Settings Browser Preset' })
    await activePreset.and(page.locator(':disabled')).waitFor()
    assert.equal(await activePreset.isDisabled(), true)
    assert.match(await activePreset.getAttribute('title'), /locked after the session starts/)

    await page.setViewportSize({ width: 390, height: 844 })
    await headerActions.getByRole('button', { name: 'More session actions' }).click()
    const mobileCustomOption = page.getByRole('menuitem').filter({ has: page.getByText('Settings Browser Preset', { exact: true }) })
    await mobileCustomOption.waitFor()
    assert.equal(await mobileCustomOption.getAttribute('aria-disabled'), 'true')
    assert.equal(await page.getByRole('menuitem').filter({ has: page.getByText('Standard Mode', { exact: true }) }).getAttribute('aria-disabled'), 'true')
    await page.keyboard.press('Escape')
    await page.setViewportSize({ width: 1440, height: 900 })
    await headerActions.getByRole('button', { name: 'Agent preset: Settings Browser Preset' }).waitFor()

    await page.getByRole('button', { name: 'Settings', exact: true }).click()
    settings = page.getByRole('dialog', { name: 'Settings' })
    await settings.getByRole('button', { name: 'Agent presets', exact: true }).click()
    const removablePreset = settings.locator('article').filter({ hasText: 'settings-browser-preset' })
    await removablePreset.getByRole('button', { name: 'Delete: Settings Browser Preset' }).click()
    const deletePreset = page.getByRole('dialog', { name: 'Delete this preset?' })
    await deletePreset.getByRole('button', { name: 'Delete', exact: true }).click()
    await settings.getByText('settings-browser-preset', { exact: true }).waitFor({ state: 'detached' })
    await page.keyboard.press('Escape')
    await settings.waitFor({ state: 'detached' })
    assert.deepEqual(pageErrors, [])
    assert.deepEqual(consoleErrors, [])
    assert.deepEqual(httpErrors, [])
  } catch (error) {
    if (page && process.env.TERNILO_E2E_ARTIFACT_DIR) await page.screenshot({ path: path.join(process.env.TERNILO_E2E_ARTIFACT_DIR, 'local-settings-failure.png') }).catch(() => {})
    throw new Error(`settings browser acceptance failed: ${app.diagnostics()}`, { cause: error })
  } finally {
    await browser?.close()
    await stop(app.child)
    await catalog.close()
    await rm(dataDirectory, { recursive: true, force: true })
  }
})
