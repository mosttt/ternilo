import { selectChoice } from './browser-select-fixture.mjs'
import assert from 'node:assert/strict'
import { mkdir, mkdtemp, readFile, readdir, rm } from 'node:fs/promises'
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
    env: { ...process.env, TERNILO_TEST_AUTHORIZATION_FIXTURE: '1' },
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
        completedText(response, 'WASM 插件验收')
        return
      }
      requests.push(request)
      const last = Array.isArray(request.input) ? request.input.at(-1) : undefined
      if (last?.type === 'function_call_output') {
        completedText(response, `Signed WASM result: ${last.output}`)
        return
      }
      const prompt = JSON.stringify(last ?? request.input)
      if (prompt.includes('调用签名 WASM 工具')) {
        const call = { type: 'function_call', call_id: 'wasm-call', name: 'signed_fixture', arguments: '{"subject":"browser"}' }
        response.writeHead(200, { 'content-type': 'text/event-stream', 'cache-control': 'no-cache' })
        response.end([
          sse({ type: 'response.output_item.added', output_index: 0, item: { ...call, arguments: '' } }),
          sse({ type: 'response.function_call_arguments.done', output_index: 0, arguments: '{"subject":"browser"}' }),
          sse({ type: 'response.completed', response: {
            status: 'completed', output: [call],
            usage: { input_tokens: 10, output_tokens: 2, input_tokens_details: { cached_tokens: 0 }, output_tokens_details: { reasoning_tokens: 0 } },
          } }),
        ].join(''))
        return
      }
      completedText(response, 'The signed fixture tool is unavailable.')
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

