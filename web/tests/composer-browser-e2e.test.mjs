import { selectChoice } from './browser-select-fixture.mjs'
import assert from 'node:assert/strict'
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises'
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

function conversationRequests(received) {
  return received.filter(request => !String(request.instructions ?? '').startsWith('You name software-agent conversations.'))
}

async function startModelFixture() {
  const received = []
  const server = createServer((request, response) => {
    const chunks = []
    request.on('data', chunk => chunks.push(chunk))
    request.on('end', () => {
      received.push(JSON.parse(Buffer.concat(chunks).toString('utf8')))
      response.writeHead(200, { 'content-type': 'text/event-stream', 'x-request-id': 'composer-context' })
      response.end([
        sse({ type: 'response.output_text.delta', output_index: 0, content_index: 0, delta: '上下文计量完成。' }),
        sse({
          type: 'response.completed',
          response: {
            status: 'completed',
            output: [{ type: 'message', role: 'assistant', content: [{ type: 'output_text', text: '上下文计量完成。' }] }],
            usage: {
              input_tokens: 80_000, output_tokens: 25_000,
              input_tokens_details: { cached_tokens: 40_000 }, output_tokens_details: { reasoning_tokens: 3_000 },
            },
          },
        }),
      ].join(''))
    })
  })
  await new Promise((resolve, reject) => {
    server.once('error', reject)
    server.listen(0, '127.0.0.1', resolve)
  })
  const address = server.address()
  return {
    baseUrl: `http://127.0.0.1:${address.port}/v1`, received,
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
  await dialog.getByRole('list', { name: `目录 ${workspace}`, exact: true }).waitFor()
  await dialog.getByRole('button', { name: '打开所选文件夹' }).click()
  await dialog.waitFor({ state: 'detached' })
  await page.locator('[data-sidebar-new-session]').click()
  await page.getByRole('textbox', { name: '输入任务' }).waitFor()
}

async function request(page, pathValue, method, bodyValue) {
  return page.evaluate(async ({ pathValue, method, bodyValue }) => {
    const token = window.__TERNILO_BOOT__?.apiToken
    const response = await fetch(`/api/v1${pathValue}`, {
      method,
      headers: {
        'content-type': 'application/json',
        ...(token ? { authorization: `Bearer ${token}` } : {}),
      },
      body: bodyValue === undefined ? undefined : JSON.stringify(bodyValue),
    })
    if (!response.ok) throw new Error(`${method} ${pathValue}: ${response.status} ${await response.text()}`)
    return response.status === 204 ? null : response.json()
  }, { pathValue, method, bodyValue })
}

async function configureFixture(page, baseUrl) {
  await request(page, '/credentials', 'POST', { name: 'TERNILO_PROVIDER_COMPOSER_API_KEY', value: 'composer-key' })
  await request(page, '/providers', 'POST', {
    id: 'composer-fixture', display_name: 'Composer Fixture', base_url: baseUrl,
    protocol: 'openai-responses', api_key_ref: 'TERNILO_PROVIDER_COMPOSER_API_KEY',
    defaults: {
      context_window: 128000, max_output_tokens: 8192,
      reasoning: { default_effort: 'high', efforts: { high: 'high' } },
    },
    models: [{
      id: 'context-model', display_name: 'Context Model', settings: { mode: 'inherit' },
    }],
    timeout_ms: 120000, max_attempts: 1, retry_base_delay_ms: 50,
  })
  const sessionId = await page.evaluate(() => localStorage.getItem('ternilo.current-session'))
  await request(page, `/sessions/${encodeURIComponent(sessionId)}`, 'PATCH', {
    model: { provider: 'named_provider', provider_id: 'composer-fixture', model: 'context-model', reasoning_effort: 'high' },
  })
  await page.reload({ waitUntil: 'domcontentloaded' })
  await page.getByRole('button', { name: /Context Model/ }).waitFor()
}

async function assertNoHorizontalOverflow(page, label) {
  const dimensions = await page.evaluate(() => ({ viewport: innerWidth, document: document.documentElement.scrollWidth, body: document.body.scrollWidth }))
  assert.equal(dimensions.document <= dimensions.viewport, true, `${label}: ${JSON.stringify(dimensions)}`)
  assert.equal(dimensions.body <= dimensions.viewport, true, `${label}: ${JSON.stringify(dimensions)}`)
}

test('Composer uses real local catalogs, takeovers, projections, attachments, context and mobile layouts', { timeout: 120_000 }, async () => {
  const dataDirectory = await mkdtemp(path.join(tmpdir(), 'ternilo-composer-'))
  const workspace = path.join(dataDirectory, 'workspace')
  const skillDirectory = path.join(workspace, '.agents', 'skills', 'release-check')
  const docsDirectory = path.join(workspace, 'docs')
  await mkdir(skillDirectory, { recursive: true })
  await mkdir(docsDirectory, { recursive: true })
  await writeFile(path.join(skillDirectory, 'SKILL.md'), `---\nname: release-check\ndescription: 检查发布前条件\nwhen-to-use: 准备发布时\nuser-invocable: true\n---\n\n# Release check\n\nInspect the workspace before release.\n`)
  await writeFile(path.join(docsDirectory, 'guide.md'), '# Composer reference fixture\n')
  const model = await startModelFixture()
  const ternilo = startTernilo(dataDirectory)
  let browser
  try {
    const origin = await ternilo.origin
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE })
    const page = await browser.newPage({ locale: 'zh-CN', viewport: { width: 1440, height: 900 } })
    const pageErrors = []
    page.on('pageerror', error => pageErrors.push(error.message))
    await page.goto(origin, { waitUntil: 'networkidle' })
    await chooseWorkspace(page, workspace)
    let input = page.getByRole('textbox', { name: '输入任务' })

    await input.fill('/go')
    await page.locator('[data-composer-menu]').getByText('/goal', { exact: true }).waitFor()
    await input.press('Enter')
    assert.equal(await input.inputValue(), '/goal ')

    await input.fill('/skill ')
    const skill = page.locator('[data-composer-menu] [role="option"]').filter({ hasText: 'release-check' })
    await skill.waitFor()
    assert.match(await skill.textContent(), /检查发布前条件/)
    assert.match(await skill.textContent(), /filesystem/)
    await input.press('Enter')
    assert.equal(await input.inputValue(), '.skill release-check ')

    await configureFixture(page, model.baseUrl)
    input = page.getByRole('textbox', { name: '输入任务' })

    const selectionDraft = '先看 @do 再继续'
    await input.fill(selectionDraft)
    const selectionCaret = selectionDraft.indexOf(' 再继续')
    await input.evaluate((element, caret) => {
      element.focus()
      element.setSelectionRange(caret, caret)
      element.dispatchEvent(new Event('select', { bubbles: true }))
      document.dispatchEvent(new Event('selectionchange', { bubbles: true }))
      element.dispatchEvent(new KeyboardEvent('keyup', { key: 'ArrowLeft', bubbles: true }))
    }, selectionCaret)
    await page.getByRole('button', { name: '进入文件夹 docs' }).waitFor()
    await page.getByRole('button', { name: '进入文件夹 docs' }).click()
    assert.equal(await input.inputValue(), '先看 @docs/ 再继续')
    await page.locator('[data-composer-menu] [role="option"]').filter({ hasText: 'guide.md' }).waitFor()
    await input.press('Enter')
    assert.equal(await input.inputValue(), '先看  再继续')
    let selectionReference = page.getByLabel('待发送引用')
    await selectionReference.getByText('guide.md').waitFor()
    await page.waitForFunction(() => document.querySelector('[data-composer-input]')?.selectionStart === 3)
    assert.deepEqual(await input.evaluate(element => [element.selectionStart, element.selectionEnd]), [3, 3])

    await input.press('Control+z')
    assert.equal(await input.inputValue(), '先看 @docs/ 再继续')
    assert.equal(await selectionReference.count(), 0)
    await page.waitForFunction(() => document.querySelector('[data-composer-input]')?.selectionStart === 9)
    assert.deepEqual(await input.evaluate(element => [element.selectionStart, element.selectionEnd]), [9, 9])
    await input.press('Control+Shift+z')
    assert.equal(await input.inputValue(), '先看  再继续')
    selectionReference = page.getByLabel('待发送引用')
    await selectionReference.getByText('guide.md').waitFor()
    await page.getByRole('button', { name: '移除引用 guide.md' }).click()
    await input.fill('')

    await input.fill('@do')
    await page.getByRole('button', { name: '进入文件夹 docs' }).waitFor()
    await page.getByRole('button', { name: '进入文件夹 docs' }).click()
    assert.equal(await input.inputValue(), '@docs/')
    await page.locator('[data-composer-menu] [role="option"]').filter({ hasText: 'guide.md' }).waitFor()
    await input.press('Enter')
    const references = page.getByLabel('待发送引用')
    await references.waitFor()
    assert.match(await references.textContent(), /guide\.md/)
    await input.fill('恢复失败草稿')

    await page.evaluate(() => {
      const transfer = new DataTransfer()
      transfer.items.add(new File(['fixture attachment'], 'fixture.txt', { type: 'text/plain' }))
      document.body.dispatchEvent(new DragEvent('dragenter', { bubbles: true, cancelable: true, dataTransfer: transfer }))
    })
    await page.locator('[data-drop-overlay]').waitFor()
    await page.evaluate(() => {
      const transfer = new DataTransfer()
      transfer.items.add(new File(['fixture attachment'], 'fixture.txt', { type: 'text/plain' }))
      document.body.dispatchEvent(new DragEvent('drop', { bubbles: true, cancelable: true, dataTransfer: transfer }))
    })
    await page.getByLabel('待发送附件').getByText('fixture.txt').waitFor()

    let rejectNextQueuePost = true
    let rejectedSubmission
    await page.route('**/api/v1/sessions/*/queue', async route => {
      if (route.request().method() !== 'POST' || !rejectNextQueuePost) return route.continue()
      rejectNextQueuePost = false
      rejectedSubmission = route.request().postDataJSON()
      await route.fulfill({ status: 503, contentType: 'application/json', body: JSON.stringify({ error: { code: 'fixture_rejected', message: '测试拒绝' } }) })
    })
    await page.getByRole('button', { name: '发送' }).click()
    await page.getByLabel('通知', { exact: true }).getByText('测试拒绝', { exact: true }).waitFor()
    await page.waitForFunction(() => document.querySelector('[data-composer-input]')?.value === '恢复失败草稿')
    assert.equal(await input.inputValue(), '恢复失败草稿')
    assert.deepEqual(rejectedSubmission.references, [{ kind: 'file', path: 'docs/guide.md', file_kind: 'file' }])
    assert.equal(rejectedSubmission.content.input, '恢复失败草稿')
    assert.equal(await references.isVisible(), true)
    assert.equal(await page.getByLabel('待发送附件').getByText('fixture.txt').isVisible(), true)
    await page.unroute('**/api/v1/sessions/*/queue')

    await input.evaluate(element => {
      const transfer = new DataTransfer()
      const bytes = Uint8Array.from(atob('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII='), value => value.charCodeAt(0))
      transfer.items.add(new File([bytes], 'pasted.png', { type: 'image/png' }))
      element.dispatchEvent(new ClipboardEvent('paste', {
        bubbles: true, cancelable: true, clipboardData: transfer,
      }))
    })
    await page.getByLabel('待发送附件').getByText('pasted.webp').waitFor()

    await input.evaluate(async element => {
      const canvas = document.createElement('canvas')
      canvas.width = 3000
      canvas.height = 2000
      const context = canvas.getContext('2d')
      const pixels = context.createImageData(canvas.width, canvas.height)
      let state = 0x12345678
      for (let index = 0; index < pixels.data.length; index += 4) {
        state = (Math.imul(state, 1664525) + 1013904223) >>> 0
        pixels.data[index] = state & 255
        pixels.data[index + 1] = (state >>> 8) & 255
        pixels.data[index + 2] = (state >>> 16) & 255
        pixels.data[index + 3] = 255
      }
      context.putImageData(pixels, 0, 0)
      const blob = await new Promise(resolve => canvas.toBlob(resolve, 'image/jpeg', 1))
      const transfer = new DataTransfer()
      transfer.items.add(new File([blob], 'phone.jpg', { type: 'image/jpeg' }))
      element.dispatchEvent(new ClipboardEvent('paste', {
        bubbles: true, cancelable: true, clipboardData: transfer,
      }))
    })
    const phonePreview = page.getByRole('button', { name: '预览 phone.jpg' })
    await phonePreview.waitFor()
    const normalized = await phonePreview.locator('img').evaluate(image => ({
      width: image.naturalWidth,
      height: image.naturalHeight,
      encodedLength: image.src.length,
      mediaType: image.src.slice(5, image.src.indexOf(';')),
    }))
    assert.equal(normalized.width * normalized.height <= 2048 * 2048 + 4096, true, JSON.stringify(normalized))
    assert.equal(normalized.encodedLength <= 4 * 1024 * 1024 * 4 / 3 + 64, true, JSON.stringify(normalized))
    assert.equal(normalized.mediaType, 'image/jpeg')

    await page.getByRole('button', { name: '移除 fixture.txt' }).click()
    await page.getByRole('button', { name: '移除 pasted.webp' }).click()
    await page.getByRole('button', { name: '移除 phone.jpg' }).click()
    await input.fill('')
    await page.getByRole('button', { name: '发送' }).click()
    await page.getByText('上下文计量完成。', { exact: true }).waitFor()
    assert.match(JSON.stringify(conversationRequests(model.received)[0]), /Composer reference fixture/)
    const durableReference = page.locator('[data-role="user"] [data-submission-references]').filter({ hasText: 'guide.md' })
    await durableReference.waitFor()
    const provenance = page.locator('[data-context-injection]').filter({ hasText: 'reference:file' })
    await page.locator('[data-turn-process]').last().click()
    await provenance.waitFor()
    await provenance.locator('[data-disclosure-row]').click()
    assert.match(await provenance.textContent(), /docs\/guide\.md/)
    await page.reload({ waitUntil: 'domcontentloaded' })
    input = page.getByRole('textbox', { name: '输入任务' })
    await input.waitFor()
    await page.locator('[data-role="user"] [data-submission-references]').filter({ hasText: 'guide.md' }).waitFor()

    const sourceSessionId = await page.evaluate(() => localStorage.getItem('ternilo.current-session'))
    await request(page, `/sessions/${encodeURIComponent(sourceSessionId)}`, 'PATCH', { title: 'Reference source' })
    const state = await request(page, '/state', 'GET')
    const source = state.sessions.find(session => session.identity.session_id === sourceSessionId)
    const target = await request(page, '/sessions', 'POST', {
      workspace_id: source.workspace_id,
      agent_preset: 'standard',
      permissions: 'workspace_write',
    })
    await request(page, `/sessions/${encodeURIComponent(target.identity.session_id)}`, 'PATCH', {
      model: { provider: 'named_provider', provider_id: 'composer-fixture', model: 'context-model', reasoning_effort: 'high' },
    })
    await page.evaluate(sessionId => localStorage.setItem('ternilo.current-session', sessionId), target.identity.session_id)
    await page.reload({ waitUntil: 'domcontentloaded' })
    input = page.getByRole('textbox', { name: '输入任务' })
    await input.waitFor()
    await input.fill('@reference')
    await page.locator('[data-composer-menu] [role="option"]').filter({ hasText: 'Reference source' }).waitFor()
    await input.press('Enter')
    assert.equal(await input.inputValue(), '')
    await page.getByLabel('待发送引用').getByText('Reference source').waitFor()
    let sessionReferenceSubmission
    await page.route('**/api/v1/sessions/*/queue', async route => {
      if (route.request().method() === 'POST' && sessionReferenceSubmission === undefined) {
        sessionReferenceSubmission = route.request().postDataJSON()
      }
      await route.continue()
    })
    const sessionAnswersBefore = await page.getByText('上下文计量完成。', { exact: true }).count()
    await page.getByRole('button', { name: '发送' }).click()
    await page.waitForFunction(expected => [...document.querySelectorAll('*')]
      .filter(element => element.children.length === 0 && element.textContent === '上下文计量完成。').length > expected, sessionAnswersBefore)
    await page.unroute('**/api/v1/sessions/*/queue')
    assert.deepEqual(sessionReferenceSubmission.references, [{
      kind: 'session', session_id: sourceSessionId, label: 'Reference source',
    }])
    assert.equal(sessionReferenceSubmission.content.input, '')
    assert.match(JSON.stringify(conversationRequests(model.received)[1]), /Reference source/)
    assert.match(JSON.stringify(conversationRequests(model.received)[1]), new RegExp(sourceSessionId))
    const durableSessionReference = page.locator('[data-role="user"] [data-submission-references]').filter({ hasText: 'Reference source' })
    await durableSessionReference.waitFor()
    const sessionProvenance = page.locator('[data-context-injection]').filter({ hasText: 'reference:session' })
    await page.locator('[data-turn-process]').last().click()
    await sessionProvenance.waitFor()
    await page.reload({ waitUntil: 'domcontentloaded' })
    input = page.getByRole('textbox', { name: '输入任务' })
    await input.waitFor()
    await page.locator('[data-role="user"] [data-submission-references]').filter({ hasText: 'Reference source' }).waitFor()
    await page.getByRole('tab', { name: '轨迹', exact: true }).click()
    const referenceTrajectory = page.locator('[data-trajectory-record]').filter({ hasText: 'reference:session' })
    await referenceTrajectory.waitFor()
    assert.match(await referenceTrajectory.textContent(), /ternilo\.reference\.session\.v1/)
    assert.match(await referenceTrajectory.textContent(), /Reference source/)
    await page.getByRole('tab', { name: '对话' }).click()

    await input.fill('/goal 交付 Composer')
    await page.getByRole('button', { name: '发送' }).click()
    await page.locator('[data-projection-dock]').getByText('交付 Composer', { exact: true }).waitFor()
    await input.fill('/todo 完成交互;运行测试')
    await page.getByRole('button', { name: '发送' }).click()
    const todo = page.locator('[data-projection-dock]').getByRole('button', { name: /0\/2/ })
    await todo.waitFor()
    await todo.click()
    await page.locator('[data-projection-dock]').getByText('完成交互', { exact: true }).waitFor()

    await input.fill('/shell! echo needs-approval')
    await page.getByRole('button', { name: '发送' }).click()
    const approval = page.locator('[data-tool-approval]')
    await approval.waitFor()
    assert.equal(await page.locator('[data-composer-input]').count(), 0)
    await approval.getByRole('button', { name: '查看调用详情' }).click()
    const approvalDetails = page.locator('.details-panel[data-details-state="loading"]')
    await approvalDetails.waitFor()
    assert.equal(await approvalDetails.locator('h2').textContent(), 'shell')
    assert.match(await approvalDetails.textContent(), /needs-approval/)
    await approvalDetails.getByRole('button', { name: '关闭详情' }).click()
    await approval.getByRole('button', { name: '拒绝' }).click()
    await approval.waitFor({ state: 'detached' })
    input = page.getByRole('textbox', { name: '输入任务' })
    await input.waitFor()

    await page.getByRole('button', { name: '设置' }).click()
    const settings = page.getByRole('dialog', { name: '设置' })
    await selectChoice(settings.getByLabel('繁忙时 Enter 键行为'), 'steer')
    assert.equal(await page.evaluate(() => localStorage.getItem('ternilo.composer.busy-enter')), 'steer')
    await settings.getByRole('button', { name: '关闭设置' }).click()

    const providerCallsBeforeDirect = model.received.length
    for (const command of [
      '/read .agents/skills/release-check/SKILL.md',
      '/goal Provider 下也直接执行',
      '/compact',
    ]) {
      const completed = await page.locator('[data-chat-event="command"][data-state="complete"]').count()
      await input.fill(command)
      const send = page.getByRole('button', { name: '发送' })
      assert.equal(await send.isEnabled(), true, `${command} must not require model readiness`)
      await send.click()
      await page.waitForFunction(expected => (
        document.querySelectorAll('[data-chat-event="command"][data-state="complete"]').length > expected
        && !document.querySelector('[data-composer-card][data-busy]')
      ), completed)
      assert.equal(model.received.length, providerCallsBeforeDirect, `${command} must bypass the configured Provider`)
    }
    const answersBefore = await page.getByText('上下文计量完成。', { exact: true }).count()
    await input.fill('验证真实上下文计量')
    await page.getByRole('button', { name: '发送' }).click()
    await page.waitForFunction(expected => [...document.querySelectorAll('*')]
      .filter(element => element.children.length === 0 && element.textContent === '上下文计量完成。').length > expected, answersBefore)
    const context = page.getByRole('button', { name: /上下文已用 82%/ })
    await context.waitFor()
    await context.click()
    assert.equal(await page.getByRole('dialog', { name: '上下文已用' }).getByText(/\/compact/).isVisible(), true)
    await context.click()

    for (const viewport of [
      { width: 390, height: 844, label: 'portrait' },
      { width: 390, height: 430, label: 'short portrait' },
      { width: 844, height: 390, label: 'landscape' },
    ]) {
      await page.setViewportSize(viewport)
      await input.fill('')
      await input.fill('/')
      await page.locator('[data-composer-menu]').waitFor()
      await assertNoHorizontalOverflow(page, viewport.label)
      const menu = await page.locator('[data-composer-menu]').boundingBox()
      assert.equal(menu.x >= -1 && menu.x + menu.width <= viewport.width + 1, true, `${viewport.label}: ${JSON.stringify(menu)}`)
      await input.press('Escape')
    }

    await page.setViewportSize({ width: 390, height: 844 })
    await input.fill('@do')
    const mobileDrill = page.getByRole('button', { name: '进入文件夹 docs' })
    await mobileDrill.waitFor()
    await assertNoHorizontalOverflow(page, 'reference menu portrait')
    await mobileDrill.click()
    await page.locator('[data-composer-menu] [role="option"]').filter({ hasText: 'guide.md' }).waitFor()
    await input.press('Enter')
    await page.getByLabel('待发送引用').getByText('guide.md').waitFor()
    await page.getByRole('button', { name: '移除引用 guide.md' }).click()

    await page.getByRole('button', { name: '打开侧边栏' }).click()
    await page.getByRole('button', { name: '设置' }).click()
    const mobileSettings = page.getByRole('dialog', { name: '设置' })
    await selectChoice(mobileSettings.getByLabel('语言'), 'en')
    await page.getByRole('button', { name: 'Close settings' }).click()
    await page.locator('[data-mobile-sidebar-close]').click()
    input = page.getByRole('textbox', { name: 'Enter task' })
    await input.fill('')
    await input.click()
    await input.press('/')
    const englishMenu = page.locator('[data-composer-menu]')
    await englishMenu.waitFor()
    assert.equal(await englishMenu.getAttribute('aria-label'), 'Commands')
    await assertNoHorizontalOverflow(page, 'English mobile')

    assert.equal(conversationRequests(model.received).length, 3)
    assert.deepEqual(pageErrors, [])
    assert.equal(ternilo.diagnostics().includes('panicked'), false, ternilo.diagnostics())
  } finally {
    await browser?.close()
    await stopProcess(ternilo.child)
    await model.close()
    await rm(dataDirectory, { recursive: true, force: true })
  }
})
