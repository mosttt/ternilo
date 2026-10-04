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
const binary = path.join(repository, 'target', 'debug', 'ternilo')

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
    child.once('exit', code => reject(new Error(`Ternilo exited ${code}: ${diagnostics}`)))
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

function releaseGate() {
  let release
  const promise = new Promise(resolve => { release = resolve })
  return { promise, release }
}

async function startModelFixture({ paging = false } = {}) {
  const requests = []
  const waiters = new Map()
  const streamGates = { inspection: releaseGate(), reader: releaseGate(), completion: releaseGate() }
  let retryAttempts = 0
  let pagingTurns = 0
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
        const title = 'Markdown 文件读取'
        response.writeHead(200, { 'content-type': 'text/event-stream', 'cache-control': 'no-cache' })
        response.end([
          sse({ type: 'response.output_text.delta', output_index: 0, content_index: 0, delta: title }),
          sse({ type: 'response.completed', response: {
            status: 'completed',
            output: [{ type: 'message', role: 'assistant', content: [{ type: 'output_text', text: title }] }],
            usage: { input_tokens: 8, output_tokens: 3, input_tokens_details: { cached_tokens: 0 } },
          } }),
        ].join(''))
        return
      }
      if (paging) {
        const answer = `ternilo: 分页提示 ${pagingTurns++}`
        response.writeHead(200, { 'content-type': 'text/event-stream', 'cache-control': 'no-cache' })
        response.end(sse({ type: 'response.output_text.delta', output_index: 0, content_index: 0, delta: answer }) + sse({ type: 'response.completed', response: {
          status: 'completed',
          output: [{ type: 'message', role: 'assistant', content: [{ type: 'output_text', text: answer }] }],
          usage: { input_tokens: 4, output_tokens: 2, input_tokens_details: { cached_tokens: 0 } },
        } }))
        return
      }
      if (JSON.stringify(request).includes('触发模型重试验收')) {
        retryAttempts += 1
        if (retryAttempts === 1) {
          response.writeHead(503, { 'content-type': 'application/json' })
          response.end(JSON.stringify({ error: { message: 'temporary fixture outage', code: 'fixture_unavailable' } }))
          return
        }
        const answer = '重试恢复成功'
        response.writeHead(200, { 'content-type': 'text/event-stream', 'cache-control': 'no-cache' })
        response.end([
          sse({ type: 'response.output_text.delta', output_index: 0, content_index: 0, delta: answer }),
          sse({ type: 'response.completed', response: {
            status: 'completed',
            output: [{ type: 'message', role: 'assistant', content: [{ type: 'output_text', text: answer }] }],
            usage: { input_tokens: 5, output_tokens: 3, input_tokens_details: { cached_tokens: 0 } },
          } }),
        ].join(''))
        return
      }
      const index = requests.push(request) - 1
      waiters.get(index)?.(request)
      waiters.delete(index)
      response.writeHead(200, { 'content-type': 'text/event-stream', 'cache-control': 'no-cache' })
      if (index === 0) {
        const calls = Array.from({ length: 4 }, (_, callIndex) => {
          const argumentsValue = JSON.stringify({ path: 'sample.txt' })
          return {
            type: 'function_call',
            call_id: callIndex === 0 ? 'read-fixture' : `read-fixture-${callIndex + 1}`,
            name: 'read_file',
            arguments: argumentsValue,
          }
        })
        const stream = [
          ...calls.flatMap((call, outputIndex) => [
            sse({ type: 'response.output_item.added', output_index: outputIndex, item: { ...call, arguments: '' } }),
            sse({ type: 'response.function_call_arguments.done', output_index: outputIndex, arguments: call.arguments }),
          ]),
          sse({ type: 'response.completed', response: {
            status: 'completed', output: calls,
            usage: { input_tokens: 30, output_tokens: 2, input_tokens_details: { cached_tokens: 0 } },
          } }),
        ]
        stream.forEach((chunk, offset) => setTimeout(() => {
          response.write(chunk)
          if (offset === stream.length - 1) response.end()
        }, 60 + offset * 50))
        return
      }
      if (index > 1) {
        const answer = `分页回答 ${index}`
        response.end([
          sse({ type: 'response.output_text.delta', output_index: 0, content_index: 0, delta: answer }),
          sse({ type: 'response.completed', response: {
            status: 'completed',
            output: [{ type: 'message', role: 'assistant', content: [{ type: 'output_text', text: answer }] }],
            usage: { input_tokens: 4, output_tokens: 2, input_tokens_details: { cached_tokens: 0 } },
          } }),
        ].join(''))
        return
      }
      const paragraphs = Array.from({ length: 42 }, (_, value) => `流式验收段落 ${value + 1}：保持跟随与阅读位置。\n\n`)
      const answer = `## 工具读取完成\n\n**Markdown 正常**\n\n\`\`\`rust\nfn main() {}\n\`\`\`\n\n${paragraphs.join('')}`
      const writeText = delta => response.write(sse({
        type: 'response.output_text.delta', output_index: 0, content_index: 0, delta,
      }))
      response.write(sse({ type: 'response.reasoning_summary_text.delta', delta: '先读取文件，再整理最终答案。' }))
      writeText(answer.slice(0, 130))
      void (async () => {
        // Keep each stream phase open until the browser has exercised its controls.
        await streamGates.inspection.promise
        paragraphs.slice(0, 8).forEach(writeText)
        await streamGates.reader.promise
        paragraphs.slice(8, 24).forEach(writeText)
        await streamGates.completion.promise
        paragraphs.slice(24).forEach(writeText)
        response.end(sse({ type: 'response.completed', response: {
          status: 'completed',
          output: [{ type: 'message', role: 'assistant', content: [{ type: 'output_text', text: answer }] }],
          usage: {
            input_tokens: 60, output_tokens: 20,
            input_tokens_details: { cached_tokens: 20 },
            output_tokens_details: { reasoning_tokens: 4 },
          },
        } }))
      })()
    })
  })
  await new Promise((resolve, reject) => { server.once('error', reject); server.listen(0, '127.0.0.1', resolve) })
  return {
    baseUrl: `http://127.0.0.1:${server.address().port}/v1`,
    streamGates,
    request(index) { return requests[index] ? Promise.resolve(requests[index]) : new Promise(resolve => waiters.set(index, resolve)) },
    requestCount() { return requests.length },
    close: () => new Promise((resolve, reject) => server.close(error => error ? reject(error) : resolve())),
  }
}

