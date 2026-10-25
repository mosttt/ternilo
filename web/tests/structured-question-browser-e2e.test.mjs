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
const binary = process.env.TERNILO_E2E_NODE_BINARY ?? path.join(repository, 'target', 'debug', 'ternilo')
const LONG_QUESTION = '先选择执行策略，并确认该策略能够覆盖依赖核对、实现、验证和发布说明等完整交付要求'
const LONG_OPTION = '先完成依赖梳理、接口核对和真实浏览器验证，再开始跨端发布流程'
const MANY_OPTIONS = ['测试', '核对依赖', '检查日志', '保留镜像', '复核存储', '部署检查', '准备回滚', '说明影响', '文档']
const LONG_DESCRIPTION = '这个说明需要在窄屏上完整换行，并让选项按钮按内容自然增高，不能覆盖后续选项。'
const BEFORE_QUESTION = '提问前的建议：先比较两种实现，再选择适合的方案。'

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

function sse(payload) {
  return `data: ${JSON.stringify(payload)}\n\n`
}

function completed(output) {
  return sse({
    type: 'response.completed',
    response: {
      status: 'completed',
      output,
      usage: {
        input_tokens: 50,
        output_tokens: 12,
        input_tokens_details: { cached_tokens: 0 },
        output_tokens_details: { reasoning_tokens: 0 },
      },
    },
  })
}

async function startModelFixture() {
  let ordinaryRequests = 0
  let resumedRequest
  let resume
  const resumed = new Promise(resolve => { resume = resolve })
  const server = createServer((incoming, response) => {
    if (incoming.method !== 'POST' || incoming.url !== '/v1/responses') {
      response.writeHead(404).end()
      return
    }
    const body = []
    incoming.on('data', chunk => body.push(chunk))
    incoming.on('end', () => {
      const parsed = JSON.parse(Buffer.concat(body).toString('utf8'))
      response.writeHead(200, { 'content-type': 'text/event-stream', 'cache-control': 'no-cache' })
      if (typeof parsed.instructions === 'string' && parsed.instructions.includes('You name software-agent conversations')) {
        response.end(completed([{ type: 'message', role: 'assistant', content: [{ type: 'output_text', text: '结构化问题验收' }] }]))
        return
      }
      ordinaryRequests += 1
      if (ordinaryRequests === 1) {
        const argumentsValue = JSON.stringify({ questions: [
          {
            id: 'strategy',
            header: '选择策略',
            question: LONG_QUESTION,
            detail: '请根据 **交付目标** 作出选择。',
            options: [
              { label: '稳妥 (Recommended)', description: '先验证再修改。' },
              { label: '快速', description: '立即进入实现。' },
              { label: LONG_OPTION, description: LONG_DESCRIPTION },
            ],
          },
          {
            id: 'surfaces',
            question: '选择要覆盖的表面',
            options: MANY_OPTIONS.map(label => ({ label, description: `${LONG_DESCRIPTION} ${LONG_DESCRIPTION}` })),
            multi_select: true,
          },
        ] })
        const call = { type: 'function_call', call_id: 'ask-structured', name: 'ask_user', arguments: argumentsValue }
        response.end([
          sse({ type: 'response.output_text.delta', output_index: 0, content_index: 0, delta: BEFORE_QUESTION }),
          sse({ type: 'response.output_item.added', output_index: 1, item: { ...call, arguments: '' } }),
          sse({ type: 'response.function_call_arguments.done', output_index: 1, arguments: argumentsValue }),
          completed([{ type: 'message', role: 'assistant', content: [{ type: 'output_text', text: BEFORE_QUESTION }] }, call]),
        ].join(''))
        return
      }
      resumedRequest = parsed
      resume(parsed)
      const text = '结构化回答已收到。'
      response.end([
        sse({ type: 'response.output_text.delta', output_index: 0, content_index: 0, delta: text }),
        completed([{ type: 'message', role: 'assistant', content: [{ type: 'output_text', text }] }]),
      ].join(''))
    })
  })
  await new Promise((resolve, reject) => {
    server.once('error', reject)
    server.listen(0, '127.0.0.1', resolve)
  })
  const address = server.address()
  return {
    baseUrl: `http://127.0.0.1:${address.port}/v1`,
    resumed: () => resumedRequest ? Promise.resolve(resumedRequest) : resumed,
    close: () => new Promise((resolve, reject) => server.close(error => error ? reject(error) : resolve())),
  }
}

