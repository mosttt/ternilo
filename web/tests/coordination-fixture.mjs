import assert from 'node:assert/strict'
import { until } from './model-device-fixture.mjs'
import { createServer } from 'node:http'
import { mkdir } from 'node:fs/promises'
import path from 'node:path'

export async function modelFixture() {
  const requests = []
  const server = createServer(async (request, response) => {
    const chunks = []
    for await (const chunk of request) chunks.push(chunk)
    const body = JSON.parse(Buffer.concat(chunks).toString())
    requests.push(body)
    const title = body.messages.some(message => message.role === 'system' && String(message.content).startsWith('You name software-agent conversations.'))
    const delegated = body.messages.some(message => message.role === 'tool')
    const spawn = !title && body.model === 'lead-model' && !delegated
      && body.tools?.some(tool => tool.function?.name === 'spawn_agent')
    const latestUser = body.messages.findLastIndex(message => message.role === 'user')
    const turnMessages = body.messages.slice(latestUser)
    const discovery = JSON.stringify(body.messages[latestUser]).includes('Discover tools for this task')
    const loaded = turnMessages.some(message => message.tool_call_id === 'discovery-search')
    const listed = turnMessages.some(message => message.tool_call_id === 'discovery-list')
    const discoverCall = { tool_calls: [{ index: 0, id: loaded ? 'discovery-list' : 'discovery-search', type: 'function', function: { name: loaded ? 'list_agents' : 'search_tools', arguments: loaded ? '{}' : JSON.stringify({ names: ['list_agents'] }) } }] }
    const budget = JSON.stringify(body.messages[latestUser]).includes('Read budgeted files')
    const budgetDone = turnMessages.some(message => message.tool_call_id === 'budget-two')
    const mapCode = JSON.stringify(body.messages[latestUser]).includes('Map code fixture')
    const mapDone = turnMessages.some(message => message.tool_call_id === 'code-map-fixture')
    const mapLoaded = body.tools?.some(tool => tool.function?.name === 'code_map')
    const budgetCalls = { tool_calls: ['one', 'two'].map((name, index) => ({ index, id: `budget-${name}`, type: 'function', function: { name: 'read_file', arguments: JSON.stringify({ path: `budget-${name}.txt` }) } })) }
    const mapCall = { tool_calls: [{ index: 0, id: mapLoaded ? 'code-map-fixture' : 'code-map-search', type: 'function', function: { name: mapLoaded ? 'code_map' : 'search_tools', arguments: JSON.stringify(mapLoaded ? { paths: ['map-fixture.py'], impact_symbol: 'map-fixture.py:2:1' } : { names: ['code_map'] }) } }] }
    const delta = budget ? budgetDone ? { content: 'Budget verified.' } : budgetCalls : mapCode ? mapDone ? { content: 'Code map verified.' } : mapCall : discovery ? listed ? { content: 'Tool discovery complete.' } : discoverCall : spawn ? { tool_calls: [{ index: 0, id: 'role-call', type: 'function', function: { name: 'spawn_agent', arguments: JSON.stringify({ task: 'Collect the evidence', role: 'investigator', background: false }) } }] }
      : { content: title ? 'Coordination acceptance' : body.model === 'role-model' ? 'Evidence collected.' : 'Coordination complete.' }
    if (!body.stream) {
      response.writeHead(200, { 'content-type': 'application/json' })
      response.end(JSON.stringify({ id: 'coordination-title', model: body.model, choices: [{ index: 0, message: { role: 'assistant', content: 'Coordination acceptance' }, finish_reason: 'stop' }], usage: { prompt_tokens: 30, completion_tokens: 12, total_tokens: 42 } }))
      return
    }
    const sse = value => `data: ${JSON.stringify(value)}\n\n`
    response.writeHead(200, { 'content-type': 'text/event-stream' })
    response.end(sse({ choices: [{ index: 0, delta, finish_reason: null }] })
      + sse({ choices: [{ index: 0, delta: {}, finish_reason: (budget ? !budgetDone : mapCode ? !mapDone : discovery ? !listed : spawn) ? 'tool_calls' : 'stop' }] })
      + sse({ choices: [], usage: { prompt_tokens: 30, completion_tokens: 12, total_tokens: 42 } }) + 'data: [DONE]\n\n')
  })
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve))
  return { baseUrl: `http://127.0.0.1:${server.address().port}/v1`, requests,
    close: () => new Promise(resolve => { server.closeAllConnections(); server.close(resolve) }) }
}

