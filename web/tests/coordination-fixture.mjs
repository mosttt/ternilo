import assert from 'node:assert/strict'
import { until } from './model-device-fixture.mjs'
import { createServer } from 'node:http'

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
    const delta = discovery ? listed ? { content: 'Tool discovery complete.' } : discoverCall : spawn ? { tool_calls: [{ index: 0, id: 'role-call', type: 'function', function: { name: 'spawn_agent', arguments: JSON.stringify({ task: 'Collect the evidence', role: 'investigator', background: false }) } }] }
      : { content: title ? 'Coordination acceptance' : body.model === 'role-model' ? 'Evidence collected.' : 'Coordination complete.' }
    if (!body.stream) {
      response.writeHead(200, { 'content-type': 'application/json' })
      response.end(JSON.stringify({ id: 'coordination-title', model: body.model, choices: [{ index: 0, message: { role: 'assistant', content: 'Coordination acceptance' }, finish_reason: 'stop' }], usage: { prompt_tokens: 30, completion_tokens: 12, total_tokens: 42 } }))
      return
    }
    const sse = value => `data: ${JSON.stringify(value)}\n\n`
    response.writeHead(200, { 'content-type': 'text/event-stream' })
    response.end(sse({ choices: [{ index: 0, delta, finish_reason: null }] })
      + sse({ choices: [{ index: 0, delta: {}, finish_reason: (discovery ? !listed : spawn) ? 'tool_calls' : 'stop' }] })
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
