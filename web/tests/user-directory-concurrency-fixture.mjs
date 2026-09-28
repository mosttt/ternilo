import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { mkdir, readFile, writeFile } from 'node:fs/promises'
import { createServer } from 'node:http'
import path from 'node:path'
import { execute, repository, selectSpace } from './platform-e2e-fixture.mjs'
import { until } from './model-device-fixture.mjs'

export const nodeBinary = process.env.TERNILO_E2E_NODE_BINARY ?? path.join(repository, 'target/debug/ternilo')
export const modelSelection = { provider: 'named_provider', provider_id: 'directory-fixture', model: 'fixture' }
export const taskInput = page => page.getByRole('textbox', { name: '输入任务', exact: true })
export const waiting = page => page.locator('[role="status"]').filter({ hasText: '等待目录空闲' }).first()

export async function isolatedEnvironment(directory) {
  const environment = Object.fromEntries(Object.keys(process.env).filter(name => name.startsWith('TERNILO_')).map(name => [name, undefined]))
  for (const name of ['HOME', 'XDG_STATE_HOME', 'XDG_CONFIG_HOME', 'XDG_DATA_HOME', 'XDG_CACHE_HOME', 'XDG_RUNTIME_DIR']) {
    environment[name] = path.join(directory, name.toLowerCase())
    await mkdir(environment[name], { recursive: true, mode: 0o700 })
  }
  return environment
}

export async function directoryModel() {
  const calls = [], errors = [], held = new Map()
  const server = createServer(async (request, response) => {
    try {
      assert.equal(request.url, '/v1/responses')
      const chunks = []
      for await (const chunk of request) chunks.push(chunk)
      const body = JSON.parse(Buffer.concat(chunks).toString())
      const title = body.instructions?.includes('You name software-agent conversations') ?? false
      const input = body.input?.filter(item => item.role === 'user').at(-1)?.content
        ?.filter(item => item.type === 'input_text').map(item => item.text).join('\n') ?? ''
      const record = { input, title, completed: false, disconnected: false }
      calls.push(record)
      const finish = () => {
        record.completed = true
        const text = title ? 'Directory fixture' : 'Completed: ' + input
        const output = [{ type: 'message', role: 'assistant', content: [{ type: 'output_text', text }] }]
        const result = { status: 'completed', output, usage: { input_tokens: 20, output_tokens: 10 } }
        if (!body.stream) {
          response.writeHead(200, { 'content-type': 'application/json' }).end(JSON.stringify(result))
          return
        }
        const event = value => 'data: ' + JSON.stringify(value) + '\n\n'
        response.writeHead(200, { 'content-type': 'text/event-stream' })
        response.end(event({ type: 'response.output_text.delta', output_index: 0, content_index: 0, delta: text })
          + event({ type: 'response.completed', response: result }))
      }
      if (!title && input.startsWith('hold-')) {
        assert.equal(held.has(input), false, 'held model calls must not be retried: ' + input)
        held.set(input, { response, finish })
        response.on('close', () => { record.disconnected = !record.completed; held.delete(input) })
      } else finish()
    } catch (error) {
      errors.push(error.stack)
      response.writeHead(500).end(String(error))
    }
  })
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve))
  return {
    baseUrl: `http://127.0.0.1:${server.address().port}/v1`, calls, errors, held,
    count(input) { return calls.filter(call => !call.title && call.input === input).length },
    reached(input) { return until(() => held.has(input), Boolean, 'model request is held: ' + input) },
    release(input) {
      const entry = held.get(input)
      assert.ok(entry, 'model call remains held: ' + input)
      held.delete(input)
      entry.finish()
    },
    async close() {
      server.closeAllConnections()
      await new Promise(resolve => server.close(resolve))
    },
  }
}

export async function configureModel(request, model) {
  await request('/providers', { body: {
    id: modelSelection.provider_id, display_name: 'Directory fixture', base_url: model.baseUrl,
    protocol: 'openai-responses', api_key_ref: null,
    defaults: { context_window: 128000, max_output_tokens: 4096 },
    models: [{ id: 'fixture', settings: { mode: 'inherit' } }], timeout_ms: 0, max_attempts: 1, retry_base_delay_ms: 50,
  } })
}

export async function cliProfile(directory, model) {
  const file = path.join(directory, 'cli-profile.json')
  await writeFile(file, JSON.stringify({ plugins: [{ id: 'model', kind: 'ternilo.model.openai_compatible', config: {
    provider: 'directory-fixture', base_url: model.baseUrl, protocol: 'openai-responses', model: 'fixture', timeout_ms: 0, max_attempts: 1,
  } }] }))
  return file
}

export function startCli({ directory, environment, prompt, profile }) {
  const args = ['run', prompt, '--json', ...(profile ? ['--profile', profile] : [])]
  const process = execute(nodeBinary, args, {
    cwd: directory, timeout: 60_000,
    env: Object.fromEntries(Object.entries({ ...globalThis.process.env, ...environment }).filter(([, value]) => value !== undefined)),
  })
  const completion = process.then(result => ({ result }), error => ({ error }))
  return { child: process.child, completion }
}