export async function verifyToolDiscovery(request, sessionId, upstream) {
  const start = upstream.requests.length
  await request(`/sessions/${sessionId}/queue`, { body: { content: { kind: 'prompt', input: 'Discover tools for this task' } } })
  await until(() => request(`/sessions/${sessionId}/queue`), value => !value.active_run_id && upstream.requests.slice(start).some(body => body.messages.some(message => message.tool_call_id === 'discovery-list')), 'completed tool discovery')
  const calls = upstream.requests.slice(start).filter(body => body.messages.some(message => message.role === 'user' && JSON.stringify(message.content).includes('Discover tools for this task')))
  assert.ok(calls.length >= 3)
  assert.ok(calls[0].tools.some(tool => tool.function?.name === 'search_tools'))
  assert.ok(!calls[0].tools.some(tool => tool.function?.name === 'list_agents'))
  assert.ok(calls[1].tools.some(tool => tool.function?.name === 'list_agents'))
}


export async function verifyToolBudgetAndMap(request, sessionId, upstream) {
  const resource = `/sessions/${sessionId}`
  const current = (await request('/state')).sessions.find(session => session.identity.session_id === sessionId)
  if (current.placement === 'cloud') await request(resource, { method: 'PATCH', body: { model_token_limit: 262144 } })
  for (const [name, content] of [['budget-one.txt', 'File budget evidence. '.repeat(1500)], ['budget-two.txt', 'File budget evidence. '.repeat(1500)], ['map-fixture.py', '# def fake(): pass\ndef actual():\n    target()\ndef caller():\n    actual()\ndef outer():\n    caller()\n']]) {
    const before = (await request(`${resource}/events`)).filter(event => event.type === 'turn_finished').length
    await request(`${resource}/queue`, { body: { content: { kind: 'prompt', input: `/write ${name} ${content}` } } })
    await until(() => request(`${resource}/events`), events => events.filter(event => event.type === 'turn_finished').length > before, 'written tool fixture')
    await until(() => request(`${resource}/queue`), queue => !queue.active_run_id, 'idle file fixture')
  }
  const plugins = current.profile_plugins.filter(entry => !['session', 'code-map'].includes(entry.id))
  plugins.push({ id: 'session', kind: 'ternilo.session.log', enabled: true, config: { max_tool_history_chars: 1024, tool_result_limits: { read_file: 256 } } })
  plugins.push({ id: 'code-map', kind: 'ternilo.tools.code_map', enabled: true, config: {} })
  await request(resource, { method: 'PATCH', body: { profile_plugins: plugins } })
  for (const [input, marker] of [['Read budgeted files', 'Budget verified.'], ['Map code fixture', 'Code map verified.']]) {
    await request(`${resource}/queue`, { body: { content: { kind: 'prompt', input } } })
    await until(() => request(`${resource}/events`), events => events.some(event => event.type === 'assistant_message' && event.response.content === marker), marker)
    await until(() => request(`${resource}/queue`), queue => !queue.active_run_id, 'idle tools')
    const requestEvent = (await request(`${resource}/events`)).findLast(event => event.type === 'model_request_started')
    const sent = upstream.requests.findLast(body => body.stream)
    const length = value => Array.from(value ?? '').length
    assert.equal(requestEvent.context_usage.system_chars, sent.messages.filter(message => message.role === 'system').reduce((sum, message) => sum + length(message.content), 0))
    assert.equal(requestEvent.context_usage.tool_schema_chars, (sent.tools ?? []).reduce((sum, tool) => sum + length(tool.function.name) + length(tool.function.description) + length(JSON.stringify(tool.function.parameters)), 0))
    assert.equal(requestEvent.context_usage.tool_output_chars, sent.messages.filter(message => message.role === 'tool').reduce((sum, message) => sum + length(message.content), 0))
    if (input.startsWith('Read')) {
      const messages = upstream.requests.at(-1).messages.filter(message => message.role === 'tool')
      assert.ok(messages.reduce((sum, message) => sum + Array.from(message.content).length, 0) <= 1024)
      assert.ok(messages.some(message => message.content.includes('context budget')))
      const fileResults = messages.filter(message => ['budget-one', 'budget-two'].includes(message.tool_call_id))
      assert.equal(fileResults.length, 2)
      assert.ok(fileResults.every(message => Array.from(message.content).length <= 256), 'per-tool limits apply before the shared history budget')
    }
  }
  const events = await request(`${resource}/events`)
  assert.ok(events.some(event => event.type === 'tool_call_finished' && event.name === 'read_file' && event.retained_output))
  const result = events.findLast(event => event.type === 'tool_call_finished' && event.name === 'code_map')
  assert.ok(result && !result.output.is_error)
  const map = JSON.parse(result.output.content)
  assert.equal(map.edge_resolution, 'syntactic')
  assert.ok(map.files[0].symbols.some(symbol => symbol.name === 'actual'))
  assert.ok(map.files[0].calls.some(call => call.target === 'target'))
  assert.equal(map.files[0].syntax_errors, false)
  assert.equal(map.impact.resolution, 'syntactic_candidates')
  assert.deepEqual(map.impact.candidate_callers.map(caller => caller.symbol.name).sort(), ['caller', 'outer'])
  assert.equal(map.impact.depth_limited, false)
}