async function api(page, endpoint, init = {}) {
  return page.evaluate(async ({ endpoint, init }) => {
    const token = window.__TERNILO_BOOT__?.apiToken ?? ''
    const response = await fetch(`/api/v1${endpoint}`, {
      ...init,
      headers: { ...(token ? { authorization: `Bearer ${token}` } : {}), ...(init.body === undefined ? {} : { 'content-type': 'application/json' }) },
      body: init.body === undefined ? undefined : JSON.stringify(init.body),
    })
    if (!response.ok) throw new Error(`${endpoint}: ${response.status} ${await response.text()}`)
    if (response.status === 204) {
      await response.arrayBuffer()
      return null
    }
    return response.json()
  }, { endpoint, init })
}

async function atTail(page) {
  try {
    await page.waitForFunction(() => {
      const element = document.querySelector('.conversation-scroll')
      return element && element.scrollHeight - element.scrollTop - element.clientHeight < 12
    })
  } catch (error) {
    const geometry = await page.evaluate(() => {
      const element = document.querySelector('.conversation-scroll')
      return element ? {
        scrollHeight: element.scrollHeight,
        scrollTop: element.scrollTop,
        clientHeight: element.clientHeight,
        distance: element.scrollHeight - element.scrollTop - element.clientHeight,
      } : null
    })
    throw new Error(`conversation did not reach tail: ${JSON.stringify(geometry)}`, { cause: error })
  }
}

async function tap(page, locator) {
  const box = await locator.boundingBox()
  assert.ok(box, `cannot tap hidden target ${await locator.getAttribute('aria-label') ?? ''}`)
  await page.touchscreen.tap(box.x + box.width / 2, box.y + box.height / 2)
}

function observePage(page, observations) {
  page.on('pageerror', error => observations.pageErrors.push(error.message))
  page.on('console', message => {
    if (message.type() === 'error') observations.consoleErrors.push(message.text())
  })
  page.on('requestfailed', request => {
    if (request.failure()?.errorText === 'net::ERR_ABORTED') return
    observations.failedRequests.push(`${request.method()} ${request.url()}: ${request.failure()?.errorText ?? 'unknown error'}`)
  })
  page.on('response', response => {
    if (response.status() >= 400) {
      observations.failedResponses.push(`${response.request().method()} ${response.url()}: ${response.status()}`)
    }
  })
}