async function chooseWorkspace(page, workspace) {
  await page.getByRole('button', { name: '选择工作文件夹' }).first().click()
  const dialog = page.getByRole('dialog', { name: '选择工作文件夹' })
  await dialog.getByRole('button', { name: '编辑文件夹路径' }).click()
  const input = dialog.getByRole('textbox', { name: '编辑文件夹路径' })
  await input.fill(workspace)
  await input.press('Enter')
  await dialog.getByRole('list', { name: `目录 ${workspace}`, exact: true }).waitFor()
  await dialog.getByRole('button', { name: '打开所选文件夹' }).click()
  await dialog.waitFor({ state: 'detached' })
  await page.locator('[data-sidebar-new-session]').click()
  await page.waitForFunction(() => Boolean(localStorage.getItem('ternilo.current-session')))
  await page.getByRole('textbox', { name: '输入任务' }).waitFor()
}

async function configureFixture(page, baseUrl) {
  await page.evaluate(async baseUrlValue => {
    const headers = { 'content-type': 'application/json' }
    const token = window.__TERNILO_BOOT__?.apiToken
    if (token) headers.authorization = `Bearer ${token}`
    const request = async (pathname, method, body) => {
      const response = await fetch(`/api/v1${pathname}`, {
        method,
        headers,
        body: body === undefined ? undefined : JSON.stringify(body),
      })
      if (!response.ok) throw new Error(`${method} ${pathname}: ${response.status} ${await response.text()}`)
      return response.status === 204 ? null : response.json()
    }
    await request('/credentials', 'POST', { name: 'TERNILO_PROVIDER_STRUCTURED_API_KEY', value: 'fixture-key' })
    await request('/providers', 'POST', {
      id: 'structured',
      display_name: 'Structured fixture',
      base_url: baseUrlValue,
      protocol: 'openai-responses',
      api_key_ref: 'TERNILO_PROVIDER_STRUCTURED_API_KEY',
      defaults: { context_window: 128000, max_output_tokens: 8192 },
      models: [{ id: 'fixture-model', display_name: 'Structured Model', settings: { mode: 'inherit' } }],
      timeout_ms: 120000,
      max_attempts: 1,
      retry_base_delay_ms: 50,
    })
    const sessionId = localStorage.getItem('ternilo.current-session')
    await request(`/sessions/${encodeURIComponent(sessionId)}`, 'PATCH', {
      model: { provider: 'named_provider', provider_id: 'structured', model: 'fixture-model' },
    })
  }, baseUrl)
  await page.reload({ waitUntil: 'domcontentloaded' })
  await page.getByRole('button', { name: /Structured Model/ }).waitFor()
}

function findToolOutput(value) {
  if (Array.isArray(value)) {
    for (const item of value) {
      const found = findToolOutput(item)
      if (found) return found
    }
    return undefined
  }
  if (value && typeof value === 'object') {
    if (value.type === 'function_call_output' && typeof value.output === 'string') return value.output
    for (const item of Object.values(value)) {
      const found = findToolOutput(item)
      if (found) return found
    }
  }
  return undefined
}

async function visibleQuestion(page, question) {
  const row = page.locator('[data-question-lifecycle][data-state="answered"]').filter({ hasText: question }).first()
  await row.waitFor({ state: 'visible' })
  return row
}

