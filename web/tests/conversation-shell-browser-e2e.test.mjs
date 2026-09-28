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
const binary = path.join(repository, 'target', 'debug', 'ternilo')

function startTernilo(dataDirectory) {
  const child = spawn(binary, [
    'serve', '--listen', '127.0.0.1:0', '--data-dir', dataDirectory,
  ], { cwd: repository, stdio: ['ignore', 'pipe', 'pipe'] })
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

function completed(output, usage = {}) {
  return sse({
    type: 'response.completed',
    response: {
      status: 'completed',
      output,
      usage: {
        input_tokens: 120,
        output_tokens: 40,
        input_tokens_details: { cached_tokens: 60 },
        output_tokens_details: { reasoning_tokens: 7 },
        ...usage,
      },
    },
  })
}

const REVIEW_PLAN = [
  '# 发布工作台计划',
  '',
  '这份计划用于验证计划审阅卡的 **Markdown**、移动端布局与内部滚动。',
  '',
  '## 执行步骤',
  '',
  ...Array.from({ length: 36 }, (_, index) => `- 步骤 ${index + 1}：检查模块 ${index + 1} 的实现、测试和文档。`),
  '',
  '```rust',
  'fn ready() -> bool { true }',
  '```',
].join('\n')

async function startModelFixture() {
  const received = []
  const waiters = new Map()
  const responseTimers = new Set()
  const server = createServer((incoming, response) => {
    if (incoming.method !== 'POST' || incoming.url !== '/v1/responses') {
      response.writeHead(404, { 'content-type': 'application/json' })
      response.end('{"error":"not found"}')
      return
    }
    const body = []
    incoming.on('data', chunk => body.push(chunk))
    incoming.on('end', () => {
      const parsed = JSON.parse(Buffer.concat(body).toString('utf8'))
      if (typeof parsed.instructions === 'string' && parsed.instructions.includes('You name software-agent conversations')) {
        const title = '会话外壳交互验收'
        response.writeHead(200, { 'content-type': 'text/event-stream', 'cache-control': 'no-cache' })
        response.end([
          sse({ type: 'response.output_text.delta', output_index: 0, content_index: 0, delta: title }),
          completed([{ type: 'message', role: 'assistant', content: [{ type: 'output_text', text: title }] }], { input_tokens: 8, output_tokens: 3 }),
        ].join(''))
        return
      }
      const record = {
        headers: incoming.headers,
        body: parsed,
      }
      const index = received.push(record) - 1
      waiters.get(index)?.(record)
      waiters.delete(index)
      response.writeHead(200, {
        'content-type': 'text/event-stream',
        'cache-control': 'no-cache',
        'x-request-id': `conversation-shell-${index + 1}`,
      })

      let stream
      let interval
      if (index === 0) {
        stream = [
          sse({ type: 'response.reasoning_summary_text.delta', delta: '先检查共享会话外壳。' }),
          ...Array.from({ length: 80 }, (_, offset) => sse({
            type: 'response.output_text.delta',
            output_index: 0,
            content_index: 0,
            delta: `流式段落 ${offset + 1}：用于验证 pinned follow 与阅读位置冻结。\n\n`,
          })),
          completed([{
            type: 'message',
            role: 'assistant',
            content: [{ type: 'output_text', text: '流式回答完成。' }],
          }]),
        ]
        interval = 75
      } else if (index === 1) {
        const argumentsValue = JSON.stringify({ questions: [
          {
            id: 'continue',
            header: '确认',
            question: '要继续执行后续步骤吗？',
            detail: '**继续**会进入下一项配置。',
            options: [
              { label: '继续 (Recommended)', description: '保留当前工作并继续。' },
              { label: '停止', description: '结束当前任务。' },
            ],
          },
          {
            id: 'targets',
            question: '选择需要更新的内容',
            options: [{ label: '测试' }, { label: '文档' }, { label: '实现' }],
            multi_select: true,
          },
        ] })
        const call = { type: 'function_call', call_id: 'question-call', name: 'ask_user', arguments: argumentsValue }
        stream = [
          sse({ type: 'response.output_item.added', output_index: 0, item: { ...call, arguments: '' } }),
          sse({ type: 'response.function_call_arguments.done', output_index: 0, arguments: argumentsValue }),
          completed([call]),
        ]
        interval = 60
      } else if (index === 2) {
        stream = [
          sse({
            type: 'response.output_text.delta',
            output_index: 0,
            content_index: 0,
            delta: '收到你的回答，问题接管流程已恢复。',
          }),
          completed([{
            type: 'message',
            role: 'assistant',
            content: [{ type: 'output_text', text: '收到你的回答，问题接管流程已恢复。' }],
          }]),
        ]
        interval = 80
      } else if (index === 3) {
        const argumentsValue = JSON.stringify({ plan: REVIEW_PLAN })
        const call = { type: 'function_call', call_id: 'plan-review-call', name: 'exit_plan_mode', arguments: argumentsValue }
        stream = [
          sse({ type: 'response.output_item.added', output_index: 0, item: { ...call, arguments: '' } }),
          sse({ type: 'response.function_call_arguments.done', output_index: 0, arguments: argumentsValue }),
          completed([call]),
        ]
        interval = 60
      } else if (index === 4) {
        stream = [
          sse({
            type: 'response.output_text.delta',
            output_index: 0,
            content_index: 0,
            delta: '计划已批准，已返回执行模式。',
          }),
          completed([{
            type: 'message',
            role: 'assistant',
            content: [{ type: 'output_text', text: '计划已批准，已返回执行模式。' }],
          }]),
        ]
        interval = 80
      } else {
        stream = [
          ...Array.from({ length: 120 }, (_, offset) => sse({
            type: 'response.output_text.delta',
            output_index: 0,
            content_index: 0,
            delta: `等待停止 ${offset + 1}\n`,
          })),
          completed([{
            type: 'message',
            role: 'assistant',
            content: [{ type: 'output_text', text: '不应等到自然结束。' }],
          }]),
        ]
        interval = 100
      }

      let closed = false
      response.once('close', () => { closed = true })
      stream.forEach((chunk, offset) => {
        const timer = setTimeout(() => {
          responseTimers.delete(timer)
          if (closed) return
          response.write(chunk)
          if (offset === stream.length - 1) response.end()
        }, 80 + offset * interval)
        responseTimers.add(timer)
      })
    })
  })
  await new Promise((resolve, reject) => {
    server.once('error', reject)
    server.listen(0, '127.0.0.1', resolve)
  })
  const address = server.address()
  return {
    baseUrl: `http://127.0.0.1:${address.port}/v1`,
    request(index) {
      if (received[index]) return Promise.resolve(received[index])
      return new Promise(resolve => waiters.set(index, resolve))
    },
    close: () => new Promise((resolve, reject) => {
      for (const timer of responseTimers) clearTimeout(timer)
      responseTimers.clear()
      server.close(error => error ? reject(error) : resolve())
    }),
  }
}

async function chooseWorkspace(page, workspace) {
  await page.getByRole('button', { name: '选择工作文件夹' }).first().click()
  const dialog = page.getByRole('dialog', { name: '选择工作文件夹' })
  await dialog.getByRole('button', { name: '编辑文件夹路径' }).click()
  const pathInput = dialog.getByRole('textbox', { name: '编辑文件夹路径' })
  await pathInput.fill(workspace)
  await pathInput.press('Enter')
  await dialog.getByRole('list', { name: `目录 ${workspace}`, exact: true }).waitFor()
  await dialog.getByRole('button', { name: '打开所选文件夹' }).click()
  await dialog.waitFor({ state: 'detached' })
  await page.locator('[data-sidebar-new-session]').click()
  await page.getByRole('textbox', { name: '输入任务' }).waitFor()
}

async function configureFixture(page, baseUrl) {
  await page.evaluate(async baseUrlValue => {
    const headers = { 'content-type': 'application/json' }
    const token = window.__TERNILO_BOOT__?.apiToken
    if (token) headers.authorization = `Bearer ${token}`
    const request = async (pathValue, method, bodyValue) => {
      const response = await fetch(`/api/v1${pathValue}`, {
        method,
        headers,
        body: bodyValue === undefined ? undefined : JSON.stringify(bodyValue),
      })
      if (!response.ok) throw new Error(`${method} ${pathValue}: ${response.status} ${await response.text()}`)
      return response.status === 204 ? null : response.json()
    }
    await request('/credentials', 'POST', {
      name: 'TERNILO_PROVIDER_FIXTURE_API_KEY',
      value: 'browser-fixture-key',
    })
    await request('/providers', 'POST', {
      id: 'fixture',
      display_name: 'Fixture',
      base_url: baseUrlValue,
      protocol: 'openai-responses',
      api_key_ref: 'TERNILO_PROVIDER_FIXTURE_API_KEY',
      defaults: {
        context_window: 128000,
        max_output_tokens: 8192,
        reasoning: { default_effort: 'high', efforts: { high: 'ultra' } },
      },
      models: [{
        id: 'fixture-model',
        display_name: 'Fixture Model',
        settings: { mode: 'inherit' },
      }],
      timeout_ms: 120000,
      max_attempts: 1,
      retry_base_delay_ms: 50,
    })
    const sessionId = localStorage.getItem('ternilo.current-session')
    if (!sessionId) throw new Error('missing current session')
    await request(`/sessions/${encodeURIComponent(sessionId)}`, 'PATCH', {
      model: {
        provider: 'named_provider',
        provider_id: 'fixture',
        model: 'fixture-model',
        reasoning_effort: 'high',
      },
    })
  }, baseUrl)
  await page.reload({ waitUntil: 'domcontentloaded' })
  await page.getByRole('button', { name: /Fixture Model/ }).waitFor()
}

async function assertNoHorizontalOverflow(page, label) {
  const dimensions = await page.evaluate(() => ({
    viewport: innerWidth,
    document: document.documentElement.scrollWidth,
    body: document.body.scrollWidth,
    conversation: document.querySelector('#conversation-main')?.scrollWidth ?? 0,
    composer: (() => {
      const box = document.querySelector('.composer-shell')?.getBoundingClientRect()
      return box ? { left: box.left, right: box.right } : null
    })(),
  }))
  assert.equal(dimensions.document <= dimensions.viewport, true, `${label}: ${JSON.stringify(dimensions)}`)
  assert.equal(dimensions.body <= dimensions.viewport, true, `${label}: ${JSON.stringify(dimensions)}`)
  assert.equal(dimensions.composer !== null, true, `${label}: missing composer`)
  assert.equal(dimensions.composer.left >= -1 && dimensions.composer.right <= dimensions.viewport + 1, true, `${label}: ${JSON.stringify(dimensions)}`)
}

async function assertAtTail(page) {
  await page.waitForFunction(() => {
    const element = document.querySelector('.conversation-scroll')
    return element && element.scrollHeight - element.scrollTop - element.clientHeight < 8
  })
}

async function setConversationScroll(page, fraction, label) {
  const position = await page.locator('.conversation-scroll').evaluate((element, value) => {
    const maximum = element.scrollHeight - element.clientHeight
    if (maximum < 120) throw new Error(`conversation is not scrollable: ${maximum}`)
    element.dispatchEvent(new WheelEvent('wheel', { bubbles: true, deltaY: -1 }))
    element.scrollTop = Math.round(maximum * value)
    element.dispatchEvent(new Event('scroll'))
    return element.scrollTop
  }, fraction)
  assert.equal(position > 0, true, `${label}: expected a non-zero position`)
  return position
}

async function assertConversationScroll(page, expected, label) {
  try {
    await page.waitForFunction(value => {
      const element = document.querySelector('.conversation-scroll')
      return element && Math.abs(element.scrollTop - value) < 2
    }, expected, { timeout: 5_000 })
  } catch {
    const geometry = await page.locator('.conversation-scroll').evaluate(element => ({
      top: element.scrollTop,
      maximum: element.scrollHeight - element.clientHeight,
      view: element.querySelector('[data-conversation-view]')?.getAttribute('data-conversation-view'),
    }))
    assert.fail(`${label}: expected ${expected}, received ${JSON.stringify(geometry)}`)
  }
  const actual = await page.locator('.conversation-scroll').evaluate(element => element.scrollTop)
  assert.equal(Math.abs(actual - expected) < 2, true, `${label}: expected ${expected}, received ${actual}`)
}

async function revealAnsweredQuestion(page, prompt) {
  const row = page.locator('[data-question-lifecycle][data-state="answered"]').filter({ hasText: prompt }).first()
  await row.waitFor({ state: 'attached', timeout: 15_000 })
  const turn = row.locator('xpath=ancestor::*[@data-chat-turn][1]')
  const process = turn.locator('[data-turn-process]')
  if (await process.count() > 0 && await process.getAttribute('aria-expanded') === 'false') await process.click()
  await row.waitFor({ state: 'visible' })
  return row
}

test('conversation shell closes hero, streaming, reader intent, question, stop and mobile flows', { timeout: 120_000 }, async () => {
  const dataDirectory = await mkdtemp(path.join(tmpdir(), 'ternilo-conversation-shell-'))
  const workspace = path.join(dataDirectory, 'workspace')
  await mkdir(workspace)
  const model = await startModelFixture()
  const ternilo = startTernilo(dataDirectory)
  let browser
  try {
    const origin = await ternilo.origin
    browser = await chromium.launch({ headless: true })
    const page = await browser.newPage({ viewport: { width: 1440, height: 900 } })
    const pageErrors = []
    page.on('pageerror', error => pageErrors.push(error.message))
    await page.goto(origin, { waitUntil: 'networkidle' })
    await chooseWorkspace(page, workspace)

    const hero = page.locator('[data-new-session-hero]')
    const input = page.getByRole('textbox', { name: '输入任务' })
    await hero.waitFor()
    assert.equal(await hero.getByText('让想法，动起来', { exact: true }).isVisible(), true)
    assert.equal(await input.getAttribute('placeholder'), '描述你想要构建的内容')
    for (const viewport of [
      { width: 390, height: 844, label: 'mobile portrait' },
      { width: 390, height: 430, label: 'mobile short' },
      { width: 844, height: 390, label: 'mobile landscape' },
    ]) {
      await page.setViewportSize(viewport)
      await assertNoHorizontalOverflow(page, viewport.label)
      assert.equal(await hero.isVisible(), true, `${viewport.label}: hero hidden`)
    }

    await page.setViewportSize({ width: 1440, height: 900 })
    await configureFixture(page, model.baseUrl)
    await input.evaluate(element => { element.dataset.residentProbe = 'true' })
    await input.fill('请输出足够长的流式内容')
    await page.getByRole('button', { name: '发送' }).click()
    await model.request(0)
    await page.getByRole('button', { name: '停止运行' }).waitFor()
    assert.equal(await page.getByRole('button', { name: '停止运行' }).isVisible(), true)
    await hero.waitFor({ state: 'detached' })
    assert.equal(await input.getAttribute('data-resident-probe'), 'true')
    await page.getByText('流式段落 12：用于验证 pinned follow 与阅读位置冻结。', { exact: true }).waitFor()
    await assertAtTail(page)

    const scroll = page.locator('.conversation-scroll')
    await scroll.hover()
    await page.mouse.wheel(0, -560)
    await page.getByRole('button', { name: '回到底部' }).waitFor()
    const readerTop = await scroll.evaluate(element => element.scrollTop)
    await page.getByText('流式段落 55：用于验证 pinned follow 与阅读位置冻结。', { exact: true }).waitFor()
    assert.equal(Math.abs(await scroll.evaluate(element => element.scrollTop) - readerTop) < 2, true)
    await page.getByRole('button', { name: '回到底部', exact: true }).click()
    await page.getByText('流式段落 80：用于验证 pinned follow 与阅读位置冻结。', { exact: true }).waitFor()
    await assertAtTail(page)
    await page.getByRole('button', { name: '发送' }).waitFor()

    const header = page.locator('.session-header')
    const headerY = (await header.boundingBox()).y
    const composerY = (await page.locator('.composer-shell').boundingBox()).y
    await scroll.evaluate(element => { element.scrollTop = 0 })
    await page.waitForTimeout(80)
    assert.equal(Math.abs((await header.boundingBox()).y - headerY) < 1, true)
    assert.equal(Math.abs((await page.locator('.composer-shell').boundingBox()).y - composerY) < 1, true)

    const chatTab = page.getByRole('tab', { name: '对话' })
    const trajectoryTab = page.getByRole('tab', { name: /轨迹/ })
    const chatBox = await chatTab.boundingBox()
    const trajectoryBox = await trajectoryTab.boundingBox()
    const tabGap = trajectoryBox.x - (chatBox.x + chatBox.width)
    assert.ok(tabGap >= 0 && tabGap <= 8, `segmented view tabs overlap or separate: ${tabGap}px`)
    assert.ok(Math.abs(chatBox.y - trajectoryBox.y) < 1, 'view tabs must share a row')
    assert.ok(chatBox.height >= 30 && trajectoryBox.height >= 30, 'view tab targets are too small')
    await page.setViewportSize({ width: 1440, height: 520 })
    const chatPosition = await setConversationScroll(page, 0.31, 'chat memory')
    await trajectoryTab.click()
    const trajectoryToolbar = page.getByRole('toolbar', { name: '轨迹工具栏' })
    await trajectoryToolbar.waitFor()
    const timeline = page.locator('[data-trajectory-timeline]')
    const ledger = page.locator('[data-trajectory-ledger]')
    assert.equal(await timeline.isVisible(), true)
    assert.equal(await ledger.isVisible(), true)
    assert.match(await ledger.textContent(), /第 1 轮/)
    assert.match(await ledger.textContent(), /LLM #1/)
    const timelineScale = timeline.locator('[data-trajectory-timeline-scale]')
    const initialScale = await timelineScale.textContent()
    await timeline.getByRole('button', { name: '放大时间轴' }).click()
    assert.notEqual(await timelineScale.textContent(), initialScale)
    await timeline.getByRole('button', { name: '复位时间轴' }).click()
    assert.equal(await timelineScale.textContent(), initialScale)
    const trajectorySearch = trajectoryToolbar.getByRole('searchbox', { name: '搜索轨迹' })
    await trajectorySearch.fill('流式段落 80')
    assert.equal(await ledger.locator('[data-trajectory-record]').count() >= 1, true)
    await trajectorySearch.fill('')
    const turnDisclosure = ledger.locator('[aria-expanded]').first()
    await turnDisclosure.click()
    assert.equal(await turnDisclosure.getAttribute('aria-expanded'), 'false')
    await turnDisclosure.click()
    assert.equal(await turnDisclosure.getAttribute('aria-expanded'), 'true')
    const trajectoryPosition = await setConversationScroll(page, 0.57, 'trajectory memory')
    await chatTab.click()
    await assertConversationScroll(page, chatPosition, 'chat view restore')
    await trajectoryTab.click()
    await assertConversationScroll(page, trajectoryPosition, 'trajectory view restore')
    await chatTab.click()

    const originalSessionTitle = await page.locator('[data-sidebar-session-active="true"] [data-sidebar-session-title]').textContent()
    assert.equal(Boolean(originalSessionTitle), true)
    await page.locator('[data-sidebar-new-session]').click()
    await hero.waitFor()
    await page.locator('[data-sidebar-session-button]').filter({ hasText: originalSessionTitle }).click()
    await page.getByText('流式段落 80：用于验证 pinned follow 与阅读位置冻结。', { exact: true }).waitFor()
    await assertConversationScroll(page, chatPosition, 'chat session restore')
    await trajectoryTab.click()
    await assertConversationScroll(page, trajectoryPosition, 'trajectory session restore')
    await chatTab.click()
    await page.setViewportSize({ width: 1440, height: 900 })

    await scroll.evaluate(element => { element.scrollTop = 0 })
    await input.fill('请先询问我是否继续')
    await page.getByRole('button', { name: '发送' }).click()
    await model.request(1)
    await assertAtTail(page)
    const question = page.locator('[data-question-takeover]')
    await question.getByText('要继续执行后续步骤吗？', { exact: true }).waitFor()
    const continueOption = question.getByRole('radio', { name: '继续' })
    assert.equal(await continueOption.isVisible(), true)
    await continueOption.click()
    await question.getByText('选择需要更新的内容', { exact: true }).waitFor()
    await question.getByRole('checkbox', { name: '测试' }).click()
    await question.getByRole('checkbox', { name: '文档' }).click()
    await question.getByRole('textbox', { name: '输入回答' }).fill('发布说明')
    await question.getByRole('button', { name: '提交' }).click()
    const resumedRequest = await model.request(2)
    const answerOutput = resumedRequest.body.input.find(item => (
      item.type === 'function_call_output' && item.call_id === 'question-call'
    ))
    assert.deepEqual(JSON.parse(answerOutput.output), { answers: [
      { id: 'continue', selected: ['继续 (Recommended)'] },
      { id: 'targets', selected: ['测试', '文档'], custom: '发布说明' },
    ] })
    await question.waitFor({ state: 'detached' })
    await page.getByText('收到你的回答，问题接管流程已恢复。', { exact: true }).waitFor()
    await page.getByRole('button', { name: '发送' }).waitFor()

    let answeredQuestion = await revealAnsweredQuestion(page, '要继续执行后续步骤吗？')
    assert.equal(await answeredQuestion.locator('[data-question-state-label]').textContent(), '已回答')
    assert.equal(await answeredQuestion.locator('[data-question-answer]').textContent(), '回答继续 (Recommended)')
    assert.equal((await answeredQuestion.textContent()).includes('等待回答'), false)

    const answeredMulti = await revealAnsweredQuestion(page, '选择需要更新的内容')
    assert.equal(await answeredMulti.locator('[data-question-answer]').textContent(), '回答测试, 文档, 发布说明')

    await page.reload({ waitUntil: 'domcontentloaded' })
    await page.getByRole('button', { name: /Fixture Model/ }).waitFor()
    answeredQuestion = await revealAnsweredQuestion(page, '要继续执行后续步骤吗？')
    assert.equal(await answeredQuestion.locator('[data-question-state-label]').textContent(), '已回答')
    assert.equal((await answeredQuestion.textContent()).includes('等待回答'), false)

    await page.setViewportSize({ width: 390, height: 844 })
    await assertNoHorizontalOverflow(page, 'answered question history mobile')
    const answeredBox = await answeredQuestion.boundingBox()
    assert.equal(answeredBox.x >= 0 && answeredBox.x + answeredBox.width <= 391, true, 'answered question history escaped viewport')
    await page.setViewportSize({ width: 1440, height: 900 })

    await input.fill('/plan')
    await page.getByRole('button', { name: '发送' }).click()
    await page.getByRole('button', { name: '切换到执行模式' }).waitFor()
    await input.fill('请给出完整计划并交给我审阅')
    await page.getByRole('button', { name: '发送' }).click()
    await model.request(3)

    const review = page.locator('[data-plan-review]')
    await review.waitFor()
    assert.equal(await page.locator('[data-question-takeover]').count(), 0)
    assert.equal(await review.getByText('计划审阅', { exact: true }).isVisible(), true)
    assert.equal(await review.getByRole('heading', { name: '发布工作台计划' }).isVisible(), true)
    assert.equal(await review.getByRole('button', { name: '讨论修改' }).isVisible(), true)
    assert.equal(await review.getByRole('button', { name: '要求修改' }).getAttribute('title'), 'Keep planning')
    assert.equal(await review.getByRole('button', { name: '批准计划' }).getAttribute('title'), 'Approve')

    for (const viewport of [
      { width: 390, height: 844, label: 'plan review mobile portrait' },
      { width: 390, height: 430, label: 'plan review mobile short' },
    ]) {
      await page.setViewportSize(viewport)
      const geometry = await review.evaluate(element => {
        const card = element.getBoundingClientRect()
        const scroller = element.querySelector('[data-plan-review-scroll]')
        return {
          viewportWidth: document.documentElement.clientWidth,
          documentWidth: document.documentElement.scrollWidth,
          card: { left: card.left, right: card.right, top: card.top, bottom: card.bottom },
          scrolls: scroller.scrollHeight > scroller.clientHeight,
          overflow: getComputedStyle(scroller).overflowY,
          buttons: [...element.querySelectorAll('[data-plan-review-action]')].map(button => ({
            label: button.textContent.trim(),
            height: button.getBoundingClientRect().height,
          })),
        }
      })
      assert.equal(geometry.documentWidth, geometry.viewportWidth, `${viewport.label}: horizontal overflow`)
      assert.equal(geometry.card.left >= 0 && geometry.card.right <= viewport.width, true, `${viewport.label}: card escaped viewport`)
      assert.equal(geometry.card.top >= 0 && geometry.card.bottom <= viewport.height + 1, true, `${viewport.label}: card escaped vertically`)
      assert.equal(geometry.scrolls, true, `${viewport.label}: long plan did not scroll internally`)
      assert.equal(geometry.overflow, 'auto')
      assert.deepEqual(geometry.buttons.filter(button => button.height < 40), [])
    }

    await page.setViewportSize({ width: 1440, height: 900 })
    await review.getByRole('button', { name: '批准计划' }).click()
    await model.request(4)
    await review.waitFor({ state: 'detached' })
    await page.getByText('计划已批准，已返回执行模式。', { exact: true }).waitFor()
    await page.getByRole('button', { name: '切换到计划模式' }).waitFor()

    await input.fill('运行一个可停止的长任务')
    await page.getByRole('button', { name: '发送' }).click()
    await model.request(5)
    const stop = page.getByRole('button', { name: '停止运行' })
    await stop.waitFor()
    await page.locator('.markdown-streaming').filter({ hasText: '等待停止 2' }).waitFor()
    await stop.click()
    await page.getByRole('button', { name: '发送' }).waitFor({ timeout: 15_000 })

    for (const viewport of [
      { width: 390, height: 844, label: 'active mobile portrait' },
      { width: 390, height: 430, label: 'active mobile short' },
      { width: 844, height: 390, label: 'active mobile landscape' },
    ]) {
      await page.setViewportSize(viewport)
      await assertNoHorizontalOverflow(page, viewport.label)
      if (viewport.width === 390) {
        assert.equal(await page.getByRole('button', { name: '打开侧边栏' }).isVisible(), true)
      }
    }

    await page.setViewportSize({ width: 390, height: 844 })
    await page.getByRole('tab', { name: '轨迹', exact: true }).click()
    await page.getByRole('toolbar', { name: '轨迹工具栏' }).waitFor()
    await assertNoHorizontalOverflow(page, 'trajectory mobile portrait')
    const mobileMetrics = page.locator('[data-trajectory-record]').first().locator('span').filter({ hasText: /输入 .*输出 .*推理 .*缓存/ })
    assert.equal(await mobileMetrics.count() >= 1, true)
    for (const button of await page.locator('[data-trajectory-timeline-controls] button:visible').all()) {
      const box = await button.boundingBox()
      assert.equal(box.width >= 40 && box.height >= 40, true)
    }
    await page.getByRole('button', { name: '返回对话' }).click()

    assert.deepEqual(pageErrors, [])
    assert.equal(ternilo.diagnostics().includes('panicked'), false, ternilo.diagnostics())
  } finally {
    await browser?.close()
    await stopProcess(ternilo.child)
    await model.close()
    await rm(dataDirectory, { recursive: true, force: true })
  }
})
