import assert from 'node:assert/strict'
import { createServer } from 'node:http'

const sse = value => `data: ${JSON.stringify(value)}\n\n`
export const thinking = '先读取文件，再根据工具结果回答。'
export const finalAnswer = '原生协议验证完成，文件内容已读取。'

export async function settleNativeViewport(page) {
  await page.evaluate(async () => {
    await new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve)))
    await Promise.all(document.getAnimations().filter(animation => animation.effect?.getTiming().iterations !== Infinity).map(animation => animation.finished.catch(() => {})))
  })
  if (page.viewportSize().width < 768) {
    await page.waitForFunction(() => {
      const frame = document.querySelector('[data-app-frame]')
      const sidebar = frame?.querySelector('[data-app-sidebar-column]')
      return frame?.hasAttribute('data-mobile') && !frame.hasAttribute('data-mobile-sidebar-open')
        && sidebar?.inert && sidebar.getBoundingClientRect().right <= 1
    })
  } else await page.locator('[data-app-frame]:not([data-mobile])').waitFor()
  await page.mouse.move(page.viewportSize().width - 4, 4)
  await page.evaluate(async () => {
    await Promise.all(document.getAnimations().filter(animation => animation.effect?.getTiming().iterations !== Infinity).map(animation => animation.finished.catch(() => {})))
  })
}

export async function nativeFixture(protocol, options = {}) {
  const requests = [], failures = [], discoveries = [], signatureChecks = []
  const gemini = protocol === 'google-gemini'
  const label = options.label ?? (gemini ? 'gemini' : 'claude')
  const apiKey = options.apiKey ?? 'native-fixture-key'
  const answer = options.answer ?? finalAnswer
  const expectedContent = options.expectedContent ?? 'Native fixture file'
  const signature = `${label}-fixture-signature`
  const server = createServer(async (incoming, response) => {
    try {
      assert.equal(incoming.headers[gemini ? 'x-goog-api-key' : 'x-api-key'], apiKey)
      assert.equal(incoming.headers.authorization, undefined)
      if (!gemini) assert.equal(incoming.headers['anthropic-version'], '2023-06-01')
      if (incoming.method === 'GET' && incoming.url === '/v1/models') {
        discoveries.push({ url: incoming.url, protocol })
        response.writeHead(200, { 'content-type': 'application/json' })
        response.end(JSON.stringify(gemini
          ? { models: [{ name: 'models/native-fixture', displayName: 'Native fixture', inputTokenLimit: 64000, outputTokenLimit: 8192, thinking: true, supportedGenerationMethods: ['generateContent'] }] }
          : { data: [{ id: 'native-fixture', display_name: 'Native fixture', max_input_tokens: 64000, max_tokens: 8192 }], has_more: false }))
        return
      }
      assert.equal(incoming.url, gemini ? '/v1/models/native-fixture:streamGenerateContent?alt=sse' : '/v1/messages')
      const chunks = []
      for await (const chunk of incoming) chunks.push(chunk)
      const body = JSON.parse(Buffer.concat(chunks).toString())
      const system = gemini ? body.systemInstruction?.parts?.[0]?.text : body.system
      const title = system?.includes('You name software-agent conversations')
      const messages = gemini ? body.contents : body.messages
      const latestUser = messages.findLastIndex(message => message.role === 'user' && (gemini
        ? message.parts?.some(part => typeof part.text === 'string')
        : typeof message.content === 'string' || message.content?.some(part => part.type === 'text')))
      const currentTurn = messages.slice(latestUser + 1)
      const followup = gemini
        ? currentTurn.some(message => message.parts?.some(part => part.functionResponse))
        : currentTurn.some(message => message.content?.some?.(part => part.type === 'tool_result'))
      if (!title) requests.push(body)
      if (followup) {
        const previous = currentTurn.find(message => message.role === (gemini ? 'model' : 'assistant'))
        assert.ok(previous, 'the tool response includes the preceding assistant message')
        const toolResult = gemini
          ? currentTurn.flatMap(message => message.parts ?? []).find(part => part.functionResponse)?.functionResponse.response.output
          : currentTurn.flatMap(message => message.content ?? []).find(part => part.type === 'tool_result')?.content
        if (gemini) assert.ok(previous.parts.some(part => part.thoughtSignature === signature), 'Gemini function-call thoughtSignature is passed back unchanged')
        else {
          assert.equal(previous.content[0].signature, signature, 'Claude thinking signature is passed back unchanged')
          assert.equal(previous.content[0].thinking, thinking)
        }
        assert.ok(toolResult?.includes(expectedContent), `tool result contains the actual Node file: ${toolResult}`)
        signatureChecks.push({ signature, toolResult })
      }
      response.writeHead(200, { 'content-type': 'text/event-stream' })
      if (gemini) {
        const parts = title ? [{ text: 'Native protocol test' }] : followup ? [{ text: answer }] : [
          { text: thinking, thought: true },
          { functionCall: { name: 'read_file', args: { path: 'fixture.txt' } }, thoughtSignature: signature },
        ]
        response.write(sse({ candidates: [{ content: { role: 'model', parts }, finishReason: 'STOP' }] }))
        response.end(sse({ usageMetadata: { promptTokenCount: 50, candidatesTokenCount: 10, thoughtsTokenCount: 5 } }))
      } else {
        response.write(sse({ type: 'message_start', message: { id: `msg-${label}`, model: 'native-fixture', usage: { input_tokens: 50, output_tokens: 1 } } }))
        const blocks = title ? [{ type: 'text', text: 'Native protocol test' }] : followup ? [{ type: 'text', text: answer }] : [
          { type: 'thinking', thinking, signature },
          { type: 'tool_use', id: `toolu-${label}-${requests.length}`, name: 'read_file', input: { path: 'fixture.txt' } },
        ]
        for (const [index, block] of blocks.entries()) {
          response.write(sse({ type: 'content_block_start', index, content_block: block }))
          response.write(sse({ type: 'content_block_stop', index }))
        }
        response.write(sse({ type: 'message_delta', delta: { stop_reason: title || followup ? 'end_turn' : 'tool_use' }, usage: { output_tokens: 15 } }))
        response.end(sse({ type: 'message_stop' }))
      }
    } catch (error) {
      failures.push(error.message)
      if (response.headersSent) response.end(sse({ type: 'error', error: { message: error.message } }))
      else response.writeHead(500, { 'content-type': 'application/json' }).end(JSON.stringify({ error: { message: error.message } }))
    }
  })
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve))
  return {
    baseUrl: `http://127.0.0.1:${server.address().port}/v1`, requests, failures, discoveries, signatureChecks, signature, apiKey, answer,
    close: () => new Promise(resolve => { server.closeAllConnections(); server.close(resolve) }),
  }
}