async function assertLongQuestionMobileLayout(page, takeover) {
  await page.setViewportSize({ width: 390, height: 844 })
  const option = takeover.locator('[data-question-option]').filter({ hasText: LONG_OPTION })
  await option.waitFor()
  const metrics = await takeover.evaluate((card, longOption) => {
    const body = card.querySelector('[data-question-body]')
    const heading = card.querySelector('[data-question-heading]')
    const optionElement = [...card.querySelectorAll('[data-question-option]')]
      .find(candidate => candidate.textContent?.includes(longOption))
    const marker = optionElement?.querySelector('[data-question-option-mark]')
    const copy = optionElement?.querySelector('[data-question-option-copy]')
    const label = optionElement?.querySelector('[data-question-option-label]')
    const description = optionElement?.querySelector('small')
    if (!body || !heading || !optionElement || !marker || !copy || !label || !description) throw new Error('long question layout nodes missing')
    const optionRect = optionElement.getBoundingClientRect()
    const labelRect = label.getBoundingClientRect()
    const descriptionRect = description.getBoundingClientRect()
    return {
      bodyOverflowY: getComputedStyle(body).overflowY,
      bodyFits: body.scrollHeight <= body.clientHeight + 1,
      optionFits: optionElement.scrollHeight <= optionElement.clientHeight + 1
        && optionElement.scrollWidth <= optionElement.clientWidth + 1,
      copyFits: copy.scrollHeight <= copy.clientHeight + 1 && copy.scrollWidth <= copy.clientWidth + 1,
      labelFits: label.scrollWidth <= label.clientWidth + 1,
      descriptionBelowLabel: descriptionRect.top >= labelRect.bottom - 1,
      optionHeight: optionRect.height,
      headingFits: heading.scrollWidth <= heading.clientWidth + 1,
    }
  }, LONG_OPTION)
  assert.equal(metrics.bodyOverflowY, 'visible', JSON.stringify(metrics))
  assert.equal(metrics.bodyFits, true, JSON.stringify(metrics))
  assert.equal(metrics.optionFits, true, JSON.stringify(metrics))
  assert.equal(metrics.copyFits, true, JSON.stringify(metrics))
  assert.equal(metrics.labelFits, true, JSON.stringify(metrics))
  assert.equal(metrics.descriptionBelowLabel, true, JSON.stringify(metrics))
  assert.equal(metrics.optionHeight > 40, true, JSON.stringify(metrics))
  assert.equal(metrics.headingFits, true, JSON.stringify(metrics))
  assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= document.documentElement.clientWidth), true)
}

async function assertQuestionFitsViewport(page, takeover) {
  const bounds = await takeover.boundingBox()
  const header = await page.locator('#conversation-main > header').boundingBox()
  const footer = await takeover.locator('footer').boundingBox()
  assert.ok(bounds && bounds.y >= header.y + header.height - 1 && bounds.y + bounds.height <= page.viewportSize().height + 1,
    JSON.stringify({ bounds, header, viewport: page.viewportSize() }))
  assert.ok(footer && footer.y >= bounds.y && footer.y + footer.height <= bounds.y + bounds.height)
  assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= document.documentElement.clientWidth), true)
}

async function swipeQuestion(page, scroller, direction) {
  const rect = await scroller.boundingBox()
  assert.ok(rect && rect.height > 40)
  const x = rect.x + rect.width / 2
  const from = rect.y + rect.height * (direction === 'down' ? .8 : .2)
  const to = rect.y + rect.height * (direction === 'down' ? .2 : .8)
  const cdp = await page.context().newCDPSession(page)
  try {
    await cdp.send('Input.dispatchTouchEvent', { type: 'touchStart', touchPoints: [{ x, y: from }] })
    for (let step = 1; step <= 10; step += 1) {
      await cdp.send('Input.dispatchTouchEvent', { type: 'touchMove', touchPoints: [{ x, y: from + (to - from) * step / 10 }] })
      await page.waitForTimeout(20)
    }
    await cdp.send('Input.dispatchTouchEvent', { type: 'touchEnd', touchPoints: [] })
    await page.waitForTimeout(80)
  } finally {
    await cdp.detach()
  }
}

async function swipeToEnd(page, scroller) {
  for (let attempt = 0; attempt < 30; attempt += 1) {
    if (await scroller.evaluate(element => element.scrollHeight - element.clientHeight - element.scrollTop < 2)) return
    await swipeQuestion(page, scroller, 'down')
  }
  assert.fail('Touch scrolling did not reach the last option and custom answer')
}

