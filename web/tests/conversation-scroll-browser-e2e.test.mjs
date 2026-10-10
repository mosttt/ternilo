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
import { freePort, initializeServer, serverRequest, startProcess, stopProcess as stopApplication, waitForHttp } from './platform-e2e-fixture.mjs'
import { localApi, until } from './model-device-fixture.mjs'

const repository = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..')
const binary = process.env.TERNILO_E2E_NODE_BINARY ?? path.join(repository, 'target/debug/ternilo')

function startTernilo(directory) {
  const child = spawn(binary, ['serve', '--listen', '127.0.0.1:0', '--data-dir', directory], {
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
    child.once('error', reject)
    child.once('exit', code => reject(new Error(`Local exited ${code}: ${diagnostics}`)))
  })
  return { child, origin, diagnostics: () => diagnostics }
}

async function stopProcess(child) {
  if (child.exitCode !== null) return
  child.kill('SIGINT')
  let timer
  await Promise.race([
    new Promise(resolve => child.once('exit', resolve)),
    new Promise(resolve => { timer = setTimeout(() => { child.kill('SIGKILL'); resolve() }, 5_000) }),
  ])
  clearTimeout(timer)
}

function sse(payload) { return `data: ${JSON.stringify(payload)}\n\n` }

function completed(text, reasoning = '') {
  return sse({ type: 'response.completed', response: {
    status: 'completed',
    output: [...(reasoning ? [{ type: 'reasoning', summary: [{ type: 'summary_text', text: reasoning }] }] : []), { type: 'message', role: 'assistant', content: [{ type: 'output_text', text }] }],
    usage: { input_tokens: 20, output_tokens: 40, input_tokens_details: { cached_tokens: 0 }, output_tokens_details: { reasoning_tokens: 7 } },
  } })
}

async function startModelFixture() {
  const streams = []
  const waiters = new Map()
  const server = createServer((request, response) => {
    if (request.method !== 'POST' || request.url !== '/v1/responses') {
      response.writeHead(404).end()
      return
    }
    const chunks = []
    request.on('data', chunk => chunks.push(chunk))
    request.on('end', () => {
      const body = JSON.parse(Buffer.concat(chunks).toString('utf8'))
      response.writeHead(200, { 'content-type': 'text/event-stream', 'cache-control': 'no-cache' })
      if (body.instructions?.includes('You name software-agent conversations')) {
        response.end(completed('滚动意图验收'))
        return
      }
      response.flushHeaders()
      let text = ''
      let reasoning = ''
      let timer
      const stream = {
        body,
        reason(delta) {
          assert.equal(body.reasoning?.summary, 'auto')
          reasoning += delta
          response.write(sse({ type: 'response.reasoning_summary_text.delta', output_index: 0, summary_index: 0, item_id: 'rs-fixture', delta }))
        },
        append(delta) {
          text += delta
          response.write(sse({ type: 'response.output_text.delta', output_index: 0, content_index: 0, delta }))
        },
        pulse() {
          timer = setInterval(() => stream.append(' ·'), 12)
        },
        stopPulse() { clearInterval(timer) },
        complete() { clearInterval(timer); if (!response.writableEnded) response.end(completed(text, reasoning)) },
      }
      response.on('close', () => clearInterval(timer))
      const index = streams.push(stream) - 1
      waiters.get(index)?.(stream)
      waiters.delete(index)
    })
  })
  await new Promise((resolve, reject) => { server.once('error', reject); server.listen(0, '127.0.0.1', resolve) })
  return {
    baseUrl: `http://127.0.0.1:${server.address().port}/v1`,
    next: () => streams.length,
    stream(index) { return streams[index] ? Promise.resolve(streams[index]) : new Promise(resolve => waiters.set(index, resolve)) },
    async close() {
      streams.forEach(stream => stream.complete())
      server.closeAllConnections()
      await new Promise((resolve, reject) => server.close(error => error ? reject(error) : resolve()))
    },
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
    if (response.status === 204) { await response.arrayBuffer(); return null }
    return response.json()
  }, { endpoint, init })
}

function geometry(page) {
  return page.locator('[data-conversation-scroll]').evaluate(element => ({
    top: element.scrollTop,
    floor: element.scrollHeight - element.clientHeight,
    distance: element.scrollHeight - element.clientHeight - element.scrollTop,
  }))
}

