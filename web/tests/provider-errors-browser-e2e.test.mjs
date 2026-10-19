import assert from 'node:assert/strict'
import { createServer } from 'node:http'
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { chromium } from 'playwright'
import { freePort, initializeServer, repository, serverRequest, startProcess, stopProcess, waitForHttp } from './platform-e2e-fixture.mjs'
import { until } from './model-device-fixture.mjs'

async function rejectedProvider() {
  const calls = []
  let status = 402, code, kind
  const server = createServer(async (request, response) => {
    assert.equal(request.headers.authorization, 'Bearer deepseek-private-fixture-key')
    const chunks = []; for await (const chunk of request) chunks.push(chunk)
    const body = JSON.parse(Buffer.concat(chunks).toString())
    calls.push({ status, code, body })
    response.writeHead(status, { 'content-type': 'application/json', 'x-request-id': `ds-failure-${calls.length}` })
    // Upstream diagnostics are deliberately unsafe; only the fixed HTTP explanation may leave Server.
    response.end(JSON.stringify({ error: { message: 'Insufficient Balance deepseek-private-fixture-key private-upstream-route', code, type: kind ?? 'unknown_error' } }))
  })
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve))
  return { url: `http://127.0.0.1:${server.address().port}/v1`, calls, setStatus: next => { status = next }, setFailure: (next, reason, type) => { status = next; code = reason; kind = type },
    close: () => new Promise(resolve => { server.closeAllConnections(); server.close(resolve) }) }
}

