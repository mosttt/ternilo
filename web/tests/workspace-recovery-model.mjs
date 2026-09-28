import assert from 'node:assert/strict'
import { createServer } from 'node:http'
import { body, json } from './platform-e2e-fixture.mjs'

/** A real tool workload keeps writing while its trusted Worker parent is suspended. */
export async function recoveryModel() {
  const calls = [], failures = []
  const server = createServer(async (request, response) => {
    try {
      assert.equal(request.url, '/v1/chat/completions')
      const input = JSON.parse(await body(request))
      const title = input.messages.some(message => String(message.content).includes('You name software-agent conversations'))
      const marker = input.messages.filter(message => message.role === 'user')
        .map(message => typeof message.content === 'string' ? message.content : message.content.map(part => part.text ?? '').join('\n'))
        .join('\n').match(/RECOVERY::(old|new)/)?.[1]
      assert.ok(title || marker)
      const hasResult = input.messages.some(message => message.role === 'tool')
      const tool = !title && !hasResult ? {
        index: 0, id: `recovery-${marker}-shell`, type: 'function', function: { name: 'shell', arguments: JSON.stringify({
          command: marker === 'old'
            ? "setsid sh -c 'while :; do printf x >> old-writes; sleep 0.05; done' & wait"
            : "printf new > new-writes; printf 'New writer finished.'",
          timeout_ms: 120_000,
        }) },
      } : null
      calls.push({ title, marker, stage: hasResult ? 'finish' : 'start' })
      const content = title ? 'Workspace recovery verification' : `Completed ${marker} writer.`
      const identity = { id: `recovery-${calls.length}`, created: 1, model: input.model }
      const usage = { prompt_tokens: 100, completion_tokens: 20, total_tokens: 120 }
      if (!input.stream) return json(response, 200, { ...identity, object: 'chat.completion', choices: [{ index: 0,
        message: { role: 'assistant', content }, finish_reason: 'stop' }], usage })
      assert.ok(!tool || input.tools.some(value => value.function?.name === 'shell'))
      const sse = value => `data: ${JSON.stringify(value)}\n\n`
      response.writeHead(200, { 'content-type': 'text/event-stream' })
      response.end(sse({ ...identity, object: 'chat.completion.chunk', choices: [{ index: 0,
        delta: tool ? { role: 'assistant', tool_calls: [tool] } : { role: 'assistant', content }, finish_reason: null }] })
        + sse({ ...identity, object: 'chat.completion.chunk', choices: [{ index: 0, delta: {}, finish_reason: tool ? 'tool_calls' : 'stop' }] })
        + sse({ ...identity, object: 'chat.completion.chunk', choices: [], usage }) + 'data: [DONE]\n\n')
    } catch (error) {
      failures.push(error.stack)
      if (!response.headersSent) json(response, 500, { error: { message: error.message } })
      else response.end()
    }
  })
  await new Promise((resolve, reject) => { server.once('error', reject); server.listen(0, '127.0.0.1', resolve) })
  return { baseUrl: `http://127.0.0.1:${server.address().port}/v1`, calls, failures,
    close: async () => { server.closeAllConnections(); await new Promise(resolve => server.close(resolve)) },
  }
}