async function atTail(page) {
  try {
    await page.waitForFunction(() => {
      const element = document.querySelector('[data-conversation-scroll]')
      return element && element.scrollHeight - element.clientHeight - element.scrollTop < 1
    }, undefined, { timeout: 5_000 })
  } catch (cause) {
    throw new Error(`conversation did not reach tail: ${JSON.stringify(await geometry(page))}`, { cause })
  }
}

async function stableAt(page, top, label) {
  await page.waitForTimeout(160)
  const actual = await geometry(page)
  assert.ok(Math.abs(actual.top - top) < 2, `${label}: expected ${top}; got ${JSON.stringify(actual)}`)
}

function assistantText(page, text) {
  return page.locator('article[data-role="assistant"]').last().filter({ hasText: text }).waitFor()
}

async function startTurn(page, model, prompt, inspectWaiting) {
  const index = model.next()
  const input = page.getByRole('textbox', { name: '输入任务' })
  await input.fill(prompt)
  await page.getByRole('button', { name: '发送', exact: true }).click()
  const stream = await model.stream(index)
  if (inspectWaiting) await inspectWaiting()
  stream.append(Array.from({ length: 35 }, (_, index) => `滚动段落 ${index + 1}：保留读者的位置。\n\n`).join(''))
  await assistantText(page, '滚动段落 35')
  await atTail(page)
  return stream
}