export async function verifyContextComposition(page, request, sessionId, locale, artifact = locale) {
  const event = (await request(`/sessions/${sessionId}/events`)).findLast(event => event.type === 'model_request_started')
  assert.ok(event.context_usage)
  const originalViewport = page.viewportSize()
  if (originalViewport.width < 400) await page.setViewportSize({ width: originalViewport.width, height: 420 })
  const trigger = page.locator('[data-context-meter]')
  await trigger.click()
  const panel = page.getByRole('dialog', { name: { zh: '上下文详情', en: 'Context details', ko: '컨텍스트 상세' }[locale], exact: true })
  await panel.waitFor()
  for (const key of ['system_chars', 'tool_schema_chars', 'conversation_chars', 'tool_output_chars', 'additional_state_chars']) {
    const row = panel.locator(`[data-context-part="${key}"]`)
    await until(() => row.locator('dd').innerText(), text => Number(text.replaceAll(',', '')) === event.context_usage[key], `current context ${key}`)
  }
  const bounds = await panel.boundingBox()
  const viewport = page.viewportSize()
  assert.ok(bounds.x >= 0 && bounds.y >= 0 && bounds.x + bounds.width <= viewport.width + 1 && bounds.y + bounds.height <= viewport.height + 1, 'context popup stays inside viewport')
  assert.equal(await panel.evaluate(element => element.scrollWidth > element.clientWidth), false)
  if (await panel.evaluate(element => element.scrollHeight > element.clientHeight + 1)) {
    const touch = await page.context().newCDPSession(page)
    const x = bounds.x + bounds.width / 2, y = bounds.y + bounds.height - 20
    try {
      await touch.send('Input.dispatchTouchEvent', { type: 'touchStart', touchPoints: [{ x, y }] })
      for (let i = 1; i <= 8; i++) await touch.send('Input.dispatchTouchEvent', { type: 'touchMove', touchPoints: [{ x, y: y - Math.min(160, bounds.height - 40) * i / 8 }] })
      await touch.send('Input.dispatchTouchEvent', { type: 'touchEnd', touchPoints: [] })
      await until(() => panel.evaluate(element => element.scrollTop), value => value > 0, 'context popup native scrolling')
    } finally { await touch.detach() }
  }
  await panel.locator('[data-context-part="additional_state_chars"]').scrollIntoViewIfNeeded()
  if (process.env.TERNILO_E2E_ARTIFACT_DIR) {
    await mkdir(process.env.TERNILO_E2E_ARTIFACT_DIR, { recursive: true })
    await page.screenshot({ path: path.join(process.env.TERNILO_E2E_ARTIFACT_DIR, `context-composition-${artifact}.png`) })
  }
  await page.keyboard.press('Escape')
  await panel.waitFor({ state: 'hidden' })
  await page.setViewportSize(originalViewport)
}