async function assertManyOptions(page, takeover, artifacts) {
  for (const [locale, width, height, submit, skip] of [
    ['zh', 390, 844, '提交', '跳过'],
    ['en', 844, 390, 'Submit', 'Skip'],
    ['ko', 320, 560, '제출', '건너뛰기'],
  ]) {
    await page.evaluate(locale => localStorage.setItem('ternilo.locale', locale), locale)
    await page.setViewportSize({ width, height })
    await page.reload({ waitUntil: 'domcontentloaded' })
    await takeover.getByRole('radio', { name: '稳妥' }).click()
    const scroller = takeover.locator('[data-question-scroll]')
    await assertQuestionFitsViewport(page, takeover)
    assert.equal(await scroller.evaluate(element => element.scrollTop), 0, 'A new question starts at its heading')
    assert.equal(await scroller.evaluate(element => element.scrollHeight > element.clientHeight), true)
    assert.equal(await takeover.getByRole('button', { name: submit, exact: true }).isVisible(), true)
    assert.equal(await takeover.getByRole('button', { name: skip, exact: true }).isVisible(), true)
    const outer = page.locator('[data-conversation-scroll]')
    const outerTop = await outer.evaluate(element => element.scrollTop)
    await swipeQuestion(page, scroller, 'down')
    assert.ok(await scroller.evaluate(element => element.scrollTop) > 0, 'Touch on option copy scrolls the question')
    await swipeToEnd(page, scroller)
    const last = takeover.getByRole('checkbox', { name: '文档', exact: true })
    const bounds = await last.boundingBox(), viewport = await scroller.boundingBox()
    assert.ok(bounds.y >= viewport.y - 1 && bounds.y + bounds.height <= viewport.y + viewport.height + 1)
    await last.tap()
    const atEnd = await scroller.evaluate(element => element.scrollTop)
    await swipeQuestion(page, scroller, 'up')
    assert.ok(await scroller.evaluate(element => element.scrollTop) < atEnd, 'Options scroll in both directions')
    assert.ok(Math.abs((await outer.evaluate(element => element.scrollTop)) - outerTop) <= 2, 'Question gestures do not pull the chat history')
    await assertQuestionFitsViewport(page, takeover)
    if (artifacts) await page.screenshot({ path: path.join(artifacts, `ask-scroll-${locale}.png`) })
  }
  await page.evaluate(() => localStorage.setItem('ternilo.locale', 'zh'))
  await page.setViewportSize({ width: 390, height: 844 })
  await page.reload({ waitUntil: 'domcontentloaded' })
  await takeover.getByRole('radio', { name: '稳妥' }).click()
}