test('Local streaming respects small reader gestures and explicit tail resume', { timeout: 180_000 }, async t => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-conversation-scroll-'))
  const workspacePath = path.join(directory, 'workspace')
  await mkdir(workspacePath)
  const model = await startModelFixture()
  const local = startTernilo(directory)
  let browser
  const observations = { pageErrors: [], consoleErrors: [], httpErrors: [], failedRequests: [] }
  try {
    const origin = await local.origin
    const hashes = {}
    for (const asset of ['app.js', 'app.css']) {
      const response = await fetch(`${origin}/assets/${asset}`)
      assert.equal(response.status, 200)
      hashes[asset] = createHash('sha256').update(Buffer.from(await response.arrayBuffer())).digest('hex')
      const expected = createHash('sha256').update(await readFile(path.join(repository, 'web/dist/assets', asset))).digest('hex')
      assert.equal(hashes[asset], expected, `Local must embed the current ${asset}`)
    }
    t.diagnostic(`Local embedded assets: ${JSON.stringify(hashes)}`)
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })

    for (const mobile of [false, true]) {
      await t.test(mobile ? 'small touch drags stay detached while streaming' : 'small wheel, resize, history and resume', async subtest => {
        const context = await browser.newContext({ locale: 'zh-CN',
          viewport: mobile ? { width: 390, height: 844 } : { width: 1280, height: 820 },
          isMobile: mobile, hasTouch: mobile,
        })
        const page = await context.newPage()
        page.on('pageerror', error => observations.pageErrors.push(error.message))
        page.on('console', message => { if (message.type() === 'error') observations.consoleErrors.push(message.text()) })
        page.on('response', response => { if (response.status() >= 400) observations.httpErrors.push(`${response.status()} ${response.request().method()} ${response.url()}`) })
        page.on('requestfailed', request => {
          if (request.failure()?.errorText !== 'net::ERR_ABORTED') observations.failedRequests.push(`${request.method()} ${request.url()}: ${request.failure()?.errorText}`)
        })
        let stream
        try {
          await page.goto(origin, { waitUntil: 'networkidle' })
          const workspace = await api(page, '/workspaces', { method: 'POST', body: { path: workspacePath } })
          const session = await api(page, '/sessions', { method: 'POST', body: { workspace_id: workspace.workspace_id, agent_preset: 'standard' } })
          const sessionId = session.identity.session_id
          if (!mobile) {
            await api(page, '/credentials', { method: 'POST', body: { name: 'TERNILO_PROVIDER_SCROLL_API_KEY', value: 'fixture-key' } })
            await api(page, '/providers', { method: 'POST', body: {
              id: 'scroll-fixture', display_name: 'Scroll Fixture', base_url: model.baseUrl,
              protocol: 'openai-responses', api_key_ref: 'TERNILO_PROVIDER_SCROLL_API_KEY',
              defaults: { context_window: 128000, max_output_tokens: 8192, reasoning: { default_effort: 'high', efforts: { high: 'high' } } },
              models: [{ id: 'fixture-model', display_name: 'Fixture Model', settings: { mode: 'inherit' } }],
              timeout_ms: 120000, max_attempts: 1, retry_base_delay_ms: 50,
            } })
          }
          await api(page, `/sessions/${encodeURIComponent(sessionId)}`, { method: 'PATCH', body: {
            title: mobile ? '触屏滚动验收' : '桌面滚动验收',
            model: { provider: 'named_provider', provider_id: 'scroll-fixture', model: 'fixture-model' },
          } })
          await page.evaluate(id => localStorage.setItem('ternilo.current-session', id), sessionId)
          await page.reload({ waitUntil: 'domcontentloaded' })
          stream = await startTurn(page, model, '建立已有对话')
          stream.complete()
          await page.getByRole('button', { name: '发送', exact: true }).waitFor()
          const pendingIndex = model.next()
          await page.getByRole('textbox', { name: '输入任务' }).fill(Array.from({ length: 16 }, (_, index) => `发送后等待输出的长消息 ${index + 1}`).join('\n'))
          await page.getByRole('button', { name: '发送', exact: true }).click()
          stream = await model.stream(pendingIndex)
          await page.locator('article[data-role="user"]').last().filter({ hasText: '发送后等待输出的长消息 16' }).waitFor()
          await page.waitForTimeout(600)
          await atTail(page)
          assert.equal(await page.getByRole('button', { name: '回到底部', exact: true }).count(), 0)
          if (mobile) assert.equal(await page.getByRole('textbox', { name: '输入任务' }).evaluate(element => document.activeElement === element), false)
          if (process.env.TERNILO_E2E_ARTIFACT_DIR) {
            await mkdir(process.env.TERNILO_E2E_ARTIFACT_DIR, { recursive: true })
            await page.screenshot({ path: path.join(process.env.TERNILO_E2E_ARTIFACT_DIR, `before-output-${mobile ? 'touch' : 'wheel'}.png`) })
          }
          stream.reason('本地 Responses 摘要。')
          await page.locator('[data-reasoning-summary]').filter({ hasText: '本地 Responses 摘要。' }).waitFor()
          await atTail(page)
          stream.append('现在才返回输出。')
          await assistantText(page, '现在才返回输出。')
          stream.complete()
          await page.getByRole('button', { name: '发送', exact: true }).waitFor()
          stream = await startTurn(page, model, mobile ? '检查触屏小步滚动' : '检查慢速滚轮')
          const scroll = page.locator('[data-conversation-scroll]')
          const samples = []
          if (mobile) {
            const box = await scroll.boundingBox()
            assert.ok(box)
            await page.touchscreen.tap(box.x + box.width / 2, box.y + Math.min(box.height * 0.4, 240))
            stream.append('普通点击后继续跟随。\n\n')
            await assistantText(page, '普通点击后继续跟随。')
            await atTail(page)
          }
          stream.append('实时增量')
          await assistantText(page, '实时增量')
          await atTail(page)
          if (!mobile) await scroll.hover()
          const initial = await geometry(page)
          stream.pulse()
          if (mobile) {
            const box = await scroll.boundingBox()
            assert.ok(box)
            const cdp = await context.newCDPSession(page)
            const x = Math.round(box.x + box.width / 2)
            const y = Math.round(box.y + Math.min(box.height * 0.4, 240))
            await cdp.send('Input.dispatchTouchEvent', { type: 'touchStart', touchPoints: [{ x, y }] })
            for (let step = 1; step <= 18; step++) {
              await cdp.send('Input.dispatchTouchEvent', { type: 'touchMove', touchPoints: [{ x, y: y + step * 2 }] })
              await page.waitForTimeout(45)
              samples.push(await geometry(page))
            }
            await cdp.send('Input.dispatchTouchEvent', { type: 'touchEnd', touchPoints: [] })
            await cdp.detach()
          } else {
            for (let step = 0; step < 6; step++) {
              await page.mouse.wheel(0, -4)
              await page.waitForTimeout(75)
              samples.push(await geometry(page))
            }
          }
          stream.stopPulse()
          subtest.diagnostic(`${mobile ? 'touch' : 'wheel'} initial=${JSON.stringify(initial)} samples=${JSON.stringify(samples)}`)
          const reader = await geometry(page)
          assert.ok(initial.top - reader.top >= 12, `small ${mobile ? 'touch' : 'wheel'} gestures snapped to tail: ${JSON.stringify({ initial, reader, samples })}`)
          await page.getByRole('button', { name: '回到底部', exact: true }).waitFor()
          await scroll.evaluate(element => element.dispatchEvent(new Event('scroll')))
          await stableAt(page, reader.top, 'passive scroll must preserve pause')
          stream.append('\n\n暂停期间的新段落。\n\n')
          await assistantText(page, '暂停期间的新段落。')
          await stableAt(page, reader.top, 'stream growth must preserve pause')
          await page.setViewportSize(mobile ? { width: 390, height: 760 } : { width: 1280, height: 750 })
          await stableAt(page, reader.top, 'viewport resize must preserve pause')
          if (mobile) {
            await page.getByRole('button', { name: '回到底部', exact: true }).click()
            await atTail(page)
            stream.append('触屏恢复跟随。\n\n')
            await assistantText(page, '触屏恢复跟随。')
            await atTail(page)
            stream.complete()
            await page.getByRole('button', { name: '发送', exact: true }).waitFor()
            return
          }

          await scroll.hover()
          await page.mouse.wheel(0, 10000)
          await atTail(page)
          stream.append('向下滚到底后继续跟随。\n\n')
          await assistantText(page, '向下滚到底后继续跟随。')
          await atTail(page)

          await page.mouse.wheel(0, -4)
          await page.waitForTimeout(100)
          const nearTail = await geometry(page)
          assert.ok(nearTail.distance >= 2 && nearTail.distance < 25, `expected near-tail paused position: ${JSON.stringify(nearTail)}`)
          await page.getByRole('tab', { name: /轨迹/ }).click()
          await page.getByRole('tab', { name: '对话', exact: true }).click()
          await stableAt(page, nearTail.top, 'view restore must retain near-tail pause')
          await page.getByRole('button', { name: '回到底部', exact: true }).waitFor()
          await page.locator('[data-sidebar-new-session]').click()
          await page.locator('[data-new-session-hero]').waitFor()
          await page.locator('[data-sidebar-session-button]').filter({ hasText: '桌面滚动验收' }).click()
          await assistantText(page, '向下滚到底后继续跟随。')
          await stableAt(page, nearTail.top, 'session restore must retain near-tail pause')
          stream.append('恢复历史后仍保留阅读位置。\n\n')
          await assistantText(page, '恢复历史后仍保留阅读位置。')
          await stableAt(page, nearTail.top, 'restored pause must survive streamed growth')

          await page.getByRole('button', { name: '回到底部', exact: true }).click()
          await atTail(page)
          stream.append('按钮恢复跟随。\n\n')
          await assistantText(page, '按钮恢复跟随。')
          await atTail(page)
          await page.locator('[data-sidebar-new-session]').click()
          await page.locator('[data-new-session-hero]').waitFor()
          stream.append(Array.from({ length: 8 }, (_, index) => `后台完成段落 ${index + 1}。\n\n`).join(''))
          stream.complete()
          await page.waitForFunction(async id => {
            const token = window.__TERNILO_BOOT__?.apiToken ?? ''
            const response = await fetch(`/api/v1/sessions/${encodeURIComponent(id)}/events`, {
              headers: token ? { authorization: `Bearer ${token}` } : {},
            })
            const events = await response.json()
            return events.some(event => event.type === 'turn_finished')
          }, sessionId)
          await page.locator('[data-sidebar-session-button]').filter({ hasText: '桌面滚动验收' }).click()
          await assistantText(page, '后台完成段落 8')
          await page.getByRole('button', { name: '发送', exact: true }).waitFor()
          await atTail(page)
          await scroll.hover()
          await page.mouse.wheel(0, -120)
          await page.getByRole('button', { name: '回到底部', exact: true }).waitFor()
          await page.getByRole('tab', { name: /轨迹/ }).click()
          stream = await startTurn(page, model, '从轨迹视图主动发送', async () => {
            assert.equal(await page.getByRole('tab', { name: '对话', exact: true }).getAttribute('aria-selected'), 'true')
            await atTail(page)
            const waiting = await geometry(page)
            await page.setViewportSize({ width: 1280, height: 690 })
            await stableAt(page, waiting.top, 'waiting for first output must not follow resize')
            await scroll.evaluate(element => element.dispatchEvent(new Event('scroll')))
            await page.waitForTimeout(1100)
            await stableAt(page, waiting.top, 'waiting status must not restart following')
          })
          stream.append('发送恢复跟随。\n\n')
          await assistantText(page, '发送恢复跟随。')
          await atTail(page)
          stream.complete()
          await page.getByRole('button', { name: '发送', exact: true }).waitFor()

          await page.waitForTimeout(200)
          const idle = await geometry(page)
          await scroll.evaluate(element => {
            const descriptor = Object.getOwnPropertyDescriptor(Element.prototype, 'scrollTop')
            window.conversationScrollWrites = []
            window.restoreConversationScrollTrace = () => Object.defineProperty(Element.prototype, 'scrollTop', descriptor)
            Object.defineProperty(Element.prototype, 'scrollTop', { ...descriptor, set(value) {
              if (this === element) window.conversationScrollWrites.push(value)
              descriptor.set.call(this, value)
            } })
          })
          await page.setViewportSize({ width: 1280, height: 650 })
          await page.waitForTimeout(200)
          const resizedIdle = await geometry(page)
          // Native anchoring may adjust the position; the application must not scroll or follow the tail.
          const idleWrites = await page.evaluate(() => {
            window.restoreConversationScrollTrace()
            return window.conversationScrollWrites
          })
          assert.deepEqual(idleWrites, [], 'completed turn must not initiate scrolling on viewport resize')
          assert.ok(resizedIdle.distance > idle.distance + 10, `idle resize must leave the tail behind: ${JSON.stringify({ idle, resizedIdle })}`)
          await page.getByRole('button', { name: '回到底部', exact: true }).waitFor()
          const next = model.next()
          await page.getByRole('textbox', { name: '输入任务' }).fill('等待输出时先向上阅读')
          await page.getByRole('button', { name: '发送', exact: true }).click()
          stream = await model.stream(next)
          await atTail(page)
          await scroll.hover()
          await page.mouse.wheel(0, -4)
          await page.waitForTimeout(100)
          const waitingReader = await geometry(page)
          stream.append('用户等待期间上滚，首次输出仍保留阅读位置。\n\n')
          await assistantText(page, '用户等待期间上滚')
          await stableAt(page, waitingReader.top, 'first output must respect upward reading during wait')
          stream.complete()
          await page.getByRole('button', { name: '发送', exact: true }).waitFor()

          const settling = model.next()
          await page.getByRole('textbox', { name: '输入任务' }).fill('完成后保留阅读位置')
          await page.getByRole('button', { name: '发送', exact: true }).click()
          stream = await model.stream(settling)
          stream.append(['```rust\nfn settled() {}\n```\n\n', ...Array.from({ length: 30 }, (_, index) => `## 完成标题 ${index + 1}\n\n完成段落 ${index + 1}：结束后不能移动。\n\n- 完成项 ${index + 1}\n\n`)].join(''))
          await assistantText(page, '完成段落 30')
          await atTail(page)
          await scroll.hover()
          await page.mouse.wheel(0, -1200)
          await page.waitForTimeout(160)
          const readingAnchor = () => scroll.evaluate(element => {
            const viewport = element.getBoundingClientRect()
            const answer = [...element.querySelectorAll('article[data-role="assistant"]')].at(-1)
            const row = [...answer.querySelectorAll('.markdown-body p')].find(item => item.getBoundingClientRect().bottom > viewport.top)
            return { text: row?.textContent, top: row ? row.getBoundingClientRect().top - viewport.top : null, scrollTop: element.scrollTop }
          })
          const streamingAnchor = await readingAnchor()
          stream.complete()
          await page.getByRole('button', { name: '发送', exact: true }).waitFor()
          await page.waitForTimeout(300)
          const settledAnchor = await readingAnchor()
          assert.ok(streamingAnchor.text && settledAnchor.text === streamingAnchor.text && Math.abs(settledAnchor.top - streamingAnchor.top) < 2,
            `settling a finished answer must not move a paused reader: ${JSON.stringify({ streamingAnchor, settledAnchor })}`)
        } finally {
          stream?.complete()
          if (process.env.TERNILO_E2E_ARTIFACT_DIR) {
            await mkdir(process.env.TERNILO_E2E_ARTIFACT_DIR, { recursive: true })
            await page.screenshot({ path: path.join(process.env.TERNILO_E2E_ARTIFACT_DIR, `conversation-scroll-${mobile ? 'touch' : 'wheel'}.png`), timeout: 5000 }).catch(error => subtest.diagnostic(`Cleanup screenshot: ${error.message}`))
          }
          await context.close()
        }
      })
    }
    t.diagnostic(`Browser observations: ${JSON.stringify(observations)}`)
    assert.deepEqual(observations, { pageErrors: [], consoleErrors: [], httpErrors: [], failedRequests: [] })
  } finally {
    await browser?.close()
    await stopProcess(local.child)
    await model.close()
    await rm(directory, { recursive: true, force: true })
  }
})