test('Provider account failures reach real Server/Node conversations, localized history and details without leaking keys', { timeout: 180_000 }, async t => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-deepseek-errors-'))
  const artifacts = process.env.TERNILO_E2E_ARTIFACT_DIR ?? directory
  await mkdir(artifacts, { recursive: true })
  const processes = [], errors = []
  let model, browser, page
  try {
    model = await rejectedProvider()
    const server = await initializeServer({ directory: path.join(directory, 'server'), origin: `http://127.0.0.1:${await freePort()}`, mode: 'single_user' })
    processes.push(server)
    const identity = server.owner.session
    const api = (resource, options = {}) => serverRequest(server.origin, resource, { token: identity.access_token, tenantId: identity.personal_tenant_id, ...options })
    await api('/credentials', { body: { name: 'DEEPSEEK_TEST_KEY', value: 'deepseek-private-fixture-key' } })
    await api('/providers', { body: { id: 'deepseek-errors', display_name: 'DeepSeek test', base_url: model.url, protocol: 'deepseek-responses', api_key_ref: 'DEEPSEEK_TEST_KEY',
      defaults: { context_window: 32000, max_output_tokens: 2048 }, models: [{ id: 'ds-test', settings: { mode: 'inherit' } }], timeout_ms: 10000, max_attempts: 3, retry_base_delay_ms: 10 } })
    const { enrollment } = await api(`/tenants/${identity.personal_tenant_id}/my-computer-enrollments`, { body: { name: 'DeepSeek execution', ttl_seconds: 600 } })
    const { credential } = await api('/enrollments/consume', { body: { token: enrollment.token } })
    const localOrigin = `http://127.0.0.1:${await freePort()}`
    const node = startProcess(process.env.TERNILO_E2E_NODE_BINARY ?? path.join(repository, 'target/debug/ternilo'), [
      'serve', '--listen', new URL(localOrigin).host, '--data-dir', path.join(directory, 'node'), '--node-id', enrollment.executor_id,
      '--gateway-url', `${server.origin.replace('http:', 'ws:')}/api/v1/executors/connect`, '--allow-insecure-gateway',
    ], { TERNILO_LOCAL_TOKEN: credential.token, XDG_STATE_HOME: path.join(directory, 'state') })
    processes.push(node)
    await waitForHttp(localOrigin, node)
    await until(() => api('/model-computers'), value => value.some(item => item.executor_id === enrollment.executor_id && item.connected), 'Node connects')
    const folder = path.join(directory, 'workspace'); await mkdir(folder)
    const { workspace } = await api('/workspaces', { body: { project_id: identity.personal_project_id, name: 'DeepSeek errors', placement: 'local_node', executor_id: enrollment.executor_id, path: folder } })
    const session = await api('/sessions', { body: { workspace_id: workspace.workspace_id } })
    const id = session.identity.session_id
    await api(`/sessions/${id}`, { method: 'PATCH', body: { model: { provider: 'account_provider', owner_user_id: identity.user.user_id, provider_id: 'deepseek-errors', model: 'ds-test' } } })

    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
    page = await browser.newPage({ locale: 'zh-CN', viewport: { width: 1366, height: 900 }, serviceWorkers: 'block' })
    page.on('pageerror', error => errors.push(error.message))
    page.on('console', message => { if (message.type() === 'error') errors.push(message.text()) })
    page.on('response', response => { if (response.status() >= 400) errors.push(`HTTP ${response.status()} ${new URL(response.url()).pathname}`) })
    await page.goto(server.origin)
    await page.getByLabel('用户名', { exact: true }).fill(server.owner.username)
    await page.getByLabel('密码', { exact: true }).fill(server.owner.password)
    await page.getByRole('button', { name: '登录', exact: true }).click()
    await page.locator(`[data-sidebar-session-row][data-session-id="${id}"] [data-sidebar-session-button]`).click()
    await page.getByRole('textbox', { name: '输入任务', exact: true }).fill('hello-balance')
    await page.getByRole('button', { name: '发送', exact: true }).click()
    const failed = page.locator('[data-chat-event="turn_failed"]').last()
    await failed.getByText('DeepSeek 余额不足', { exact: true }).waitFor()
    const events = await api(`/sessions/${id}/events`)
    const failure = events.findLast(event => event.type === 'turn_failed')
    assert.match(failure.message, /DeepSeek API error \(HTTP 402\)/)
    assert.doesNotMatch(JSON.stringify(events), /deepseek-private-fixture-key|private-upstream-route|check the Server model request log/)
    assert.equal(events.filter(event => event.run_id === failure.run_id && event.type === 'model_retry_scheduled').length, 0)
    assert.equal(model.calls.filter(call => call.body.tools?.length).length, 1, 'balance failures are not retried')

    for (const [locale, title, width] of [['zh', 'DeepSeek 余额不足', 1366], ['en', 'Insufficient DeepSeek balance', 390], ['ko', 'DeepSeek 잔액 부족', 390]]) {
      await page.evaluate(locale => localStorage.setItem('ternilo.locale', locale), locale)
      await page.setViewportSize({ width, height: 900 })
      await page.reload()
      await failed.getByText(title, { exact: true }).waitFor()
      assert.match(await failed.innerText(), /platform\.deepseek\.com\/top_up/)
      assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth))
      assert.ok(await failed.evaluate(element => element.scrollWidth <= element.clientWidth))
      await page.screenshot({ path: path.join(artifacts, `deepseek-balance-${locale}.png`), fullPage: true })
      await failed.click()
      const details = page.locator('[data-details-state="error"]')
      await details.getByText(title, { exact: true }).waitFor()
      assert.match(await details.innerText(), /HTTP 402/)
      await page.screenshot({ path: path.join(artifacts, `deepseek-details-${locale}.png`), fullPage: true })
    }
    await page.evaluate(() => localStorage.setItem('ternilo.locale', 'zh'))
    await page.setViewportSize({ width: 1366, height: 900 })
    await page.reload()
    for (const [status, title] of [[400, 'DeepSeek 请求格式错误'], [401, 'DeepSeek API Key 认证失败'], [422, 'DeepSeek 请求参数错误'], [429, 'DeepSeek 请求过于频繁'], [500, 'DeepSeek 服务故障'], [503, 'DeepSeek 服务繁忙']]) {
      model.setStatus(status)
      const queued = await api(`/sessions/${id}/queue`, { body: { content: { kind: 'prompt', input: `failure-${status}` }, attachments: [] } })
      const history = await until(() => api(`/sessions/${id}/events`), value => value.some(event => event.run_id === queued.run_id && event.type === 'turn_failed'), `HTTP ${status} failure`)
      await failed.getByText(title, { exact: true }).waitFor()
      assert.match(history.findLast(event => event.type === 'turn_failed').message, new RegExp(`HTTP ${status}`))
      assert.doesNotMatch(JSON.stringify(history), /deepseek-private-fixture-key|private-upstream-route/)
      assert.equal(model.calls.filter(call => call.status === status && call.body.tools?.length).length, [429, 500, 503].includes(status) ? 3 : 1)
    }
    const openaiProfile = { id: 'openai-errors', display_name: 'OpenAI-compatible test', base_url: model.url, protocol: 'openai-responses', api_key_ref: 'DEEPSEEK_TEST_KEY',
      defaults: { context_window: 32000, max_output_tokens: 2048 }, models: [{ id: 'openai-test', settings: { mode: 'inherit' } }], timeout_ms: 10000, max_attempts: 3, retry_base_delay_ms: 10 }
    await api('/providers', { body: openaiProfile })
    await api(`/sessions/${id}`, { method: 'PATCH', body: { model: { provider: 'account_provider', owner_user_id: identity.user.user_id, provider_id: 'openai-errors', model: 'openai-test' } } })
    const billingCases = [
      ['organization_spend_limit_exceeded', 'API 组织支出已达上限', 1], ['project_spend_limit_exceeded', 'API 项目支出已达上限', 1],
      ['organization_usage_limit_exceeded', 'API 组织使用额度已用尽', 1], ['insufficient_quota', '模型 API 额度不足', 1],
      ['rate_limit_exceeded', '模型 API 请求速率已达上限', 3], ['slow_down', '模型 API 请求增长过快', 3],
      ['credit_balance_exhausted', '模型 API 余额不足', 1],
    ]
    for (const [code, title, attempts] of billingCases) {
      model.setFailure(429, code, 'insufficient_quota')
      const queued = await api(`/sessions/${id}/queue`, { body: { content: { kind: 'prompt', input: code }, attachments: [] } })
      const history = await until(() => api(`/sessions/${id}/events`), value => value.some(event => event.run_id === queued.run_id && event.type === 'turn_failed'), code)
      await failed.getByText(title, { exact: true }).waitFor()
      assert.ok(history.findLast(event => event.type === 'turn_failed').message.includes(`HTTP 429; ${code}`))
      assert.doesNotMatch(JSON.stringify(history), /deepseek-private-fixture-key|private-upstream-route/)
      assert.equal(model.calls.filter(call => call.code === code && call.body.tools?.length).length, attempts)
    }
    for (const [locale, title, width] of [['zh', '模型 API 余额不足', 1366], ['en', 'Insufficient model API balance', 390], ['ko', '모델 API 잔액 부족', 390]]) {
      await page.evaluate(locale => localStorage.setItem('ternilo.locale', locale), locale)
      await page.setViewportSize({ width, height: 900 }); await page.reload()
      await failed.getByText(title, { exact: true }).waitFor()
      assert.ok(await failed.evaluate(element => element.scrollWidth <= element.clientWidth))
      assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth))
      await page.screenshot({ path: path.join(artifacts, `openai-balance-${locale}.png`), fullPage: true })
    }
    // The external model API uses the same safe classification for upstream failures.
    await api('/admin/models/providers', { body: { profile: { ...openaiProfile, api_key_ref: null }, enabled: true, api_key: 'deepseek-private-fixture-key' } })
    await api('/admin/models/publications', { body: { model_id: 'provider-error', display_name: 'Provider error', provider_id: openaiProfile.id, upstream_model: 'openai-test', enabled: true } })
    const grant = await api('/admin/models/grants', { body: { name: 'Error test', subject: { kind: 'user', id: identity.user.user_id }, model_ids: ['provider-error'], monthly_tokens: 1000000, max_concurrent_requests: 2, allow_resource_sharing: false } })
    const key = await api('/model-access/keys', { body: { name: 'Error test key', grant_id: grant.grant_id, model_ids: ['provider-error'] } })
    for (const stream of [false, true]) {
      const response = await fetch(`${server.origin}/v1/responses`, { method: 'POST', headers: { authorization: `Bearer ${key.token}`, 'content-type': 'application/json' }, body: JSON.stringify({ model: 'provider-error', input: 'hello', stream, max_output_tokens: 512 }) })
      assert.equal(response.status, 502)
      const rejected = await response.json()
      assert.equal(rejected.error.code, 'credit_balance_exhausted')
      assert.match(rejected.error.message, /prepaid API credits are exhausted/)
      assert.doesNotMatch(JSON.stringify(rejected), /deepseek-private-fixture-key|private-upstream-route/)
    }
    assert.deepEqual(errors, [])
    await writeFile(path.join(artifacts, 'result.json'), JSON.stringify({ status: 'passed', locales: ['zh', 'en', 'ko'], httpStatuses: [400, 401, 402, 422, 429, 500, 503], openaiCodes: billingCases.map(([code]) => code), externalApi: true, errors }, null, 2))
  } catch (error) {
    if (page) await page.screenshot({ path: path.join(artifacts, 'failure.png'), fullPage: true }).catch(() => {})
    for (const process of processes) t.diagnostic(process.diagnostics())
    throw error
  } finally {
    await browser?.close()
    for (const process of processes.reverse()) await stopProcess(process)
    await model?.close()
    await rm(directory, { recursive: true, force: true })
  }
})
