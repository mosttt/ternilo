import assert from 'node:assert/strict'
import { mkdir, mkdtemp, rm } from 'node:fs/promises'
import { createServer } from 'node:http'
import { tmpdir } from 'node:os'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import { spawn } from 'node:child_process'
import test from 'node:test'
import { chromium } from 'playwright'

const webRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..')
const repository = path.resolve(webRoot, '..')
const terniloBinary = path.join(repository, 'target', 'debug', 'ternilo')
const fixtureBinary = path.join(repository, 'target', 'debug', 'examples', 'signed_web_fixture')
const promptSectionId = 'fixture-guidance'
const promptSectionOrder = 450
const promptSectionContent = 'Use the signed Rhai fixture only for deterministic acceptance checks.'
const extensionSkillName = 'signed-extension-fixture'
const extensionSkillDescription = 'Use a signed Extension fixture for browser acceptance checks.'
const extensionSkillWhenToUse = 'Use when verifying signed Extension installation and catalog loading.'
const extensionSkillContent = 'Use the mounted signed Extension fixture only for deterministic browser acceptance checks. Verify its signature before invoking any contributed tool.'
const extensionSkillRequest = 'Run the signed Extension Skill explicitly.'
const extensionHookId = 'guard-fixture'
const extensionCommandName = 'signed-fixture'
const extensionProviderTemplate = 'signed-fixture-provider'
const materializedProviderId = 'signed-fixture-materialized'

function run(child, label) {
  return new Promise((resolve, reject) => {
    let stderr = ''
    child.stderr?.setEncoding('utf8')
    child.stderr?.on('data', chunk => { stderr += chunk })
    child.once('error', reject)
    child.once('exit', code => code === 0 ? resolve() : reject(new Error(`${label} exited ${code}: ${stderr}`)))
  })
}

async function generateSignedFixture(directory) {
  await run(spawn(fixtureBinary, [directory], { cwd: repository, stdio: ['ignore', 'ignore', 'pipe'] }), 'signed fixture generator')
}