test('Server account and platform Responses follow submitted messages before output and display requested summaries', { timeout: 240000 }, async t => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-server-send-scroll-'))
  const processes = [], errors = []
  const model = await startModelFixture()
  let browser
  try {
    const server = await initializeServer({ directory: path.join(directory, 'server'), origin: `http://127.0.0.1:${await freePort()}`, mode: 'single_user', environment: { XDG_STATE_HOME: path.join(directory, 'state') } })
    processes.push(server)
    const tenantId = server.owner.session.personal_tenant_id
    const request = (resource, options = {}) => serverRequest(server.origin, resource, { token: server.owner.session.access_token, tenantId, ...options })
    const enrollment = await request(`/tenants/${tenantId}/my-computer-enrollments`, { body: { name: 'scroll', project_id: null, ttl_seconds: 600 } })
    const enrolledComputerId = enrollment.enrollment.executor_id
    const credential = await request('/enrollments/consume', { body: { token: enrollment.enrollment.token } })
    const origin = `http://127.0.0.1:${await freePort()}`
    const node = startProcess(binary, ['serve', '--listen', new URL(origin).host, '--data-dir', path.join(directory, 'node'), '--node-id', enrolledComputerId, '--gateway-url', `${server.origin.replace('http:', 'ws:')}/api/v1/executors/connect`, '--allow-insecure-gateway'], { TERNILO_LOCAL_TOKEN: credential.credential.token })
    processes.push(node)
    await waitForHttp(origin, node)
    const local = await localApi(origin)
    const folder = path.join(directory, 'workspace')
    await mkdir(folder)
    const workspace = await local('/workspaces', { body: { path: folder } })
    const profile = {
      id: 'summary-fixture', display_name: 'Summary fixture', base_url: model.baseUrl, protocol: 'openai-responses', api_key_ref: null,
      defaults: { context_window: 128000, max_output_tokens: 8192, reasoning: { default_effort: 'high', efforts: { high: 'high' } } },
      models: [{ id: 'fixture-model', settings: { mode: 'inherit' } }], timeout_ms: 120000, max_attempts: 1, retry_base_delay_ms: 50,
    }
    await request('/providers', { body: profile })
    await request('/admin/models/providers', { body: { profile, enabled: true } })
    await request('/admin/models/publications', { body: { model_id: 'summary-model', display_name: 'Summary model', provider_id: profile.id, upstream_model: 'fixture-model', enabled: true } })
    const grant = await request('/admin/models/grants', { body: { name: 'Summary budget', subject: { kind: 'user', id: server.owner.session.user.user_id }, model_ids: ['summary-model'], monthly_tokens: 2000000, max_concurrent_requests: 2, allow_resource_sharing: false } })
    for (const target of [origin, server.origin]) {
      for (const asset of ['app.js', 'app.css']) {
        const response = await fetch(`${target}/assets/${asset}`)
        assert.equal(response.status, 200)
        assert.equal(createHash('sha256').update(Buffer.from(await response.arrayBuffer())).digest('hex'), createHash('sha256').update(await readFile(path.join(repository, 'web/dist/assets', asset))).digest('hex'))
      }
    }
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
    for (const scope of ['account', 'platform']) {
      for (const width of [1280, 390, 320]) {
        const previousIds = new Set((await request('/state')).sessions.map(item => item.identity.session_id))
        await local('/sessions', { body: { workspace_id: workspace.workspace_id, agent_preset: 'standard' } })
        const state = await until(() => request('/state'), value => value.sessions.some(item => !previousIds.has(item.identity.session_id)), 'session synchronized')
        const sessionId = state.sessions.find(item => !previousIds.has(item.identity.session_id)).identity.session_id
        const selection = scope === 'account'
          ? { provider: 'account_provider', owner_user_id: server.owner.session.user.user_id, provider_id: profile.id, model: 'fixture-model' }
          : { provider: 'platform_model', model_id: 'summary-model', grant_id: grant.grant_id }
        await request(`/sessions/${sessionId}`, { method: 'PATCH', body: { title: `${scope}-${width}`, model: selection } })
        const context = await browser.newContext({ locale: 'zh-CN', viewport: { width: 1280, height: 820 }, isMobile: width < 600, hasTouch: width < 600 })
        const page = await context.newPage()
        page.setDefaultTimeout(15000)
        page.on('pageerror', error => errors.push(error.message))
        page.on('console', message => { if (message.type() === 'error') errors.push(message.text()) })
        page.on('response', response => { if (response.status() >= 400) errors.push(`${response.status()} ${response.url()}`) })
        page.on('requestfailed', failed => { if (failed.failure()?.errorText !== 'net::ERR_ABORTED') errors.push(`${failed.url()}: ${failed.failure()?.errorText}`) })
        let stream
        try {
          await page.goto(server.origin)
          await page.getByLabel('用户名', { exact: true }).fill(server.owner.username)
          await page.getByLabel('密码', { exact: true }).fill(server.owner.password)
          await page.getByRole('button', { name: '登录', exact: true }).click()
          const row = page.locator(`[data-sidebar-session-row][data-session-id="${sessionId}"]`)
          await row.waitFor({ state: 'attached' })
          if (!await row.isVisible()) await page.locator('[data-sidebar-workspace-button]').first().click()
          await row.locator('[data-sidebar-session-button]').click()
          await page.setViewportSize({ width, height: 820 })
          stream = await startTurn(page, model, '建立对话历史')
          stream.complete()
          await page.getByRole('button', { name: '发送', exact: true }).waitFor()
          const index = model.next()
          await page.getByRole('textbox', { name: '输入任务' }).fill('首个输出前就应当显示最后一条消息\n并且完整显示消息下方的操作栏')
          await page.getByRole('button', { name: '发送', exact: true }).click()
          stream = await model.stream(index)
          await page.locator('article[data-role="user"]').last().filter({ hasText: '首个输出前' }).waitFor()
          await page.waitForTimeout(600)
          await atTail(page)
          assert.equal(await page.getByRole('button', { name: '回到底部', exact: true }).count(), 0)
          assert.deepEqual(stream.body.reasoning, { effort: 'high', summary: 'auto' })
          if (width < 600) assert.equal(await page.getByRole('textbox', { name: '输入任务' }).evaluate(element => document.activeElement === element), false)
          if (process.env.TERNILO_E2E_ARTIFACT_DIR) await page.screenshot({ path: path.join(process.env.TERNILO_E2E_ARTIFACT_DIR, `${scope}-${width}-before-output.png`) })
          stream.reason('这是上游返回的可展示摘要。')
          await page.locator('[data-reasoning-summary]').filter({ hasText: '这是上游返回的可展示摘要。' }).waitFor()
          await atTail(page)
          stream.append('摘要和回答分别显示。')
          await assistantText(page, '摘要和回答分别显示。')
          stream.complete()
          await page.getByRole('button', { name: '发送', exact: true }).waitFor()
          const events = await until(() => request(`/sessions/${sessionId}/events`), values => values.filter(event => event.type === 'turn_finished').length === 2, 'both turns complete')
          const answer = events.filter(event => event.type === 'assistant_message').at(-1).response
          assert.equal(answer.reasoning_content, '这是上游返回的可展示摘要。')
          assert.equal(answer.usage.reasoning_tokens, 7)
          await page.locator('[data-chat-turn]').last().locator('[data-turn-process][aria-expanded="false"]').click()
          await page.locator('[data-reasoning-row]').last().getByRole('button').click()
          await page.locator('[data-reasoning-body]').filter({ hasText: '这是上游返回的可展示摘要。' }).waitFor()
          if (process.env.TERNILO_E2E_ARTIFACT_DIR) await page.screenshot({ path: path.join(process.env.TERNILO_E2E_ARTIFACT_DIR, `${scope}-${width}-summary.png`) })
          t.diagnostic(`${scope} ${width}px: before-output tail, summary request, live display and persisted usage passed`)
        } finally {
          stream?.complete()
          await context.close()
        }
      }
    }
    assert.deepEqual(errors, [])
  } finally {
    await browser?.close()
    for (const process of processes.reverse()) await stopApplication(process)
    await model.close()
    await rm(directory, { recursive: true, force: true })
  }
})