export async function cliOutcome(cli) {
  const outcome = await cli.completion
  if (outcome.error) throw outcome.error
  return JSON.parse(outcome.result.stdout)
}

export function observe(page, label, observations) {
  page.setDefaultTimeout(15_000)
  page.on('pageerror', error => observations.errors.push(label + ': ' + error.message))
  page.on('console', message => {
    observations.console.push({ page: label, type: message.type(), text: message.text() })
    if (message.type() === 'error') observations.errors.push(label + ': ' + message.text())
  })
  page.on('response', response => {
    const pathname = new URL(response.url()).pathname
    observations.network.push({ page: label, method: response.request().method(), path: pathname, status: response.status() })
    if (response.status() >= 400) observations.errors.push(`${label}: HTTP ${response.status()} ${pathname}`)
  })
  page.on('requestfailed', request => {
    const failure = request.failure()?.errorText ?? ''
    observations.network.push({ page: label, method: request.method(), path: new URL(request.url()).pathname, failure })
    if (!failure.includes('ERR_ABORTED')) observations.errors.push(`${label}: ${failure} ${request.url()}`)
  })
}

export async function openSession(page, origin, sessionId, account, tenantId) {
  await page.goto(origin)
  if (account) {
    await page.getByLabel('用户名', { exact: true }).fill(account.username)
    await page.getByLabel('密码', { exact: true }).fill(account.password)
    await page.getByRole('button', { name: '登录', exact: true }).click()
    await selectSpace(page, tenantId)
  }
  await page.locator('[data-sidebar-workspace-button]').first().waitFor()
  for (const toggle of await page.locator('[data-sidebar-workspace-button][aria-expanded="false"]').all()) await toggle.click()
  for (const more of await page.locator('[data-sidebar-workspace-group] [role="treeitem"] > button[aria-expanded="false"]:not([data-sidebar-workspace-button]):not([data-sidebar-session-button])').all()) await more.click()
  await page.locator(`[data-sidebar-session-row][data-session-id="${sessionId}"] [data-sidebar-session-button]`).click()
  await taskInput(page).waitFor()
}

export async function send(page, text) {
  await taskInput(page).fill(text)
  const submitted = page.waitForResponse(response => /\/api\/v1\/sessions\/[^/]+\/queue$/.test(new URL(response.url()).pathname) && response.request().method() === 'POST')
  await page.getByRole('button', { name: '发送', exact: true }).click()
  const response = await submitted
  assert.equal(response.status(), 201)
  return response.json()
}

export const queue = (request, sessionId, input) => request(`/sessions/${sessionId}/queue`, { body: { content: { kind: 'prompt', input } } })
export const history = (request, sessionId) => request(`/sessions/${sessionId}/events`)
export const pendingQuestion = (request, sessionId) => until(() => request(`/questions?session_id=${sessionId}`), questions => questions.length === 1, 'session retains a pending question')

export async function terminal(request, sessionId, runId, expected = 'turn_finished') {
  const events = await until(() => history(request, sessionId), events => events.some(event => event.run_id === runId && ['turn_finished', 'turn_cancelled', 'turn_failed'].includes(event.type)), 'turn completes: ' + runId)
  const run = events.filter(event => event.run_id === runId)
  assert.ok(run.some(event => event.type === expected), JSON.stringify(run))
  return run
}

export function noWaiting(events) {
  assert.equal(events.some(event => event.type === 'workspace_execution_waiting'), false, 'same-user tasks never wait for another same-user directory holder')
}

export async function screenshots(page, artifacts, name) {
  for (const width of [1280, 390]) {
    await page.setViewportSize({ width, height: width === 390 ? 844 : 900 })
    await page.evaluate(async () => {
      await new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve)))
      await Promise.all(document.getAnimations().filter(animation => animation.effect?.getTiming().iterations !== Infinity).map(animation => animation.finished.catch(() => {})))
    })
    const closeSidebar = page.locator('[data-app-sidebar-column]:not([inert]) [data-mobile-sidebar-close]')
    if (width === 390 && await closeSidebar.isVisible()) await closeSidebar.click()
    const bottom = page.getByRole('button', { name: '回到底部', exact: true })
    if (await bottom.isVisible()) {
      await bottom.click()
      await until(() => page.locator('[data-conversation-scroll]').evaluate(element => element.scrollHeight - element.clientHeight - element.scrollTop), remaining => remaining < 8, 'screenshot shows the latest task outcome')
    }
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true)
    await page.screenshot({ path: path.join(artifacts, `${name}-${width}.png`), animations: 'disabled' })
  }
}

export async function verifyAssets(origin) {
  const hashes = {}
  for (const name of ['app.js', 'app.css']) {
    const response = await fetch(origin + '/assets/' + name)
    assert.equal(response.status, 200)
    const digest = bytes => createHash('sha256').update(bytes).digest('hex')
    hashes[name] = digest(Buffer.from(await response.arrayBuffer()))
    assert.equal(hashes[name], digest(await readFile(path.join(repository, 'web/dist/assets', name))))
  }
  return hashes
}