function startTernilo(dataDirectory) {
  const child = spawn(terniloBinary, ['serve', '--listen', '127.0.0.1:0', '--data-dir', dataDirectory], {
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
    child.once('exit', code => reject(new Error(`Ternilo exited ${code}: ${diagnostics}`)))
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

function sse(payload) { return `data: ${JSON.stringify(payload)}\n\n` }

function completedText(response, text) {
  response.writeHead(200, { 'content-type': 'text/event-stream', 'cache-control': 'no-cache' })
  response.end([
    sse({ type: 'response.output_text.delta', output_index: 0, content_index: 0, delta: text }),
    sse({ type: 'response.completed', response: {
      status: 'completed',
      output: [{ type: 'message', role: 'assistant', content: [{ type: 'output_text', text }] }],
      usage: { input_tokens: 8, output_tokens: 4, input_tokens_details: { cached_tokens: 0 }, output_tokens_details: { reasoning_tokens: 0 } },
    } }),
  ].join(''))
}

async function startModelFixture() {
  const requests = []
  const server = createServer((incoming, response) => {
    if (incoming.method !== 'POST' || incoming.url !== '/v1/responses') {
      response.writeHead(404).end()
      return
    }
    const chunks = []
    incoming.on('data', chunk => chunks.push(chunk))
    incoming.on('end', () => {
      const request = JSON.parse(Buffer.concat(chunks).toString('utf8'))
      if (typeof request.instructions === 'string' && request.instructions.includes('You name software-agent conversations')) {
        completedText(response, 'Rhai 扩展验收')
        return
      }
      requests.push(request)
      const last = Array.isArray(request.input) ? request.input.at(-1) : undefined
      if (last?.type === 'function_call_output') {
        completedText(response, `Model observed Rhai output: ${last.output}`)
        return
      }
      const prompt = JSON.stringify(last ?? request.input)
      if (prompt.includes('调用签名 Rhai 工具')) {
        const call = { type: 'function_call', call_id: 'rhai-call', name: 'signed_rhai_fixture', arguments: '{"subject":"browser"}' }
        response.writeHead(200, { 'content-type': 'text/event-stream', 'cache-control': 'no-cache' })
        response.end([
          sse({ type: 'response.output_item.added', output_index: 0, item: { ...call, arguments: '' } }),
          sse({ type: 'response.function_call_arguments.done', output_index: 0, arguments: call.arguments }),
          sse({ type: 'response.completed', response: {
            status: 'completed', output: [call],
            usage: { input_tokens: 10, output_tokens: 2, input_tokens_details: { cached_tokens: 0 }, output_tokens_details: { reasoning_tokens: 0 } },
          } }),
        ].join(''))
        return
      }
      completedText(response, 'The signed Rhai fixture tool is unavailable.')
    })
  })
  await new Promise((resolve, reject) => {
    server.once('error', reject)
    server.listen(0, '127.0.0.1', resolve)
  })
  return {
    baseUrl: `http://127.0.0.1:${server.address().port}/v1`,
    requests,
    close: () => new Promise((resolve, reject) => server.close(error => error ? reject(error) : resolve())),
  }
}

async function api(page, endpoint, init = {}) {
  return page.evaluate(async ({ endpoint, init }) => {
    const token = window.__TERNILO_BOOT__?.apiToken ?? ''
    const response = await fetch(`/api/v1${endpoint}`, {
      cache: 'no-store',
      method: init.method ?? 'GET',
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

function toolNames(request) {
  return (request.tools ?? []).map(tool => tool.name).filter(Boolean)
}

function latestUserInputText(request) {
  const message = Array.isArray(request?.input) ? request.input.at(-1) : request?.input
  if (typeof message === 'string') return message
  if (typeof message?.content === 'string') return message.content
  if (!Array.isArray(message?.content)) return ''
  return message.content
    .filter(part => part?.type === 'input_text' && typeof part.text === 'string')
    .map(part => part.text)
    .join('\n')
}

test('signed Rhai extension closes the real Local Web install, settings, runtime, and trace flow', { timeout: 150_000 }, async () => {
  const dataDirectory = await mkdtemp(path.join(tmpdir(), 'ternilo-rhai-browser-'))
  const workspacePath = path.join(dataDirectory, 'workspace')
  const fixtureDirectory = path.join(dataDirectory, 'fixture')
  await Promise.all([mkdir(workspacePath), mkdir(fixtureDirectory)])
  await generateSignedFixture(fixtureDirectory)
  const model = await startModelFixture()
  const app = startTernilo(dataDirectory)
  let browser
  const pageErrors = []
  const consoleErrors = []
  const apiRequests = []
  try {
    browser = await chromium.launch({
      headless: true,
      executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined,
    })
    const page = await browser.newPage({ viewport: { width: 1280, height: 820 } })
    page.on('pageerror', error => pageErrors.push(error.message))
    page.on('console', message => {
      if (message.type() === 'error') consoleErrors.push(message.text())
    })
    page.on('request', request => {
      const url = new URL(request.url())
      if (url.pathname.startsWith('/api/v1/')) apiRequests.push({ method: request.method(), path: url.pathname })
    })
    await page.goto(await app.origin, { waitUntil: 'domcontentloaded' })
    const workspace = await api(page, '/workspaces', { method: 'POST', body: { path: workspacePath } })
    const session = await api(page, '/sessions', { method: 'POST', body: { workspace_id: workspace.workspace_id, agent_preset: 'standard' } })
    const sessionId = session.identity.session_id
    await api(page, '/providers', { method: 'POST', body: {
      id: 'rhai-browser', display_name: 'Rhai Browser', base_url: model.baseUrl,
      protocol: 'openai-responses', api_key_ref: null,
      defaults: { context_window: 32_000, max_output_tokens: 2_048 },
      models: [{ id: 'fixture-model', display_name: 'Fixture Model', settings: { mode: 'inherit' } }],
      timeout_ms: 30_000, max_attempts: 1, retry_base_delay_ms: 25,
    } })
    await api(page, `/sessions/${encodeURIComponent(sessionId)}`, { method: 'PATCH', body: {
      model: { provider: 'named_provider', provider_id: 'rhai-browser', model: 'fixture-model' },
    } })
    await page.reload({ waitUntil: 'domcontentloaded' })
    await page.getByRole('textbox', { name: '输入任务' }).waitFor()

    await page.getByRole('button', { name: '设置' }).click()
    const settings = page.getByRole('dialog', { name: '设置' })
    await settings.getByRole('button', { name: '插件', exact: true }).click()
    await settings.getByRole('tab', { name: '扩展包' }).click()
    await settings.locator('#plugin-publisher-file').setInputFiles(path.join(fixtureDirectory, 'publisher.json'))
    await settings.getByText('ternilo-browser-fixture', { exact: true }).waitFor()
    await settings.locator('#plugin-bundle-file').setInputFiles(path.join(fixtureDirectory, 'rhai-bundle.json'))
    const review = page.getByRole('dialog', { name: '审阅并安装扩展包' })
    await review.waitFor()
    assert.match(await review.textContent(), /dev\.ternilo\.browser-rhai-fixture.*rhai/s)
    assert.match(await review.locator('[data-extension-source-review]').textContent(), /fn signed_rhai.*settings\.prefix/s)
    const promptReview = review.locator(`[data-extension-prompt-section-review="${promptSectionId}"]`)
    await promptReview.waitFor()
    assert.match(await promptReview.textContent(), new RegExp(`${promptSectionId}.*顺序 ${promptSectionOrder}.*${promptSectionContent}`, 's'))
    const skillReview = review.locator(`[data-extension-skill-review="${extensionSkillName}"]`)
    await skillReview.waitFor()
    assert.match(await skillReview.textContent(), new RegExp(`${extensionSkillName}.*${extensionSkillDescription}.*${extensionSkillWhenToUse}.*模型调用: 允许.*用户调用: 允许.*${extensionSkillContent}`, 's'))
    const hookReview = review.locator(`[data-extension-hook-review="${extensionHookId}"]`)
    const commandReview = review.locator(`[data-extension-command-review="${extensionCommandName}"]`)
    const providerReview = review.locator(`[data-extension-provider-review="${extensionProviderTemplate}"]`)
    await Promise.all([hookReview.waitFor(), commandReview.waitFor(), providerReview.waitFor()])
    assert.match(await hookReview.textContent(), /guard-fixture.*pre_tool_use.*guard_fixture.*fixture-never-invoked/s)
    assert.match(await commandReview.textContent(), /\/signed-fixture.*signed_rhai_fixture.*Invoke the signed Rhai fixture tool\..*<subject>.*subject.*signed-browser-fixture/s)
    assert.match(await providerReview.textContent(), /Signed Fixture Provider.*signed-fixture-provider.*openai-responses.*api\.example\.test.*signed-fixture-model.*SIGNED_FIXTURE_API_KEY/s)
    await page.setViewportSize({ width: 390, height: 844 })
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= document.documentElement.clientWidth), true)
    assert.equal(await review.evaluate(element => element.scrollWidth <= element.clientWidth), true)
    assert.equal(await skillReview.evaluate(element => element.scrollWidth <= element.clientWidth), true)
    assert.equal(await hookReview.evaluate(element => element.scrollWidth <= element.clientWidth), true)
    assert.equal(await commandReview.evaluate(element => element.scrollWidth <= element.clientWidth), true)
    assert.equal(await providerReview.evaluate(element => element.scrollWidth <= element.clientWidth), true)
    assert.equal(await skillReview.locator('[data-extension-skill-content]').evaluate(element => element.scrollWidth <= element.clientWidth), true)
    assert.match(await promptReview.locator('pre').textContent(), new RegExp(promptSectionContent.replaceAll('.', '\\.')))
    assert.equal(await skillReview.locator('[data-extension-skill-content]').textContent(), extensionSkillContent)
    await page.setViewportSize({ width: 1280, height: 820 })
    await review.getByRole('button', { name: '确认安装' }).click()
    await review.waitFor({ state: 'detached' })

    const packageRow = settings.locator('[data-extension-package="dev.ternilo.browser-rhai-fixture@1.0.0"]')
    await packageRow.waitFor()
    assert.equal(await packageRow.locator('[data-extension-runtime]').textContent(), 'rhai')
    const installedPrompt = packageRow.locator(`[data-extension-prompt-section="${promptSectionId}"]`)
    await installedPrompt.waitFor()
    assert.match(await installedPrompt.textContent(), new RegExp(`${promptSectionId}.*顺序 ${promptSectionOrder}.*${promptSectionContent}`, 's'))
    const installedSkill = packageRow.locator(`[data-extension-skill="${extensionSkillName}"]`)
    await installedSkill.waitFor()
    assert.match(await installedSkill.textContent(), new RegExp(`${extensionSkillName}.*${extensionSkillDescription}.*${extensionSkillWhenToUse}.*模型调用: 允许.*用户调用: 允许.*${extensionSkillContent}`, 's'))
    const installedHook = packageRow.locator(`[data-extension-hook="${extensionHookId}"]`)
    const installedCommand = packageRow.locator(`[data-extension-command="${extensionCommandName}"]`)
    const installedProvider = packageRow.locator(`[data-extension-provider="${extensionProviderTemplate}"]`)
    await Promise.all([installedHook.waitFor(), installedCommand.waitFor(), installedProvider.waitFor()])
    assert.match(await installedHook.textContent(), /guard-fixture.*pre_tool_use.*guard_fixture.*fixture-never-invoked/s)
    assert.match(await installedCommand.textContent(), /\/signed-fixture.*signed_rhai_fixture.*Invoke the signed Rhai fixture tool\..*<subject>.*subject.*signed-browser-fixture/s)
    assert.match(await installedProvider.textContent(), /Signed Fixture Provider.*signed-fixture-provider.*openai-responses.*api\.example\.test.*signed-fixture-model.*SIGNED_FIXTURE_API_KEY/s)
    const inventory = await api(page, '/extensions')
    const installed = inventory.extensions.find(entry => entry.manifest.package_id === 'dev.ternilo.browser-rhai-fixture')
    assert.equal(installed.manifest.schema_version, 1)
    assert.equal(installed.manifest.runtime.kind, 'rhai')

    await installedProvider.getByRole('button').click()
    const providerDialog = page.locator('[data-extension-provider-dialog]')
    await providerDialog.waitFor()
    const providerId = providerDialog.locator('#extension-provider-id')
    const credentialReference = providerDialog.locator('#extension-provider-credential-ref')
    assert.equal(await providerId.inputValue(), extensionProviderTemplate)
    assert.equal(await credentialReference.inputValue(), 'SIGNED_FIXTURE_API_KEY')
    await providerId.fill(materializedProviderId)
    await providerDialog.locator('button[type="submit"]').click()
    await providerDialog.waitFor({ state: 'detached' })
    const providers = await api(page, '/providers')
    const materializedProvider = providers.find(provider => provider.id === materializedProviderId)
    assert.deepEqual(materializedProvider, {
      id: materializedProviderId,
      display_name: 'Signed Fixture Provider',
      base_url: 'https://api.example.test/v1',
      protocol: 'openai-responses',
      api_key_ref: 'SIGNED_FIXTURE_API_KEY',
      defaults: { context_window: 128_000, max_output_tokens: 16_384 },
      models: [{
        id: 'signed-fixture-model',
        display_name: 'Signed Fixture Model',
        settings: { mode: 'inherit' },
      }],
      timeout_ms: 120_000,
      max_attempts: 3,
      retry_base_delay_ms: 250,
    })
    await settings.getByRole('button', { name: '模型', exact: true }).click()
    await settings.getByText('Signed Fixture Provider', { exact: true }).waitFor()
    assert.match(await settings.textContent(), /signed-fixture-materialized.*openai-responses/s)
    await settings.getByRole('button', { name: '插件', exact: true }).click()
    await settings.getByRole('tab', { name: '扩展包' }).click()
    await packageRow.waitFor()

    const mount = packageRow.locator('[data-extension-mount]')
    await mount.click()
    await page.waitForFunction(() => document.querySelector('[data-extension-mount]')?.getAttribute('aria-checked') === 'true')
    let state = await api(page, '/state')
    let mounted = state.sessions
      .find(item => item.identity.session_id === sessionId)
      .profile_plugins.find(entry => entry.kind === 'ternilo.extension.package')
    assert.deepEqual(mounted.config.settings, { prefix: 'Signed Rhai: ' })
    const skillCatalog = await api(page, `/sessions/${encodeURIComponent(sessionId)}/skills`)
    const extensionSkill = skillCatalog.skills.find(skill => skill.name === extensionSkillName)
    assert.equal(extensionSkill?.description, extensionSkillDescription)
    assert.equal(extensionSkill?.when_to_use, extensionSkillWhenToUse)
    assert.deepEqual(extensionSkill?.invocation, { model_invocable: true, user_invocable: true })

    const extensionSettings = packageRow.locator('[data-extension-settings="dev.ternilo.browser-rhai-fixture@1.0.0"]')
    await extensionSettings
      .getByRole('button', { name: '展开: 会话设置：dev.ternilo.browser-rhai-fixture' })
      .click()
    const prefix = extensionSettings.getByLabel('Rhai response prefix')
    assert.equal(await prefix.inputValue(), 'Signed Rhai: ')
    await prefix.fill('Browser Rhai: ')
    await extensionSettings.getByRole('button', { name: '保存', exact: true }).click()
    await page.getByText('扩展包会话设置已保存', { exact: true }).waitFor()
    state = await api(page, '/state')
    mounted = state.sessions
      .find(item => item.identity.session_id === sessionId)
      .profile_plugins.find(entry => entry.kind === 'ternilo.extension.package')
    assert.deepEqual(mounted.config.settings, { prefix: 'Browser Rhai: ' })
    assert.equal((await api(page, `/sessions/${encodeURIComponent(sessionId)}/plugins`)).plugins
      .some(entry => entry.kind === 'ternilo.extension.package' && entry.enabled), true)
    await settings.getByRole('button', { name: '关闭设置' }).click()

    const input = page.getByRole('textbox', { name: '输入任务' })
    await input.fill('调用签名 Rhai 工具')
    await page.getByRole('button', { name: '发送' }).click()
    await page.getByText(new RegExp(`Model observed Rhai output: Browser Rhai: browser \\(${sessionId}\\)`)).waitFor()
    const tool = page.locator('[data-tool-call-id="rhai-call"]')
    await tool.waitFor({ state: 'attached' })
    if (!await tool.isVisible()) {
      await tool.locator('xpath=ancestor::*[@data-chat-turn][1]').locator('[data-turn-process]').click()
    }
    await tool.waitFor()
    await page.waitForFunction(() => document.querySelector('[data-tool-call-id="rhai-call"]')?.getAttribute('data-state') === 'complete')
    assert.equal(await tool.getAttribute('data-tool-contribution'), 'builtin.declarative')
    assert.match(await tool.textContent(), /Signed Rhai response.*Subject: browser/)
    await tool.locator('[data-tool-call-toggle]').click()
    assert.match(await tool.textContent(), new RegExp(`Browser Rhai: browser \\(${sessionId}\\)`))
    assert.equal(toolNames(model.requests[0]).includes('signed_rhai_fixture'), true)
    assert.match(model.requests[0].instructions, new RegExp(promptSectionContent.replaceAll('.', '\\.')))
    const systemPrompt = page.locator('[data-system-prompt-row]').last()
    await systemPrompt.locator('[data-disclosure-row]').click()
    assert.match(await systemPrompt.locator('[data-system-prompt-body]').textContent(), new RegExp(promptSectionContent.replaceAll('.', '\\.')))

    await api(page, `/sessions/${encodeURIComponent(sessionId)}/skills/${encodeURIComponent(extensionSkillName)}/turns`, {
      method: 'POST',
      body: { input: extensionSkillRequest, run_id: null, attachments: [] },
    })
    const skillRequest = model.requests.at(-1)
    const skillModelInput = latestUserInputText(skillRequest)
    assert.equal(skillModelInput.includes(`<skill_content name="${extensionSkillName}">`), true, skillModelInput)
    assert.equal(skillModelInput.includes(extensionSkillContent), true, skillModelInput)
    assert.equal(skillModelInput.includes('<user_request>'), true, skillModelInput)
    assert.equal(skillModelInput.includes(extensionSkillRequest), true, skillModelInput)
    const skillUserMessage = page.locator('article[data-role="user"]')
      .filter({ hasText: `/skill ${extensionSkillName}` }).last()
    await skillUserMessage.waitFor()
    const skillUserText = await skillUserMessage.textContent()
    assert.match(skillUserText, new RegExp(`/skill ${extensionSkillName}`))
    assert.match(skillUserText, new RegExp(extensionSkillRequest.replaceAll('.', '\\.')))
    assert.equal(skillUserText.includes(extensionSkillContent), false)
    const skillEvents = await api(page, `/sessions/${encodeURIComponent(sessionId)}/events`)
    const persistedSkillMessage = skillEvents.filter(event => (
      event.type === 'user_message' &&
      event.source?.kind === 'skill_invocation' &&
      event.source?.name === extensionSkillName
    )).at(-1)
    assert.ok(persistedSkillMessage)
    assert.equal(persistedSkillMessage.display_content, `/skill ${extensionSkillName}\n\n${extensionSkillRequest}`)
    assert.match(persistedSkillMessage.content, new RegExp(extensionSkillContent.replaceAll('.', '\\.')))

    await page.getByRole('tab', { name: '轨迹' }).click()
    const trajectoryTool = page.locator('[data-trajectory-record][data-kind="tool"]').filter({ hasText: 'Signed Rhai response' })
    await trajectoryTool.waitFor()
    assert.match(await trajectoryTool.textContent(), new RegExp(`Subject: browser.*Browser Rhai: browser \\(${sessionId}\\)`, 's'))
    await trajectoryTool.click()
    const details = page.locator('.details-panel[data-details-state="ready"]')
    await details.waitFor()
    assert.equal(await details.locator('h2').textContent(), 'Signed Rhai response')
    assert.match(await details.textContent(), new RegExp(`browser.*Browser Rhai: browser \\(${sessionId}\\)`, 's'))
    await details.getByRole('button', { name: '关闭详情' }).click()
    await page.getByRole('tab', { name: '对话' }).click()

    await page.reload({ waitUntil: 'domcontentloaded' })
    const durableTool = page.locator('[data-tool-call-id="rhai-call"]')
    await durableTool.waitFor({ state: 'attached' })
    if (!await durableTool.isVisible()) {
      await durableTool.locator('xpath=ancestor::*[@data-chat-turn][1]').locator('[data-turn-process]').click()
    }
    await durableTool.waitFor()
    assert.equal(await durableTool.getAttribute('data-tool-contribution'), 'builtin.declarative')
    assert.match(await durableTool.textContent(), /Signed Rhai response.*Subject: browser/)
    const durableSystemPrompt = page.locator('[data-system-prompt-row]').last()
    await durableSystemPrompt.locator('[data-disclosure-row]').click()
    assert.match(await durableSystemPrompt.locator('[data-system-prompt-body]').textContent(), new RegExp(promptSectionContent.replaceAll('.', '\\.')))

    await page.getByRole('button', { name: '设置' }).click()
    const reopenedSettings = page.getByRole('dialog', { name: '设置' })
    await reopenedSettings.getByRole('button', { name: '插件', exact: true }).click()
    await reopenedSettings.getByRole('tab', { name: '扩展包' }).click()
    const reopenedPackage = reopenedSettings.locator('[data-extension-package="dev.ternilo.browser-rhai-fixture@1.0.0"]')
    const reopenedMount = reopenedPackage.locator('[data-extension-mount]')
    await reopenedMount.click()
    await page.waitForFunction(() => document.querySelector('[data-extension-mount]')?.getAttribute('aria-checked') === 'false')
    const unmountedSkills = await api(page, `/sessions/${encodeURIComponent(sessionId)}/skills`)
    assert.equal(unmountedSkills.skills.some(skill => skill.name === extensionSkillName), false)
    await reopenedMount.click()
    await page.waitForFunction(() => document.querySelector('[data-extension-mount]')?.getAttribute('aria-checked') === 'true')
    const remountedSkills = await api(page, `/sessions/${encodeURIComponent(sessionId)}/skills`)
    const remountedSkill = remountedSkills.skills.find(skill => skill.name === extensionSkillName)
    assert.equal(remountedSkill?.description, extensionSkillDescription)
    assert.deepEqual(remountedSkill?.invocation, { model_invocable: true, user_invocable: true })
    await reopenedSettings.getByRole('button', { name: '关闭设置' }).click()

    const sawApiRequest = (method, requestPath) => apiRequests.some(request => request.method === method && request.path === requestPath)
    assert.equal(sawApiRequest('GET', '/api/v1/extensions'), true, JSON.stringify(apiRequests))
    assert.equal(sawApiRequest('POST', '/api/v1/extensions/publishers'), true, JSON.stringify(apiRequests))
    assert.equal(sawApiRequest('POST', '/api/v1/extensions'), true, JSON.stringify(apiRequests))
    assert.equal(sawApiRequest('POST', '/api/v1/providers/from-extension'), true, JSON.stringify(apiRequests))
    assert.equal(sawApiRequest('GET', `/api/v1/sessions/${sessionId}/skills`), true, JSON.stringify(apiRequests))
    assert.equal(apiRequests.some(request => request.path === '/api/v1/plugins'), false, JSON.stringify(apiRequests))
    assert.deepEqual(consoleErrors, [])
    assert.deepEqual(pageErrors, [])
  } catch (cause) {
    throw new Error(`${cause instanceof Error ? cause.message : String(cause)}\nserver: ${app.diagnostics()}`, { cause })
  } finally {
    await browser?.close()
    await stop(app.child)
    await model.close()
    await rm(dataDirectory, { recursive: true, force: true })
  }
})
