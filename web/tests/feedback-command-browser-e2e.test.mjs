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
      const body = JSON.parse(Buffer.concat(chunks).toString('utf8'))
      received.push(body)
      const titleRequest = String(body.instructions ?? '').startsWith('You name software-agent conversations.')
      const answer = titleRequest ? '可见对话' : `READY ${conversationRequests(received).length}`
      response.writeHead(200, { 'content-type': 'text/event-stream', 'x-request-id': `feedback-${received.length}` })
      response.end([
        sse({ type: 'response.output_text.delta', output_index: 0, content_index: 0, delta: answer }),
        sse({
          type: 'response.completed',
          response: {
            status: 'completed',
            output: [{ type: 'message', role: 'assistant', content: [{ type: 'output_text', text: answer }] }],
            usage: { input_tokens: 12, output_tokens: 2, input_tokens_details: { cached_tokens: 0 } },
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

async function api(page, pathValue, method = 'GET', bodyValue) {
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

async function configureModel(page, baseUrl) {
  await api(page, '/credentials', 'POST', { name: 'TERNILO_PROVIDER_FEEDBACK_API_KEY', value: 'fixture-key' })
  await api(page, '/providers', 'POST', {
    id: 'feedback-fixture', display_name: 'Feedback Fixture', base_url: baseUrl,
    protocol: 'openai-responses', api_key_ref: 'TERNILO_PROVIDER_FEEDBACK_API_KEY',
    defaults: { context_window: 128000, max_output_tokens: 4096 },
    models: [{ id: 'feedback-model', display_name: 'Feedback Model', settings: { mode: 'inherit' } }],
    timeout_ms: 120000, max_attempts: 1, retry_base_delay_ms: 50,
  })
  const sessionId = await page.evaluate(() => localStorage.getItem('ternilo.current-session'))
  await api(page, `/sessions/${encodeURIComponent(sessionId)}`, 'PATCH', {
    model: { provider: 'named_provider', provider_id: 'feedback-fixture', model: 'feedback-model' },
  })
  await page.reload({ waitUntil: 'domcontentloaded' })
  await page.getByRole('button', { name: /Feedback Model/ }).waitFor()
  return sessionId
}

test('/feedback is a durable localized command and never enters the model history', { timeout: 120_000 }, async () => {
  const dataDirectory = await mkdtemp(path.join(tmpdir(), 'ternilo-feedback-command-'))
  const workspace = path.join(dataDirectory, 'workspace')
  await mkdir(workspace)
  const model = await startModelFixture()
  const ternilo = startTernilo(dataDirectory)
  let browser
  try {
    const origin = await ternilo.origin
    browser = await chromium.launch({ headless: true })
    const page = await browser.newPage({ viewport: { width: 1280, height: 820 } })
    const pageErrors = []
    page.on('pageerror', error => pageErrors.push(error.message))
    await page.goto(origin, { waitUntil: 'networkidle' })
    await chooseWorkspace(page, workspace)
    const sessionId = await configureModel(page, model.baseUrl)
    let input = page.getByRole('textbox', { name: '输入任务' })

    await input.fill('建立可见对话')
    await page.getByRole('button', { name: '发送' }).click()
    await page.getByText('READY 1', { exact: true }).waitFor({ timeout: 15_000 }).catch(async error => {
      throw new Error(`${error.message}\nreceived=${JSON.stringify(model.received)}\npage=${(await page.locator('body').innerText()).slice(-3000)}\nserver=${ternilo.diagnostics()}`)
    })
    assert.equal(conversationRequests(model.received).length, 1)

    await input.fill('/feed')
    const command = page.locator('[data-composer-menu] [role="option"]').filter({ hasText: '/feedback' })
    await command.waitFor()
    assert.match(await command.textContent(), /不会发送给模型/)
    await input.fill('/feedback the diff view is unreadable')
    await page.getByRole('button', { name: '发送' }).click()
    const accepted = page.locator('[data-chat-event="command"]').filter({ hasText: '反馈已记录' }).last()
    await accepted.waitFor()
    assert.equal(conversationRequests(model.received).length, 1, 'the feedback command must not start or steer a model run')

    await input.fill('/feedback')
    await page.getByRole('button', { name: '发送' }).click()
    const rejected = page.locator('[data-chat-event="command"]').filter({ hasText: '需要填写反馈内容' }).last()
    await rejected.waitFor()
    assert.equal(conversationRequests(model.received).length, 1)

    const events = await api(page, `/sessions/${encodeURIComponent(sessionId)}/events`)
    const feedback = events.filter(event => event.type === 'feedback_submitted')
    assert.deepEqual(feedback.map(event => event.text), ['the diff view is unreadable'])
    assert.equal(events.some(event => event.type === 'user_message' && event.content === 'the diff view is unreadable'), false)
    assert.deepEqual(
      events.filter(event => event.type === 'command_finished').map(event => event.outcome.code),
      ['feedback_recorded', 'feedback_text_required'],
    )

    await input.fill('验证后续上下文')
    await page.getByRole('button', { name: '发送' }).click()
    await page.getByText('READY 2', { exact: true }).waitFor()
    assert.equal(conversationRequests(model.received).length, 2)
    assert.equal(JSON.stringify(conversationRequests(model.received)[1]).includes('the diff view is unreadable'), false)

    await page.evaluate(() => localStorage.setItem('ternilo.locale', 'en'))
    await page.reload({ waitUntil: 'domcontentloaded' })
    await page.getByRole('textbox', { name: 'Enter task' }).waitFor()
    await page.locator('[data-chat-event="command"]').filter({ hasText: 'Feedback recorded' }).last().waitFor()
    await page.locator('[data-chat-event="command"]').filter({ hasText: 'Feedback text is required' }).last().waitFor()
    assert.equal(conversationRequests(model.received).length, 2)
    assert.deepEqual(pageErrors, [])
  } finally {
    await browser?.close()
    await stopProcess(ternilo.child)
    await model.close()
    await rm(dataDirectory, { recursive: true, force: true })
  }
})