test('Chat and Details close history, feedback, stream, tool, fold, usage and mobile flows on Salvo', { timeout: 180_000 }, async () => {
  const dataDirectory = await mkdtemp(path.join(tmpdir(), 'ternilo-chat-details-'))
  const workspacePath = path.join(dataDirectory, 'workspace')
  await mkdir(workspacePath)
  await writeFile(path.join(workspacePath, 'sample.txt'), 'fixture file content\nsecond line\n')
  const model = await startModelFixture()
  const ternilo = startTernilo(dataDirectory)
  let browser
  try {
    const origin = await ternilo.origin
    browser = await chromium.launch({
      headless: true,
      executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined,
    })
    const context = await browser.newContext({ locale: 'zh-CN', viewport: { width: 1280, height: 820 } })
    let page = await context.newPage()
    const observations = { pageErrors: [], consoleErrors: [], failedRequests: [], failedResponses: [] }
    observePage(page, observations)
    await page.goto(origin, { waitUntil: 'networkidle' })
    const workspace = await api(page, '/workspaces', { method: 'POST', body: { path: workspacePath } })
    const session = await api(page, '/sessions', { method: 'POST', body: { workspace_id: workspace.workspace_id, agent_preset: 'standard' } })
    const sessionId = session.identity?.session_id ?? session.session_id
    await api(page, '/credentials', { method: 'POST', body: { name: 'TERNILO_PROVIDER_CHAT_DETAILS_API_KEY', value: 'fixture-key' } })
    await api(page, '/providers', { method: 'POST', body: {
      id: 'chat-details', display_name: 'Chat Details Fixture', base_url: model.baseUrl,
      protocol: 'openai-responses', api_key_ref: 'TERNILO_PROVIDER_CHAT_DETAILS_API_KEY',
      defaults: { context_window: 128000, max_output_tokens: 8192 },
      models: [{ id: 'fixture-model', display_name: 'Fixture Model', settings: { mode: 'inherit' } }],
      timeout_ms: 120000, max_attempts: 2, retry_base_delay_ms: 50,
    } })
    await api(page, `/sessions/${encodeURIComponent(sessionId)}`, { method: 'PATCH', body: {
      model: { provider: 'named_provider', provider_id: 'chat-details', model: 'fixture-model' },
    } })
    await page.reload({ waitUntil: 'domcontentloaded' })
    const input = page.getByRole('textbox', { name: '输入任务' })
    await input.waitFor()
    await input.fill('读取 sample.txt 并用 Markdown 回答')
    await page.getByRole('button', { name: '发送' }).click()
    await model.request(0)
    await model.request(1)

    const tool = page.locator('[data-tool-call-id="read-fixture"]')
    await tool.waitFor()
    await page.waitForFunction(() => document.querySelector('[data-tool-call-id="read-fixture"]')?.getAttribute('data-state') === 'complete')
    assert.match(await tool.textContent(), /读取文件/)
    await tool.locator('[data-tool-call-toggle]').click()
    assert.match(await tool.locator('[data-tool-view="read"]').textContent(), /fixture file content/)
    assert.equal(await page.getByRole('complementary', { name: '详情' }).count(), 0, '展开行内工具结果不应误打开 Details')
    await tool.locator('[data-tool-call-inspect]').click()
    let details = page.getByRole('complementary', { name: '详情' })
    await details.waitFor()
    assert.match(await details.textContent(), /输入/)
    assert.match(await details.textContent(), /输出/)
    assert.match(await details.textContent(), /sample\.txt/)
    const inputTree = details.getByRole('tree', { name: '输入 JSON 树' })
    await inputTree.waitFor()
    assert.match(await inputTree.textContent(), /path:"sample\.txt"/)
    const rawDisclosure = details.locator('details').filter({ hasText: '原始内容' })
    await rawDisclosure.locator('summary').click()
    const rawTree = rawDisclosure.getByRole('tree', { name: '原始内容 JSON 树' })
    const startedExpander = rawTree.locator('[data-json-path="$.started"] [data-json-expander]')
    await startedExpander.focus()
    await startedExpander.press('ArrowRight')
    await page.waitForFunction(() => document.querySelector('[data-json-path="$.started"] [data-json-expander]')?.getAttribute('aria-expanded') === 'true')
    assert.equal(await startedExpander.getAttribute('aria-expanded'), 'true')
    assert.match(await rawTree.textContent(), /run_id:/)
    await startedExpander.press('ArrowLeft')
    await page.waitForFunction(() => document.querySelector('[data-json-path="$.started"] [data-json-expander]')?.getAttribute('aria-expanded') === 'false')
    assert.equal(await startedExpander.getAttribute('aria-expanded'), 'false')
    const rawInspector = rawDisclosure.locator('[data-json-inspector]')
    await rawInspector.getByRole('button', { name: '查看原始 JSON' }).click()
    assert.match(await rawInspector.locator('[data-json-raw]').textContent(), /"started"/)
    assert.match(await rawInspector.locator('[data-json-raw]').textContent(), /"finished"/)
    await rawInspector.getByRole('button', { name: '查看结构化 JSON' }).click()
    await rawInspector.getByRole('button', { name: '复制完整 JSON' }).waitFor()
    await details.getByRole('button', { name: '关闭详情' }).click()

    model.streamGates.inspection.release()
    await page.getByText('流式验收段落 8：保持跟随与阅读位置。', { exact: true }).waitFor()
    const scroll = page.locator('.conversation-scroll')
    await atTail(page)
    await scroll.evaluate(element => {
      element.dispatchEvent(new WheelEvent('wheel', { bubbles: true, deltaY: -260 }))
      element.scrollTop = Math.max(0, element.scrollHeight - element.clientHeight - 260)
      element.dispatchEvent(new Event('scroll'))
    })
    const readerTop = await scroll.evaluate(element => element.scrollTop)
    model.streamGates.reader.release()
    await page.getByText('流式验收段落 24：保持跟随与阅读位置。', { exact: true }).waitFor()
    assert.equal(Math.abs(await scroll.evaluate(element => element.scrollTop) - readerTop) < 3, true)
    await page.getByRole('button', { name: '回到底部', exact: true }).click()
    model.streamGates.completion.release()
    await page.getByText('流式验收段落 42：保持跟随与阅读位置。', { exact: true }).waitFor()
    await page.getByText('Markdown 正常', { exact: true }).waitFor()
    await page.getByRole('button', { name: '发送' }).waitFor()
    await atTail(page)
    assert.equal(await page.locator('article[data-role="assistant"] strong').filter({ hasText: 'Markdown 正常' }).count(), 1)
    assert.equal(await page.locator('article[data-role="assistant"] code.language-rust').count(), 1)

    const turnProcess = page.locator('[data-turn-process="1"]')
    await turnProcess.waitFor()
    if (await turnProcess.getAttribute('aria-expanded') === 'true') await turnProcess.click()
    assert.equal(await turnProcess.getAttribute('aria-expanded'), 'false')
    const collapsedLayout = await turnProcess.evaluate(element => {
      const turn = element.closest('[data-chat-turn]')
      const answer = turn?.querySelector('[data-turn-process-answer]')
      const processRect = element.getBoundingClientRect()
      const answerRect = answer?.getBoundingClientRect()
      const hiddenMembers = [...(turn?.querySelectorAll('[data-turn-process-member][hidden], [data-turn-inline-reasoning][hidden]') ?? [])]
      return {
        gap: answerRect ? answerRect.top - processRect.bottom : null,
        hiddenMembers: hiddenMembers.map(member => ({
          hidden: member.getAttribute('hidden'),
          boxes: member.getClientRects().length,
          height: member.getBoundingClientRect().height,
        })),
      }
    })
    assert.ok(collapsedLayout.hiddenMembers.length >= 4, JSON.stringify(collapsedLayout))
    assert.equal(collapsedLayout.hiddenMembers.every(member => member.boxes === 0 && member.height === 0), true, JSON.stringify(collapsedLayout))
    assert.ok(collapsedLayout.gap !== null && collapsedLayout.gap >= 0 && collapsedLayout.gap <= 20, JSON.stringify(collapsedLayout))
    if (await turnProcess.getAttribute('aria-expanded') === 'false') await turnProcess.click()
    assert.equal(await tool.isVisible(), true)
    const usage = page.locator('[data-turn-usage]')
    await usage.locator('summary').click()
    assert.match(await usage.textContent(), /缓存读取20 tok/)
    assert.match(await usage.textContent(), /输出22 tok/)

    const positive = page.getByRole('button', { name: '好回答' }).last()
    await positive.click()
    await page.waitForFunction(() => document.querySelector('[data-message-actions="assistant"] button[aria-pressed="true"]')?.getAttribute('aria-label') === '移除评价')
    await page.getByRole('button', { name: '移除评价' }).click()
    await page.waitForFunction(() => !document.querySelector('[data-message-actions="assistant"] button[aria-pressed="true"]'))

    const negative = page.getByRole('button', { name: '不好的回答' }).last()
    await negative.click()
    await page.waitForFunction(() => document.querySelector('[data-message-actions="assistant"] button[aria-pressed="true"]')?.getAttribute('aria-label') === '移除评价')
    await page.getByRole('button', { name: '编辑反馈备注' }).click()
    await page.getByRole('textbox', { name: '反馈备注' }).fill('需要补充失败恢复说明')
    await page.getByRole('button', { name: '保存' }).click()
    await page.getByRole('button', { name: '编辑反馈备注' }).filter({ hasText: '需要补充失败恢复说明' }).waitFor()

    let failFeedbackOnce = true
    await page.route('**/api/v1/sessions/*/feedback', async route => {
      if (!failFeedbackOnce) return route.continue()
      failFeedbackOnce = false
      await route.fulfill({ status: 500, contentType: 'application/json', body: JSON.stringify({ error: 'feedback fixture failure' }) })
    })
    await page.getByRole('button', { name: '移除评价' }).click()
    await page.waitForFunction(() => !document.querySelector('[data-message-actions="assistant"] button:disabled'))
    assert.equal(await page.getByRole('button', { name: '移除评价' }).getAttribute('aria-pressed'), 'true')
    await page.getByRole('button', { name: '编辑反馈备注' }).filter({ hasText: '需要补充失败恢复说明' }).waitFor({ state: 'visible' })
    await page.unroute('**/api/v1/sessions/*/feedback')
    await page.getByRole('button', { name: '移除评价' }).click()
    await page.waitForFunction(() => !document.querySelector('[data-message-actions="assistant"] button[aria-pressed="true"]'))
    assert.equal(await page.getByText('需要补充失败恢复说明', { exact: true }).count(), 0)

    await input.fill('触发模型重试验收')
    await page.getByRole('button', { name: '发送' }).click()
    await page.getByText('重试恢复成功', { exact: true }).waitFor()
    const retryTurn = page.locator('[data-chat-turn]').last()
    const retryProcess = retryTurn.locator('[data-turn-process]')
    if (await retryProcess.count() && await retryProcess.getAttribute('aria-expanded') === 'false') await retryProcess.click()
    const retry = retryTurn.locator('[data-chat-event="retry"][data-state="started"]')
    await retry.waitFor()
    await retry.getByRole('button').click()
    assert.match(await retry.textContent(), /temporary fixture outage/)
    assert.match(await retry.textContent(), /50 ms/)

    await page.evaluate(() => { localStorage.setItem('ternilo.transcript-view', 'normal'); window.dispatchEvent(new CustomEvent('ternilo:transcript-view-changed', { detail: 'normal' })) })
    assert.equal(await page.locator('[data-turn-process="1"]').count(), 0)
    await page.evaluate(() => { localStorage.setItem('ternilo.transcript-view', 'compact'); window.dispatchEvent(new CustomEvent('ternilo:transcript-view-changed', { detail: 'compact' })) })
    await page.locator('[data-turn-process="1"]').waitFor()

    const desktopStats = page.locator('.session-stats-line')
    await desktopStats.waitFor()
    await page.waitForFunction(() => !document.querySelector('.session-stats-line')?.hasAttribute('data-truncated'))
    const desktopStatsLayout = await desktopStats.evaluate(element => ({
      overflowX: getComputedStyle(element).overflowX,
      clientWidth: element.clientWidth,
      scrollWidth: element.scrollWidth,
    }))
    assert.equal(desktopStatsLayout.overflowX, 'hidden')
    assert.ok(desktopStatsLayout.scrollWidth <= desktopStatsLayout.clientWidth + 1)

    // Re-enter the same persisted session from a genuinely touch-capable
    // browser context. This keeps desktop stream assertions independent from
    // Chromium's mobile emulation while exercising the mobile interaction path.
    const desktopPage = page
    const mobileContext = await browser.newContext({ locale: 'zh-CN',
      storageState: await context.storageState(),
      viewport: { width: 390, height: 844 },
      isMobile: true,
      hasTouch: true,
    })
    await mobileContext.addInitScript(currentSessionId => {
      localStorage.setItem('ternilo.current-session', currentSessionId)
      localStorage.setItem('ternilo.transcript-view', 'compact')
    }, sessionId)
    page = await mobileContext.newPage()
    observePage(page, observations)
    await page.goto(origin, { waitUntil: 'domcontentloaded' })
    await page.getByText('Markdown 正常', { exact: true }).waitFor()
    const mobileProcess = page.locator('[data-turn-process="1"]')
    if (await mobileProcess.getAttribute('aria-expanded') === 'false') await mobileProcess.click()
    const historicalTool = page.locator('[data-tool-call-id="read-fixture"]')
    await historicalTool.locator('[data-tool-call-inspect]').click()
    details = page.getByRole('complementary', { name: '详情' })
    await details.waitFor()

    await page.setViewportSize({ width: 390, height: 844 })
    await page.waitForTimeout(100)
    const dimensions = await page.evaluate(() => ({ viewport: innerWidth, document: document.documentElement.scrollWidth, panel: document.querySelector('.details-panel')?.getBoundingClientRect().width }))
    assert.equal(dimensions.document <= dimensions.viewport, true, JSON.stringify(dimensions))
    assert.equal(dimensions.panel >= dimensions.viewport - 2, true, JSON.stringify(dimensions))
    const mobileRawDisclosure = details.locator('details').filter({ hasText: '原始内容' })
    const disclosureSummary = mobileRawDisclosure.locator('summary')
    const disclosureBox = await disclosureSummary.boundingBox()
    assert.ok(disclosureBox && disclosureBox.height >= 40)
    await tap(page, disclosureSummary)
    const mobileExpander = mobileRawDisclosure.locator('[data-json-expander]').first()
    const mobileExpanderBox = await mobileExpander.boundingBox()
    assert.ok(mobileExpanderBox && mobileExpanderBox.width >= 40 && mobileExpanderBox.height >= 40)
    for (const button of await details.locator('[data-json-inspector] button:visible').all()) {
      const box = await button.boundingBox()
      assert.ok(box && box.height >= 40, `mobile JSON action is ${box?.height ?? 0}px tall`)
    }
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true)

    // 844px-wide phone landscape does not fit a dedicated details column. It
    // must retain the same visible overlay instead of mounting Details at 0px.
    await page.setViewportSize({ width: 844, height: 390 })
    await page.waitForFunction(() => document.querySelector('[data-app-frame]')?.hasAttribute('data-details-overlay'))
    const landscapeDetails = await page.evaluate(() => {
      const frame = document.querySelector('[data-app-frame]')?.getBoundingClientRect()
      const panel = document.querySelector('[data-app-details-column]')?.getBoundingClientRect()
      return frame && panel ? {
        frame: { left: frame.left, top: frame.top, right: frame.right, bottom: frame.bottom },
        panel: { left: panel.left, top: panel.top, right: panel.right, bottom: panel.bottom },
        hit: document.elementFromPoint(innerWidth - 20, 80)?.closest('[data-app-details-column]') !== null,
      } : null
    })
    assert.ok(landscapeDetails)
    for (const edge of ['left', 'top', 'right', 'bottom']) {
      assert.ok(
        Math.abs(landscapeDetails.panel[edge] - landscapeDetails.frame[edge]) <= 1,
        `landscape details ${edge} differs from frame: ${JSON.stringify(landscapeDetails)}`,
      )
    }
    assert.equal(landscapeDetails.hit, true)
    const detailsClose = details.getByRole('button', { name: '关闭详情' })
    const detailsCloseBox = await detailsClose.boundingBox()
    assert.ok(detailsCloseBox && detailsCloseBox.width >= 40 && detailsCloseBox.height >= 40)
    await tap(page, detailsClose)
    await details.waitFor({ state: 'detached' })

    // A dialog portalled from the open mobile drawer owns its own focus scope;
    // the drawer trap must not pull Tab back behind it.
    await page.setViewportSize({ width: 390, height: 844 })
    const sidebarTrigger = page.getByRole('button', { name: '打开侧边栏' })
    await tap(page, sidebarTrigger)
    const sidebar = page.getByRole('complementary', { name: '会话侧边栏' })
    await sidebar.waitFor()
    await page.waitForFunction(() => {
      const rect = document.querySelector('[data-app-sidebar-column]')?.getBoundingClientRect()
      return rect && rect.left >= -1
    })
    await tap(page, sidebar.getByRole('button', { name: '设置', exact: true }))
    const settings = page.getByRole('dialog', { name: '设置' })
    await settings.waitFor()
    await page.keyboard.press('Tab')
    assert.equal(await settings.evaluate(element => element.contains(document.activeElement)), true)
    await tap(page, settings.getByRole('button', { name: '关闭设置' }))
    await settings.waitFor({ state: 'detached' })
    await tap(page, sidebar.getByRole('button', { name: '关闭侧边栏' }))
    await page.waitForFunction(() => {
      const rect = document.querySelector('[data-app-sidebar-column]')?.getBoundingClientRect()
      return rect && rect.right <= 1
    })

    const headerMore = page.getByRole('button', { name: '更多会话操作' })
    const headerMoreBox = await headerMore.boundingBox()
    assert.ok(headerMoreBox && headerMoreBox.width >= 40 && headerMoreBox.height >= 40)
    await tap(page, headerMore)
    const renameItem = page.getByRole('menuitem', { name: '重命名', exact: true })
    await page.waitForTimeout(200)
    const renameBox = await renameItem.boundingBox()
    assert.ok(renameBox && renameBox.height >= 40, `mobile dropdown item is ${renameBox?.height ?? 0}px tall`)
    await page.keyboard.press('Escape')

    const mobileConversation = page.locator('[data-conversation-scroll]')
    const mobileTail = await mobileConversation.evaluate(element => {
      element.scrollTop = element.scrollHeight
      element.dispatchEvent(new Event('scroll'))
      element.dispatchEvent(new TouchEvent('touchstart', { bubbles: true }))
      const tail = element.scrollTop
      element.scrollTop = tail - 4
      element.dispatchEvent(new Event('scroll'))
      element.dispatchEvent(new TouchEvent('touchmove', { bubbles: true }))
      element.scrollTop = tail - 32
      element.dispatchEvent(new Event('scroll'))
      return tail
    })
    await page.waitForTimeout(100)
    assert.ok(
      await mobileConversation.evaluate((element, tail) => element.scrollTop <= tail - 28, mobileTail),
      'the first mobile upward gesture was pulled back to the tail',
    )
    await page.getByRole('button', { name: '回到底部' }).waitFor()

    const statsLine = page.locator('.session-stats-line')
    await statsLine.waitFor()
    await page.waitForFunction(() => !document.querySelector('.session-stats-line')?.hasAttribute('data-truncated'))
    assert.equal(await statsLine.getAttribute('title'), null)
    const statsOverflow = await statsLine.evaluate(element => ({
      overflowX: getComputedStyle(element).overflowX,
      clientWidth: element.clientWidth,
      scrollWidth: element.scrollWidth,
      rows: new Set([...element.querySelectorAll('[data-session-stats-group]')].map(group => Math.round(group.getBoundingClientRect().top))).size,
      groupsInside: [...element.querySelectorAll('[data-session-stats-group]')].every(group => {
        const root = element.getBoundingClientRect()
        const item = group.getBoundingClientRect()
        return item.left >= root.left - 1 && item.right <= root.right + 1
      }),
    }))
    assert.equal(statsOverflow.overflowX, 'visible')
    assert.ok(statsOverflow.scrollWidth <= statsOverflow.clientWidth + 1)
    assert.ok(statsOverflow.rows >= 2 && statsOverflow.rows <= 3, JSON.stringify(statsOverflow))
    assert.equal(statsOverflow.groupsInside, true)
    for (const viewport of [{ width: 390, height: 430 }, { width: 844, height: 390 }]) {
      await page.setViewportSize(viewport)
      await page.waitForTimeout(80)
      const overflow = await page.evaluate(() => document.documentElement.scrollWidth - innerWidth)
      assert.equal(overflow <= 1, true, `${viewport.width}x${viewport.height} overflow ${overflow}`)
    }
    await page.evaluate(() => localStorage.setItem('ternilo.locale', 'en'))
    await page.reload({ waitUntil: 'domcontentloaded' })
    await page.getByRole('button', { name: 'Good response' }).last().waitFor()
    assert.equal(await page.locator('html').getAttribute('lang'), 'en')
    const englishOverflow = await page.evaluate(() => document.documentElement.scrollWidth - innerWidth)
    assert.equal(englishOverflow <= 1, true, `English overflow ${englishOverflow}`)
    console.log('Mobile touch Details and portal focus acceptance passed')
    await mobileContext.close()
    page = desktopPage

    const intentionalFeedbackFailures = observations.failedResponses.filter(failure => (
      failure.includes(`/sessions/${sessionId}/feedback`) && failure.endsWith(': 500')
    ))
    assert.equal(intentionalFeedbackFailures.length, 1, JSON.stringify(observations, null, 2))
    assert.deepEqual(
      observations.failedResponses.filter(failure => !intentionalFeedbackFailures.includes(failure)),
      [],
      JSON.stringify(observations, null, 2),
    )
    const intentionalFeedbackConsole = observations.consoleErrors.filter(message => (
      message.includes('Failed to load resource') && message.includes('500')
    ))
    assert.equal(intentionalFeedbackConsole.length, 1, JSON.stringify(observations, null, 2))
    assert.deepEqual(
      observations.consoleErrors.filter(message => !intentionalFeedbackConsole.includes(message)),
      [],
      JSON.stringify(observations, null, 2),
    )
    assert.deepEqual(observations.failedRequests, [], JSON.stringify(observations, null, 2))
    assert.deepEqual(observations.pageErrors, [], JSON.stringify(observations, null, 2))
    console.log('Chat/Details Salvo Chromium acceptance passed')
  } finally {
    if (browser) await browser.close()
    await stopProcess(ternilo.child)
    await model.close()
    await rm(dataDirectory, { recursive: true, force: true })
  }
})