test('plugin authorization and signed WASM close real Local Web lifecycles', { timeout: 180_000 }, async () => {
  const dataDirectory = await mkdtemp(path.join(tmpdir(), 'ternilo-auth-wasm-browser-'))
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
    browser = await chromium.launch({ headless: true })
    const page = await browser.newPage({ locale: 'zh-CN', viewport: { width: 1280, height: 820 } })
    page.on('pageerror', error => pageErrors.push(error.message))
    page.on('console', message => {
      if (message.type() === 'error') consoleErrors.push(message.text())
    })
    page.on('request', request => {
      const url = new URL(request.url())
      if (url.pathname.startsWith('/api/v1/')) {
        apiRequests.push({ method: request.method(), path: url.pathname })
      }
    })
    await page.goto(await app.origin, { waitUntil: 'domcontentloaded' })
    const workspace = await api(page, '/workspaces', { method: 'POST', body: { path: workspacePath } })
    const session = await api(page, '/sessions', { method: 'POST', body: { workspace_id: workspace.workspace_id, agent_preset: 'standard' } })
    const sessionId = session.identity.session_id
    await api(page, '/providers', { method: 'POST', body: {
      id: 'auth-wasm-browser', display_name: 'Auth WASM Browser', base_url: model.baseUrl,
      protocol: 'openai-responses', api_key_ref: null,
      defaults: { context_window: 32_000, max_output_tokens: 2_048 },
      models: [{ id: 'fixture-model', display_name: 'Fixture Model', settings: { mode: 'inherit' } }],
      timeout_ms: 30_000, max_attempts: 1, retry_base_delay_ms: 25,
    } })
    await api(page, `/sessions/${encodeURIComponent(sessionId)}`, { method: 'PATCH', body: {
      model: { provider: 'named_provider', provider_id: 'auth-wasm-browser', model: 'fixture-model' },
    } })
    await page.reload({ waitUntil: 'domcontentloaded' })
    await page.getByRole('textbox', { name: '输入任务' }).waitFor()

    await page.getByRole('button', { name: '设置' }).click()
    let settings = page.getByRole('dialog', { name: '设置' })
    await settings.getByRole('button', { name: '凭据与登录', exact: true }).click()
    const flow = settings.locator('[data-authorization-flow="record:dev.ternilo.authorization-fixture/device-code"]')
    await flow.waitFor({ timeout: 8_000 }).catch(async cause => {
      const snapshot = await api(page, '/authorizations?surface_id=browser-debug')
      throw new Error(`authorization flow missing; snapshot=${JSON.stringify(snapshot)}; settings=${await settings.innerText()}`, { cause })
    })
    assert.equal(await flow.getAttribute('data-authorization-state'), 'ready')
    assert.equal((await settings.textContent()).includes('API key ·'), false, 'Provider keys must not appear as plugin authorization flows')

    await flow.getByRole('button', { name: 'Sign in with device code' }).click()
    await flow.locator('code').filter({ hasText: 'TERNILO-42' }).waitFor()
    assert.equal(await flow.getAttribute('data-authorization-state'), 'running')
    await flow.getByRole('button', { name: '取消' }).click()
    await page.waitForFunction(() => document.querySelector('[data-authorization-flow]')?.getAttribute('data-authorization-state') === 'cancelled')
    assert.equal((await api(page, '/credentials')).records.some(record => record.key === 'dev.ternilo.authorization-fixture/device-code'), false)

    await flow.getByRole('button', { name: 'Sign in with device code' }).click()
    const prompt = flow.locator('select')
    await prompt.waitFor()
    await selectChoice(prompt, 'complete')
    await flow.getByRole('button', { name: '提交' }).click()
    await page.waitForFunction(() => document.querySelector('[data-authorization-flow]')?.getAttribute('data-authorization-state') === 'authorized')
    assert.equal((await api(page, '/credentials')).records.some(record => record.key === 'dev.ternilo.authorization-fixture/device-code' && record.kind === 'grant'), true)
    const credentialDocument = JSON.parse(await readFile(path.join(dataDirectory, 'credential-records.json'), 'utf8'))
    assert.equal(credentialDocument['dev.ternilo.authorization-fixture/device-code'].payload.authorized, true)

    await page.setViewportSize({ width: 390, height: 844 })
    assert.equal(await settings.evaluate(element => element.scrollWidth <= element.clientWidth), true)
    await settings.getByRole('button', { name: '通用', exact: true }).click()
    await selectChoice(settings.getByLabel('语言'), 'en')
    settings = page.getByRole('dialog', { name: 'Settings' })
    await settings.getByRole('button', { name: 'Credentials & sign-in', exact: true }).click()
    await settings.getByText('Authorization complete; credential saved', { exact: true }).waitFor()
    await settings.getByRole('button', { name: 'General', exact: true }).click()
    await selectChoice(settings.getByLabel('Language'), 'zh')
    settings = page.getByRole('dialog', { name: '设置' })
    await page.setViewportSize({ width: 1280, height: 820 })

    await settings.getByRole('button', { name: '插件', exact: true }).click()
    await settings.getByRole('tab', { name: '扩展包' }).click()
    await settings.locator('#plugin-publisher-file').setInputFiles(path.join(fixtureDirectory, 'publisher.json'))
    await settings.getByText('ternilo-browser-fixture', { exact: true }).waitFor()
    await settings.locator('#plugin-bundle-file').setInputFiles(path.join(fixtureDirectory, 'bundle.json'))
    const review = page.getByRole('dialog', { name: '审阅并安装扩展包' })
    await review.waitFor()
    assert.match(await review.textContent(), /dev\.ternilo\.browser-fixture/)
    assert.match(await review.textContent(), /ternilo-browser-fixture/)
    assert.match(await review.locator('[data-extension-presentation-review]').textContent(), /Signed fixture report.*sparkles.*table.*1/)
    assert.equal(await review.getByRole('checkbox', { name: /log/ }).isChecked(), true)
    assert.equal(await review.getByRole('checkbox', { name: /workspace_read/ }).isChecked(), true)
    await review.getByRole('button', { name: '确认安装' }).click()
    await review.waitFor({ state: 'detached' })
    const artifactDirectory = path.join(dataDirectory, 'extensions', 'artifacts')
    assert.equal((await readdir(artifactDirectory)).length, 1)
    const inheritedMount = {
      id: 'preset-signed-fixture',
      kind: 'ternilo.extension.package',
      enabled: true,
      config: {
        package_id: 'dev.ternilo.browser-fixture',
        version: '1.0.0',
        settings: { salutation: 'hello from preset' },
      },
    }
    const copiedPreset = await api(page, '/agent-presets', { method: 'POST', body: {
      from: 'standard', id: 'browser-extension-preset', display_name: 'Browser extension preset',
    } })
    await api(page, '/agent-presets/browser-extension-preset', { method: 'PUT', body: {
      display_name: 'Browser extension preset',
      description: 'Exercises inherited Extension Package mounts.',
      profile: { plugins: [...copiedPreset.profile.plugins, inheritedMount] },
    } })
    await api(page, `/sessions/${encodeURIComponent(sessionId)}`, { method: 'PATCH', body: {
      agent_preset: 'browser-extension-preset',
    } })
    await page.reload({ waitUntil: 'domcontentloaded' })
    await page.getByRole('button', { name: '设置' }).click()
    settings = page.getByRole('dialog', { name: '设置' })
    await settings.getByRole('button', { name: '插件', exact: true }).click()
    await settings.getByRole('tab', { name: '扩展包' }).click()
    const packageRow = settings.locator('[data-extension-package="dev.ternilo.browser-fixture@1.0.0"]')
    await packageRow.waitFor()
    const mount = packageRow.locator('[data-extension-mount]')
    await page.waitForFunction(() => document.querySelector('[data-extension-mount]')?.getAttribute('aria-checked') === 'true')
    assert.equal(await mount.getAttribute('aria-checked'), 'true')
    assert.equal(await mount.getAttribute('aria-label'), '从当前会话移除扩展包 dev.ternilo.browser-fixture')
    const inheritedState = await api(page, '/state')
    const inheritedSession = inheritedState.sessions.find(item => item.identity.session_id === sessionId)
    assert.equal(inheritedSession.profile_plugins.some(entry => entry.kind === 'ternilo.extension.package'), false)
    assert.deepEqual(
      inheritedSession.preset_plugins.find(entry => entry.kind === 'ternilo.extension.package'),
      inheritedMount,
    )
    const extensionSettings = packageRow.locator(
      '[data-extension-settings="dev.ternilo.browser-fixture@1.0.0"]',
    )
    await extensionSettings
      .getByRole('button', { name: '展开: 会话设置：dev.ternilo.browser-fixture' })
      .click()
    const salutation = extensionSettings.getByLabel('Fixture salutation')
    assert.equal(await salutation.inputValue(), 'hello from preset')
    await salutation.fill('hello from browser settings')
    await extensionSettings.getByRole('button', { name: '保存', exact: true }).click()
    await page.getByText('扩展包会话设置已保存', { exact: true }).waitFor()
    const configuredState = await api(page, '/state')
    const configuredExtension = configuredState.sessions
      .find(item => item.identity.session_id === sessionId)
      .profile_plugins.find(entry => entry.kind === 'ternilo.extension.package')
    assert.deepEqual(configuredExtension, {
      ...inheritedMount,
      config: {
        ...inheritedMount.config,
        settings: { salutation: 'hello from browser settings' },
      },
    })
    await mount.click()
    await page.waitForFunction(() => document.querySelector('[data-extension-mount]')?.getAttribute('aria-checked') === 'false')
    const unmountedState = await api(page, '/state')
    assert.deepEqual(
      unmountedState.sessions
        .find(item => item.identity.session_id === sessionId)
        .profile_plugins.find(entry => entry.kind === 'ternilo.extension.package'),
      { ...configuredExtension, enabled: false },
    )
    assert.equal(
      (await api(page, `/sessions/${encodeURIComponent(sessionId)}/plugins`)).plugins
        .find(entry => entry.kind === 'ternilo.extension.package')?.enabled,
      false,
    )
    await mount.click()
    await page.waitForFunction(() => document.querySelector('[data-extension-mount]')?.getAttribute('aria-checked') === 'true')
    const remountedState = await api(page, '/state')
    assert.deepEqual(
      remountedState.sessions
        .find(item => item.identity.session_id === sessionId)
        .profile_plugins.find(entry => entry.kind === 'ternilo.extension.package'),
      configuredExtension,
    )
    await settings.getByRole('button', { name: '关闭设置' }).click()

    const input = page.getByRole('textbox', { name: '输入任务' })
    await input.fill('调用签名 WASM 工具')
    await page.getByRole('button', { name: '发送' }).click()
    const approval = page.locator('[data-tool-approval]')
    await approval.waitFor()
    assert.match(await approval.textContent(), /Signed fixture report/)
    await approval.getByRole('button', { name: '查看调用详情' }).click()
    let details = page.locator('.details-panel[data-details-state="loading"]')
    await details.waitFor()
    assert.equal(await details.locator('h2').textContent(), 'Signed fixture report')
    assert.match(await details.textContent(), /browser/)
    await details.getByRole('button', { name: '关闭详情' }).click()
    await approval.getByRole('button', { name: '允许一次' }).click()
    await approval.waitFor({ state: 'detached' })
    await page.getByText(/Signed WASM result:.*hello from fixture/).waitFor()
    const tool = page.locator('[data-tool-call-id="wasm-call"]')
    await tool.waitFor({ state: 'attached' })
    if (!await tool.isVisible()) {
      await tool.locator('xpath=ancestor::*[@data-chat-turn][1]').locator('[data-turn-process]').click()
    }
    await tool.waitFor()
    await page.waitForFunction(() => document.querySelector('[data-tool-call-id="wasm-call"]')?.getAttribute('data-state') === 'complete')
    assert.equal(await tool.getAttribute('data-tool-contribution'), 'builtin.declarative')
    assert.match(await tool.textContent(), /Signed fixture report.*Subject: browser/)
    await tool.locator('[data-tool-call-toggle]').click()
    assert.match(await tool.textContent(), /Trust.*Message.*signed.*hello from fixture/)
    assert.equal(toolNames(model.requests[0]).includes('signed_fixture'), true)
    await page.getByRole('tab', { name: '轨迹' }).click()
    const trajectoryTool = page.locator('[data-trajectory-record][data-kind="tool"]').filter({ hasText: 'Signed fixture report' })
    await trajectoryTool.waitFor()
    assert.match(await trajectoryTool.textContent(), /Subject: browser.*hello from fixture/)
    await trajectoryTool.click()
    details = page.locator('.details-panel[data-details-state="ready"]')
    await details.waitFor()
    assert.equal(await details.locator('h2').textContent(), 'Signed fixture report')
    assert.match(await details.textContent(), /browser.*signed.*hello from fixture/s)
    const detailsTable = details.locator('[data-details-tool-presentation][data-tool-contribution="builtin.declarative"] [data-tool-view="declarative-table"]')
    await detailsTable.waitFor()
    assert.deepEqual(await detailsTable.locator('th').allTextContents(), ['Trust', 'Message'])
    assert.deepEqual(await detailsTable.locator('td').allTextContents(), ['signed', 'hello from fixture'])
    await details.getByRole('button', { name: '关闭详情' }).click()
    await page.getByRole('tab', { name: '对话' }).click()

    await page.reload({ waitUntil: 'domcontentloaded' })
    const durableTool = page.locator('[data-tool-call-id="wasm-call"]')
    await durableTool.waitFor({ state: 'attached' })
    if (!await durableTool.isVisible()) {
      await durableTool.locator('xpath=ancestor::*[@data-chat-turn][1]').locator('[data-turn-process]').click()
    }
    await durableTool.waitFor()
    assert.equal(await durableTool.getAttribute('data-tool-contribution'), 'builtin.declarative')
    assert.match(await durableTool.textContent(), /Signed fixture report.*Subject: browser/)

    await page.getByRole('button', { name: '设置' }).click()
    settings = page.getByRole('dialog', { name: '设置' })
    await settings.getByRole('button', { name: '插件', exact: true }).click()
    await settings.getByRole('tab', { name: '扩展包' }).click()
    const installed = settings.locator('[data-extension-package="dev.ternilo.browser-fixture@1.0.0"]')
    await installed.getByRole('switch', { name: '停用扩展包 dev.ternilo.browser-fixture' }).click()
    await installed.getByRole('switch', { name: '启用扩展包 dev.ternilo.browser-fixture' }).waitFor()
    await page.waitForFunction(() => document.querySelector('[data-extension-mount]')?.getAttribute('aria-checked') === 'false')
    const disabledState = await api(page, '/state')
    const disabledSession = disabledState.sessions.find(item => item.identity.session_id === sessionId)
    assert.deepEqual(
      disabledSession.profile_plugins.find(entry => entry.kind === 'ternilo.extension.package'),
      { ...configuredExtension, enabled: false },
      'disabling an inherited mount preserves its explicit disabled override and settings',
    )
    assert.equal(disabledSession.preset_plugins.some(entry => entry.kind === 'ternilo.extension.package'), false)
    const disabledProfile = await api(page, `/sessions/${encodeURIComponent(sessionId)}/plugins`)
    assert.deepEqual(
      disabledProfile.plugins.find(entry => entry.kind === 'ternilo.extension.package'),
      { ...configuredExtension, enabled: false },
    )
    await settings.getByRole('button', { name: '关闭设置' }).click()

    await input.fill('确认禁用后工具目录消失')
    await page.getByRole('button', { name: '发送' }).click()
    await page.getByText('The signed fixture tool is unavailable.', { exact: true }).waitFor()
    assert.equal(toolNames(model.requests.at(-1)).includes('signed_fixture'), false)

    await page.getByRole('button', { name: '设置' }).click()
    settings = page.getByRole('dialog', { name: '设置' })
    await settings.getByRole('button', { name: '插件', exact: true }).click()
    await settings.getByRole('tab', { name: '扩展包' }).click()
    const removable = settings.locator('[data-extension-package="dev.ternilo.browser-fixture@1.0.0"]')
    await removable.getByRole('button', { name: '卸载扩展包 dev.ternilo.browser-fixture' }).click()
    const uninstall = page.getByRole('dialog', { name: '卸载这个扩展包？' })
    const pluginDelete = page.waitForResponse(response => response.request().method() === 'DELETE' && response.url().includes('/api/v1/extensions/'))
    await uninstall.getByRole('button', { name: '卸载', exact: true }).click()
    const pluginDeleteResponse = await pluginDelete
    assert.equal(pluginDeleteResponse.status(), 204, pluginDeleteResponse.url())
    await removable.waitFor({ state: 'detached' })
    const remainingInventory = await api(page, '/extensions')
    assert.equal(remainingInventory.extensions.length, 0, JSON.stringify(remainingInventory.extensions))
    assert.equal((await api(page, `/sessions/${encodeURIComponent(sessionId)}/plugins`)).plugins.some(entry => entry.kind === 'ternilo.extension.package'), false)
    assert.deepEqual(await readdir(artifactDirectory), [])

    await settings.locator('#plugin-bundle-file').setInputFiles(path.join(fixtureDirectory, 'bundle.json'))
    const reinstallReview = page.getByRole('dialog', { name: '审阅并安装扩展包' })
    await reinstallReview.getByRole('button', { name: '确认安装' }).click()
    await reinstallReview.waitFor({ state: 'detached' })
    const publisherMounted = settings.locator('[data-extension-package="dev.ternilo.browser-fixture@1.0.0"]')
    await publisherMounted.locator('[data-extension-mount]').click()
    await page.waitForFunction(() => document.querySelector('[data-extension-mount]')?.getAttribute('aria-checked') === 'true')
    await settings.getByRole('button', { name: '撤销发布者 ternilo-browser-fixture' }).click()
    const publisherRevoke = page.getByRole('dialog', { name: '撤销这个发布者？' })
    await publisherRevoke.getByRole('button', { name: '撤销', exact: true }).click()
    await publisherRevoke.waitFor({ state: 'detached' })
    await publisherMounted.getByText('已撤销', { exact: true }).waitFor()
    await page.waitForFunction(() => document.querySelector('[data-extension-mount]')?.getAttribute('aria-checked') === 'false')
    const revokedState = await api(page, '/state')
    assert.equal(revokedState.sessions.find(item => item.identity.session_id === sessionId).profile_plugins.some(entry => entry.kind === 'ternilo.extension.package'), false)
    assert.equal((await api(page, `/sessions/${encodeURIComponent(sessionId)}/plugins`)).plugins.some(entry => entry.kind === 'ternilo.extension.package'), false)
    await settings.getByRole('button', { name: '关闭设置' }).click()
    await input.fill('确认发布者撤销后会话继续')
    await page.getByRole('button', { name: '发送' }).click()
    await page.getByText('The signed fixture tool is unavailable.', { exact: true }).last().waitFor()
    await page.reload({ waitUntil: 'domcontentloaded' })
    await page.setViewportSize({ width: 390, height: 844 })
    const historicalTool = page.locator('[data-tool-call-id="wasm-call"]')
    await historicalTool.waitFor({ state: 'attached' })
    if (!await historicalTool.isVisible()) {
      await historicalTool.locator('xpath=ancestor::*[@data-chat-turn][1]').locator('[data-turn-process]').click()
    }
    await historicalTool.waitFor()
    assert.equal(await historicalTool.getAttribute('data-tool-contribution'), 'builtin.declarative')
    assert.match(await historicalTool.textContent(), /Signed fixture report.*Subject: browser/)
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= document.documentElement.clientWidth), true)
    await page.getByRole('button', { name: '打开侧边栏' }).click()
    const mobileSidebar = page.getByRole('complementary', { name: '会话侧边栏' })
    await mobileSidebar.getByRole('button', { name: '设置' }).click()
    settings = page.getByRole('dialog', { name: '设置' })
    await settings.getByRole('button', { name: '通用', exact: true }).click()
    await selectChoice(settings.getByLabel('语言'), 'en')
    settings = page.getByRole('dialog', { name: 'Settings' })
    await settings.getByRole('button', { name: 'Close settings' }).click()
    await page.locator('[data-conversation-view="chat"]:not([hidden])').waitFor()
    await page.getByRole('textbox', { name: 'Enter task' }).waitFor()
    assert.match(await historicalTool.textContent(), /Signed fixture report/)
    const sawApiRequest = (method, path) => apiRequests.some(request => request.method === method && request.path === path)
    assert.equal(sawApiRequest('GET', '/api/v1/extensions'), true, JSON.stringify(apiRequests))
    assert.equal(sawApiRequest('POST', '/api/v1/extensions/publishers'), true, JSON.stringify(apiRequests))
    assert.equal(sawApiRequest('POST', '/api/v1/extensions'), true, JSON.stringify(apiRequests))
    assert.equal(
      sawApiRequest('PUT', '/api/v1/extensions/dev.ternilo.browser-fixture/1.0.0'),
      true,
      JSON.stringify(apiRequests),
    )
    assert.equal(
      sawApiRequest('DELETE', '/api/v1/extensions/dev.ternilo.browser-fixture/1.0.0'),
      true,
      JSON.stringify(apiRequests),
    )
    assert.equal(
      sawApiRequest('POST', '/api/v1/extensions/publishers/ternilo-browser-fixture/revoke'),
      true,
      JSON.stringify(apiRequests),
    )
    assert.equal(
      apiRequests.some(request => request.path === '/api/v1/plugins'),
      false,
      JSON.stringify(apiRequests),
    )
    assert.deepEqual(consoleErrors, [])
    assert.deepEqual(pageErrors, [])
  } catch (cause) {
    const page = browser?.contexts()[0]?.pages()[0]
    const alerts = page ? await page.getByRole('alert').allTextContents() : []
    throw new Error(`${cause instanceof Error ? cause.message : String(cause)}\nalerts: ${JSON.stringify(alerts)}\nserver: ${app.diagnostics()}`, { cause })
  } finally {
    await browser?.close()
    await stop(app.child)
    await model.close()
    await rm(dataDirectory, { recursive: true, force: true })
  }
})