test('Local Chromium completes structured single and multi-select ask_user', { timeout: 180_000 }, async () => {
  const dataDirectory = await mkdtemp(path.join(tmpdir(), 'ternilo-structured-question-'))
  const workspace = path.join(dataDirectory, 'workspace')
  await mkdir(workspace)
  const model = await startModelFixture()
  const ternilo = startTernilo(dataDirectory)
  const artifacts = process.env.TERNILO_E2E_ARTIFACT_DIR
  if (artifacts) await mkdir(artifacts, { recursive: true })
  let browser
  try {
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
    const page = await browser.newPage({ locale: 'zh-CN', viewport: { width: 1280, height: 820 }, hasTouch: true, serviceWorkers: 'block' })
    const pageErrors = []
    const consoleErrors = [], responseErrors = []
    page.on('pageerror', error => pageErrors.push(error.message))
    page.on('console', message => { if (message.type() === 'error') consoleErrors.push(message.text()) })
    page.on('response', response => { if (response.status() >= 400) responseErrors.push(`${response.status()} ${response.url()}`) })
    await page.goto(await ternilo.origin, { waitUntil: 'networkidle' })
    await chooseWorkspace(page, workspace)
    await configureFixture(page, model.baseUrl)

    await page.getByRole('textbox', { name: '输入任务' }).fill('请提出结构化问题')
    await page.getByRole('button', { name: '发送' }).click()
    const takeover = page.locator('[data-question-takeover]')
    await takeover.getByText(LONG_QUESTION, { exact: true }).waitFor()
    assert.equal(await takeover.getByText('选择策略', { exact: true }).isVisible(), true)
    assert.equal(await takeover.getByText('交付目标', { exact: true }).isVisible(), true)
    assert.equal(await takeover.getByText('先验证再修改。', { exact: true }).isVisible(), true)
    assert.equal(await takeover.getByText(LONG_DESCRIPTION, { exact: true }).isVisible(), true)
    await assertLongQuestionMobileLayout(page, takeover)
    await takeover.getByRole('radio', { name: '稳妥' }).click()

    await takeover.getByText('选择要覆盖的表面', { exact: true }).waitFor()
    await assertManyOptions(page, takeover, artifacts)
    await takeover.getByRole('checkbox', { name: '测试' }).tap()
    await swipeToEnd(page, takeover.locator('[data-question-scroll]'))
    await takeover.getByRole('checkbox', { name: '文档' }).click()
    await takeover.getByRole('textbox', { name: '输入回答' }).fill('发布说明')
    await takeover.getByRole('button', { name: '上一个问题' }).click()
    await takeover.getByRole('button', { name: '下一个问题', exact: true }).last().click()
    assert.equal(await takeover.locator('[data-question-scroll]').evaluate(element => element.scrollTop), 0)
    assert.equal(await takeover.getByRole('checkbox', { name: '测试' }).getAttribute('aria-checked'), 'true')
    await takeover.getByRole('button', { name: '收起问题' }).click()
    await takeover.getByRole('button', { name: '展开问题' }).click()
    await swipeToEnd(page, takeover.locator('[data-question-scroll]'))
    assert.equal(await takeover.getByRole('checkbox', { name: '文档' }).getAttribute('aria-checked'), 'true')
    assert.equal(await takeover.getByRole('textbox', { name: '输入回答' }).inputValue(), '发布说明')
    // Exercise the reduced content viewport used when a mobile keyboard is open.
    await page.setViewportSize({ width: 390, height: 360 })
    await takeover.getByRole('textbox', { name: '输入回答' }).focus()
    await swipeToEnd(page, takeover.locator('[data-question-scroll]'))
    await assertQuestionFitsViewport(page, takeover)
    const input = await takeover.getByRole('textbox', { name: '输入回答' }).boundingBox()
    const footer = await takeover.locator('footer').boundingBox()
    assert.ok(input.y >= 0 && input.y + input.height <= footer.y)
    if (artifacts) await page.screenshot({ path: path.join(artifacts, 'ask-small-input.png') })
    await takeover.getByRole('button', { name: '提交' }).click()
    await page.setViewportSize({ width: 390, height: 844 })

    const request = await model.resumed()
    const output = findToolOutput(request)
    assert.equal(typeof output, 'string')
    assert.deepEqual(JSON.parse(output), { answers: [
      { id: 'strategy', selected: ['稳妥 (Recommended)'] },
      { id: 'surfaces', selected: ['测试', '文档'], custom: '发布说明' },
    ] })
    await page.getByText('结构化回答已收到。', { exact: true }).waitFor()
    await takeover.waitFor({ state: 'detached' })

    const single = await visibleQuestion(page, LONG_QUESTION)
    assert.equal(await single.locator('[data-question-answer]').textContent(), '回答稳妥 (Recommended)')
    const multi = await visibleQuestion(page, '选择要覆盖的表面')
    assert.equal(await multi.locator('[data-question-answer]').textContent(), '回答测试, 文档, 发布说明')
    assert.equal(await page.getByText(BEFORE_QUESTION, { exact: true }).isVisible(), true)
    await page.reload({ waitUntil: 'domcontentloaded' })
    await page.locator('[data-history-state="loading"], [data-history-state="backfilling"]').waitFor({ state: 'detached' })
    assert.equal(await (await visibleQuestion(page, '选择要覆盖的表面')).locator('[data-question-answer]').textContent(), '回答测试, 文档, 发布说明')
    assert.equal(await page.getByText(BEFORE_QUESTION, { exact: true }).isVisible(), true)
    assert.equal(await page.getByText('结构化回答已收到。', { exact: true }).isVisible(), true)
    if (artifacts) {
      await page.getByText(BEFORE_QUESTION, { exact: true }).scrollIntoViewIfNeeded()
      await page.screenshot({ path: path.join(artifacts, 'ask-history-conversation.png') })
    }
    assert.deepEqual(pageErrors, [])
    assert.deepEqual(consoleErrors, [])
    assert.deepEqual(responseErrors, [])
    assert.equal(ternilo.diagnostics().includes('panicked'), false, ternilo.diagnostics())
  } catch (error) {
    if (process.env.TERNILO_E2E_ARTIFACT_DIR && browser?.contexts()[0]?.pages()[0]) {
      await mkdir(process.env.TERNILO_E2E_ARTIFACT_DIR, { recursive: true })
      await browser.contexts()[0].pages()[0].screenshot({ path: path.join(process.env.TERNILO_E2E_ARTIFACT_DIR, 'ask-failure.png') })
    }
    throw error
  } finally {
    await browser?.close()
    await stopProcess(ternilo.child)
    await model.close()
    await rm(dataDirectory, { recursive: true, force: true })
  }
})
