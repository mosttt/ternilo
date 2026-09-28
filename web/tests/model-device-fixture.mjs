import { selectChoice } from './browser-select-fixture.mjs'
import assert from 'node:assert/strict'
import { createServer } from 'node:http'
import { serverRequest } from './platform-e2e-fixture.mjs'

export async function until(read, predicate, label) {
  const deadline = Date.now() + 35_000
  let value
  while (Date.now() < deadline) {
    value = await read()
    if (predicate(value)) return value
    await new Promise(resolve => setTimeout(resolve, 100))
  }
  throw new Error(`${label}: ${JSON.stringify(value)}`)
}
export async function modelFixture(proof = 'account-device-task') {
  const calls = []
  const server = createServer(async (request, response) => {
    if (request.url !== '/v1/chat/completions') { response.writeHead(404).end(); return }
    const chunks = []
    for await (const chunk of request) chunks.push(chunk)
    const body = JSON.parse(Buffer.concat(chunks).toString())
    calls.push(body)
    const latest = body.messages.findLastIndex(message => message.role === 'user')
    const tools = body.tools?.some(tool => tool.function?.name === 'write_file')
    const write = tools && !body.messages.slice(latest + 1).some(message => message.role === 'tool')
    const content = 'The account model completed this local task.'
    const identity = { id: `fixture-${calls.length}`, model: body.model, created: 1 }
    const usage = { prompt_tokens: 30, completion_tokens: 12, total_tokens: 42 }
    if (!body.stream) {
      response.writeHead(200, { 'content-type': 'application/json' })
      response.end(JSON.stringify({ ...identity, object: 'chat.completion', choices: [{ index: 0, message: { role: 'assistant', content }, finish_reason: 'stop' }], usage }))
      return
    }
    const delta = write ? { role: 'assistant', tool_calls: [{ index: 0, id: `write-${calls.length}`, type: 'function', function: {
      name: 'write_file', arguments: JSON.stringify({ path: 'model-device-proof.txt', content: proof }),
    } }] } : { role: 'assistant', content }
    const sse = value => `data: ${JSON.stringify({ ...identity, object: 'chat.completion.chunk', ...value })}\n\n`
    response.writeHead(200, { 'content-type': 'text/event-stream' })
    response.end(sse({ choices: [{ index: 0, delta, finish_reason: null }] })
      + sse({ choices: [{ index: 0, delta: {}, finish_reason: write ? 'tool_calls' : 'stop' }] })
      + sse({ choices: [], usage }) + 'data: [DONE]\n\n')
  })
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve))
  return { baseUrl: `http://127.0.0.1:${server.address().port}/v1`, calls,
    close: () => new Promise(resolve => { server.closeAllConnections(); server.close(resolve) }) }
}
export async function seedModels(server, upstream) {
  const request = (url, options = {}) => serverRequest(server.origin, url, { token: server.owner.session.access_token, ...options })
  await request('/admin/models/providers', { body: { profile: {
    id: 'fixture', display_name: 'Fixture upstream', base_url: upstream.baseUrl, protocol: 'openai-chat-completions', api_key_ref: null,
    defaults: { context_window: 32000, max_output_tokens: 2048 }, models: [{ id: 'upstream-model', settings: { mode: 'inherit' } }],
    timeout_ms: 0, max_attempts: 1, retry_base_delay_ms: 25,
  }, enabled: true } })
  await request('/admin/models/publications', { body: { model_id: 'account-model', display_name: 'Account Model', provider_id: 'fixture', upstream_model: 'upstream-model', enabled: true } })
  const grant = name => request('/admin/models/grants', { body: { name, subject: { kind: 'user', id: server.owner.session.user.user_id },
    model_ids: ['account-model'], monthly_tokens: 2_000_000, max_concurrent_requests: 4, allow_resource_sharing: false } })
  return { request, grant, first: await grant('Budget Alpha'), second: await grant('Budget Beta') }
}
export async function localApi(origin) {
  const html = await (await fetch(origin)).text()
  const token = JSON.parse(html.match(/window\.__TERNILO_BOOT__\s*=\s*(\{[^<]+\});/)[1]).apiToken
  return (url, options = {}) => serverRequest(origin, url, { token, ...options })
}
export async function openModels(page) {
  await page.getByRole('button', { name: '设置', exact: true }).click()
  const settings = page.getByRole('dialog', { name: '设置', exact: true })
  await settings.getByRole('button', { name: '模型', exact: true }).click()
  return settings
}
export async function closeModels(page) {
  await page.getByRole('button', { name: '关闭设置', exact: true }).click()
}
export async function approveConnection(local, approval, server, name, grantId, providerId) {
  const settings = await openModels(local)
  const request = await localApi(new URL(local.url()).origin)
  const previous = (await request('/model-connections')).length
  await settings.getByRole('button', { name: '连接 Server', exact: true }).click()
  await settings.getByLabel('Server 地址', { exact: true }).fill(server.origin)
  await settings.getByLabel('连接名称', { exact: true }).fill(name)
  await settings.getByRole('button', { name: '连接 Server', exact: true }).last().click()
  const code = await settings.locator('[data-device-user-code]').textContent()
  const link = await settings.getByRole('link', { name: '前往 Server 确认', exact: true }).getAttribute('href')
  await approval.goto(link)
  const remembered = await approval.evaluate(() => sessionStorage.getItem('ternilo.native.session'))
  if (!remembered) {
    await approval.getByLabel('用户名', { exact: true }).waitFor()
    await approval.getByLabel('用户名', { exact: true }).fill(server.owner.username)
    await approval.getByLabel('密码', { exact: true }).fill(server.owner.password)
    await approval.getByRole('button', { name: '登录', exact: true }).click()
  }
  await approval.getByText(code, { exact: true }).waitFor()
  if (!await approval.getByText(`当前账号：${server.owner.username}`, { exact: true }).count()) {
    await approval.getByRole('button', { name: '切换账号', exact: true }).tap()
    await approval.getByLabel('用户名', { exact: true }).fill(server.owner.username)
    await approval.getByLabel('密码', { exact: true }).fill(server.owner.password)
    await approval.getByRole('button', { name: '登录', exact: true }).click()
    await approval.getByText(code, { exact: true }).waitFor()
    await approval.getByText(`当前账号：${server.owner.username}`, { exact: true }).waitFor()
  }
  if (grantId || providerId) {
    await selectChoice(approval.getByLabel('允许使用的模型范围', { exact: true }), 'selected')
    if (grantId) await approval.locator(`[data-device-grant="${grantId}"]`).getByRole('checkbox').first().check()
    if (providerId) await approval.locator(`[data-device-provider="${providerId}"]`).getByRole('checkbox').first().check()
  }
  assert.ok(await approval.evaluate(() => document.documentElement.scrollWidth <= innerWidth))
  await approval.getByRole('button', { name: '允许连接', exact: true }).tap()
  await approval.getByText('已允许连接。回到 Ternilo 即可继续。', { exact: true }).waitFor()
  await until(() => settings.locator('[data-model-connection]').count(), count => count === previous + 1, 'account connection saved')
  await closeModels(local)
}
export async function chooseModel(page, providerId) {
  const trigger = page.locator('button[title]').filter({ hasText: /配置模型|Account Model|account-model/ }).last()
  await trigger.click()
  await page.getByRole('menuitem', { name: /^模型/ }).click()
  await page.locator(`[data-model-provider="${providerId}"]`).getByRole('menuitem').first().click()
}