test('Chat history pages earlier turns without moving the reading anchor', { timeout: 180_000 }, async () => {
  const dataDirectory = await mkdtemp(path.join(tmpdir(), 'ternilo-chat-paging-'))
  const workspacePath = path.join(dataDirectory, 'workspace')
  await mkdir(workspacePath)
  const model = await startModelFixture({ paging: true })
  const ternilo = startTernilo(dataDirectory)
  let browser
  try {
    const origin = await ternilo.origin
    browser = await chromium.launch({
      headless: true,
      executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined,
    })
    const context = await browser.newContext({ locale: 'zh-CN', viewport: { width: 1280, height: 820 } })
    const page = await context.newPage()
    const observations = { pageErrors: [], consoleErrors: [], failedRequests: [], failedResponses: [] }
    observePage(page, observations)
    await page.goto(origin, { waitUntil: 'networkidle' })
    const workspace = await api(page, '/workspaces', { method: 'POST', body: { path: workspacePath } })
    const session = await api(page, '/sessions', { method: 'POST', body: {
      workspace_id: workspace.workspace_id,
      agent_preset: 'standard',
    } })
    const sessionId = session.identity?.session_id ?? session.session_id
    await api(page, '/credentials', { method: 'POST', body: { name: 'TERNILO_PROVIDER_CHAT_PAGING_API_KEY', value: 'fixture-key' } })
    await api(page, '/providers', { method: 'POST', body: {
      id: 'chat-paging', display_name: 'Chat Paging Fixture', base_url: model.baseUrl,
      protocol: 'openai-responses', api_key_ref: 'TERNILO_PROVIDER_CHAT_PAGING_API_KEY',
      defaults: { context_window: 128000, max_output_tokens: 8192 },
      models: [{ id: 'fixture-model', display_name: 'Fixture Model', settings: { mode: 'inherit' } }],
      timeout_ms: 30000, max_attempts: 1, retry_base_delay_ms: 50,
    } })
    await api(page, `/sessions/${encodeURIComponent(sessionId)}`, { method: 'PATCH', body: {
      model: { provider: 'named_provider', provider_id: 'chat-paging', model: 'fixture-model' },
    } })
    const pagingTurnCount = 171
    const apiToken = await page.evaluate(() => window.__TERNILO_BOOT__?.apiToken)
    // Populate durable history before opening its live view; this test covers paging existing turns.
    await page.goto('about:blank')
    for (let index = 0; index < pagingTurnCount; index++) {
      const response = await page.request.post(`${origin}/api/v1/sessions/${encodeURIComponent(sessionId)}/turns`, {
        headers: apiToken ? { authorization: `Bearer ${apiToken}` } : {},
        data: { run_id: `paging-${index}`, input: `分页提示 ${index}`, attachments: [] },
      })
      assert.equal(response.ok(), true, `history turn ${index}: ${response.status()} ${await response.text()}`)
      const outcome = await response.json()
      assert.equal(outcome.answer, `ternilo: 分页提示 ${index}`)
    }

    await page.goto(origin, { waitUntil: 'domcontentloaded' })
    await page.evaluate(currentSessionId => localStorage.setItem('ternilo.current-session', currentSessionId), sessionId)
    await page.reload({ waitUntil: 'domcontentloaded' })
    await page.getByText('ternilo: 分页提示 170', { exact: true }).waitFor()
    assert.equal(await page.locator('[data-chat-turn]').count(), 160)
    const pagingScroll = page.locator('[data-conversation-scroll]')
    await pagingScroll.evaluate(element => { element.scrollTop = 0; element.dispatchEvent(new Event('scroll')) })
    const anchorBefore = await page.locator('[data-chat-anchor-key]').first().evaluate(row => {
      const scrollport = row.closest('[data-conversation-scroll]')
      return { key: row.getAttribute('data-chat-anchor-key'), top: row.getBoundingClientRect().top - scrollport.getBoundingClientRect().top }
    })
    await page.getByRole('button', { name: '加载更早' }).click()
    assert.equal(await page.locator('[data-chat-turn]').count(), pagingTurnCount)
    const anchorAfter = await page.locator(`[data-chat-anchor-key="${anchorBefore.key}"]`).evaluate((row, expected) => {
      const scrollport = row.closest('[data-conversation-scroll]')
      return Math.abs(row.getBoundingClientRect().top - scrollport.getBoundingClientRect().top - expected)
    }, anchorBefore.top)
    assert.equal(anchorAfter <= 2, true, `paging anchor moved ${anchorAfter}px`)
    assert.deepEqual(observations, { pageErrors: [], consoleErrors: [], failedRequests: [], failedResponses: [] })
    console.log('Chat paging Salvo Chromium acceptance passed')
  } finally {
    if (browser) await browser.close()
    await stopProcess(ternilo.child)
    await model.close()
    await rm(dataDirectory, { recursive: true, force: true })
  }
})
