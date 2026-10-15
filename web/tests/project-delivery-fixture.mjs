import assert from 'node:assert/strict'
import { createServer } from 'node:http'
export const fixedDocument = '<html lang="en"><head><style>body{font:18px/1.6 sans-serif;margin:16px;overflow-wrap:anywhere}section{max-width:100%;padding:12px;background:#e8f4ff;box-sizing:border-box}h1{font-size:24px}</style></head><body><section><h1>Delivery / 交付 / 전달</h1><p>Verified document layout.</p><p>内容完整，可阅读。내용이 온전히 표시됩니다.</p></section></body></html>'
export async function untilOperation(read, predicate, label) {
  const deadline = Date.now() + 90_000
  let value
  while (Date.now() < deadline) {
    value = await read()
    if (predicate(value)) return value
    await new Promise(resolve => setTimeout(resolve, 100))
  }
  throw new Error(`${label}: ${JSON.stringify(value).slice(-3000)}`)
}
export const rhai = value => value === null ? '()' : Array.isArray(value) ? `[${value.map(rhai).join(',')}]` : typeof value === 'object' ? `#{${Object.entries(value).map(([key, value]) => `${JSON.stringify(key)}:${rhai(value)}`).join(',')}}` : JSON.stringify(value)
async function listen(handler) {
  const server = createServer(handler)
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve))
  return { origin: `http://127.0.0.1:${server.address().port}`, close: () => new Promise(resolve => { server.closeAllConnections(); server.close(resolve) }) }
}
export async function githubFixture() {
  const state = { head: 'a'.repeat(40), flip: false, denyMerge: false, requests: [] }
  const server = await listen(async (request, response) => {
    const chunks = []; for await (const chunk of request) chunks.push(chunk)
    const body = chunks.length ? JSON.parse(Buffer.concat(chunks).toString()) : null
    const url = new URL(request.url, 'http://fixture')
    state.requests.push({ method: request.method, path: url.pathname, query: url.search, body, authorization: request.headers.authorization })
    const summary = { number: 1, html_url: 'https://example.invalid/pr/1', title: 'Fixture PR', state: 'open', draft: false, mergeable: true, head: { sha: state.head, ref: 'feature' }, base: { sha: 'c'.repeat(40), ref: 'main' } }
    const release = { id: 21, html_url: 'https://example.invalid/releases/v1', name: 'Stable', tag_name: 'v1', target_commitish: 'main', draft: false, prerelease: false, published_at: '2026-10-15T00:00:00Z' }
    let result = summary
    if (url.pathname.endsWith('/releases/tags/missing')) {
      response.writeHead(404, { 'content-type': 'application/json' }).end(JSON.stringify({ message: 'Not Found' })); return
    } else if (/\/releases\/\d+\/assets$/.test(url.pathname)) {
      if (url.searchParams.get('page') === '1') response.setHeader('link', '<http://example.invalid?page=2>; rel="next"')
      result = [{ id: Number(url.searchParams.get('page')), name: url.searchParams.get('page') === '1' ? 'app.tar.gz' : 'checksums.txt', state: 'uploaded', size: 8192, content_type: 'application/octet-stream', browser_download_url: 'https://example.invalid/asset', download_count: 3 }]
    } else if (url.pathname.endsWith('/releases')) {
      const draft = { ...release, id: 22, name: 'Draft', tag_name: 'v2', draft: true, prerelease: true, published_at: null }
      if (url.searchParams.get('page') === '1') {
        response.setHeader('link', '<http://example.invalid?page=2>; rel="next"')
        result = [release, draft]
      } else result = [{ ...release, id: 20, tag_name: 'v0' }]
    } else if (url.pathname.includes('/releases/')) result = url.pathname.endsWith('/22') ? { ...release, id: 22, draft: true, prerelease: true, published_at: null } : release
    else if (url.pathname.endsWith('/files')) {
      response.setHeader('link', '<http://example.invalid?page=2>; rel="next"')
      result = [{ filename: 'selected.txt', status: 'modified', patch: '@@ -1 +1 @@\n-before\n+after' }]
      if (state.flip) state.head = 'b'.repeat(40)
    } else if (url.pathname.endsWith('/check-runs')) result = { total_count: 1, check_runs: [{ name: 'Tests', status: 'completed', conclusion: 'success' }] }
    else if (url.pathname.endsWith('/status')) result = { state: 'success', statuses: [] }
    else if (url.pathname.endsWith('/merge')) result = { merged: !state.denyMerge, sha: state.head, message: state.denyMerge ? 'Not merged' : 'Merged fixture' }
    else if (request.method === 'POST') result = { ...summary, title: body.title, body: body.body, draft: body.draft }
    else if (url.pathname.endsWith('/pulls')) result = [summary]
    response.writeHead(200, { 'content-type': 'application/json' }).end(JSON.stringify(result))
  })
  return { ...server, state }
}
export async function documentModel() {
  const requests = []
  const server = await listen(async (request, response) => {
    const chunks = []; for await (const chunk of request) chunks.push(chunk)
    const body = JSON.parse(Buffer.concat(chunks).toString()); requests.push(body)
    const system = String(body.messages.find(message => message.role === 'system')?.content)
    const title = system.startsWith('You name software-agent conversations')
    const results = new Map(body.messages.filter(message => message.role === 'tool').map(message => [message.tool_call_id, message.content]))
    let call, content = title ? 'Document delivery' : 'Document delivery verified.'
    try {
      if (!title) {
        const before = () => JSON.parse(results.get('doc-bad'))
        const after = () => JSON.parse(results.get('doc-good'))
        if (!results.has('doc-bad')) call = ['doc-bad', 'render_document', { path: 'delivery.html', width: 320, height: 560 }]
        else if (!results.has('doc-reject')) call = ['doc-reject', 'record_document_review', { render_id: before().render_id, verdict: 'pass', findings: ['Attempted review before fixing overflow.'] }]
        else if (!results.has('doc-fix')) call = ['doc-fix', 'write_file', { path: 'delivery.html', content: fixedDocument }]
        else if (!results.has('doc-stale')) call = ['doc-stale', 'record_document_review', { render_id: before().render_id, verdict: 'pass', findings: ['Attempted review of a stale screenshot.'] }]
        else if (!results.has('doc-good')) call = ['doc-good', 'render_document', { path: 'delivery.html', width: 320, height: 560 }]
        else if (!results.has('doc-reviewed')) call = ['doc-reviewed', 'record_document_review', { render_id: after().render_id, verdict: 'pass', findings: ['All text fits the viewport; the screenshot includes the multilingual content.'] }]
      }
    } catch (error) { response.writeHead(500, { 'content-type': 'application/json' }).end(JSON.stringify({ error: { message: `Invalid render fixture result: ${error.message}; ${[...results.values()].join('\n').slice(-1500)}` } })); return }
    const delta = call ? { tool_calls: [{ index: 0, id: call[0], type: 'function', function: { name: call[1], arguments: JSON.stringify(call[2]) } }] } : { content }
    const usage = { prompt_tokens: 150, completion_tokens: 30, total_tokens: 180 }
    if (!body.stream) { response.writeHead(200, { 'content-type': 'application/json' }).end(JSON.stringify({ id: 'document-title', model: body.model, choices: [{ index: 0, message: { role: 'assistant', content }, finish_reason: 'stop' }], usage })); return }
    const event = value => `data: ${JSON.stringify(value)}\n\n`
    response.writeHead(200, { 'content-type': 'text/event-stream' }).end(event({ choices: [{ index: 0, delta, finish_reason: null }] }) + event({ choices: [{ index: 0, delta: {}, finish_reason: call ? 'tool_calls' : 'stop' }] }) + event({ choices: [], usage }) + 'data: [DONE]\n\n')
  })
  return { ...server, requests }
}

export function deliveryOperations(request, id) {
  const run = async input => {
    const resource = `/sessions/${id}`
    const before = (await request(`${resource}/events`)).at(-1)?.seq ?? -1
    await request(`${resource}/queue`, { body: { content: { kind: 'prompt', input } } })
    const events = await untilOperation(async () => {
      for (const pending of await request(`/questions?session_id=${id}`)) await request(`/questions/${pending.question.id}/answer?session_id=${id}`, { body: { selected: ['Allow once'], custom: null } })
      return request(`${resource}/events`)
    }, values => values.some(event => event.seq > before && ['turn_finished', 'turn_failed', 'turn_cancelled'].includes(event.type)), 'completed delivery operation').catch(async error => { error.message += `\nQueue: ${JSON.stringify(await request(`${resource}/queue`))}\nSession: ${JSON.stringify((await request('/state')).sessions.find(session => session.identity.session_id === id))}`; throw error })
    await untilOperation(() => request(`${resource}/queue`), queue => !queue.active_run_id, 'idle delivery queue')
    return events.filter(event => event.seq > before)
  }
  const tool = async (name, args, expectedError = false) => {
    const events = await run(`/code call_tool(${JSON.stringify(name)}, ${rhai(args)})`)
    const output = events.findLast(event => event.type === 'code_dispatch_finished' && event.name === name)?.output
    if (!output || output.is_error !== expectedError) throw new Error(`${name}: ${output?.content ?? JSON.stringify(events).slice(-3000)}`)
    return expectedError ? output.content : JSON.parse(output.content)
  }
  return { run, tool }
}

export async function verifyReleaseQueries(tool, github) {
  const start = github.state.requests.length
  const first = await tool('github_list_releases', { repository: 'fixture/repo' })
  assert.equal(first.has_more, true)
  assert.equal(first.releases[0].tag, 'v1')
  assert.equal(first.releases[1].draft, true)
  assert.equal(first.releases[1].prerelease, true)
  const last = await tool('github_list_releases', { repository: 'fixture/repo', page: 2 })
  assert.equal(last.has_more, false)
  const latest = await tool('github_release_status', { repository: 'fixture/repo' })
  assert.equal(latest.release.draft, false)
  assert.equal(latest.release.published_at, '2026-10-15T00:00:00Z')
  assert.equal(latest.assets[0].size, 8192)
  assert.equal(latest.assets[0].state, 'uploaded')
  assert.equal(latest.has_more_assets, true)
  const assets = await tool('github_release_status', { repository: 'fixture/repo', release_id: latest.release.id, assets_page: 2 })
  assert.equal(assets.has_more_assets, false)
  assert.equal(assets.assets[0].name, 'checksums.txt')
  const draft = await tool('github_release_status', { repository: 'fixture/repo', release_id: 22 })
  assert.equal(draft.release.draft, true)
  assert.equal(draft.release.published_at, null)
  assert.equal((await tool('github_release_status', { repository: 'fixture/repo', tag: 'series/v1' })).release.id, 21)
  assert.match(await tool('github_release_status', { repository: 'fixture/repo', tag: 'missing' }, true), /404/)
  const sent = github.state.requests.length
  await tool('github_release_status', { repository: 'fixture/repo', tag: 'v1', release_id: 21 }, true)
  assert.equal(github.state.requests.length, sent, 'invalid selector does not call GitHub')
  const requests = github.state.requests.slice(start)
  assert.ok(requests.every(request => request.method === 'GET'))
  assert.ok(requests.some(request => request.path.endsWith('/releases/latest')))
  assert.ok(requests.some(request => request.path.endsWith('/tags/series%2Fv1')))
}


export async function verifyDocumentMath(tool) {
  await tool('write_file', { path: 'formulas.md', content: String.raw`Invalid $\notACommand{x}$.` })
  assert.match(await tool('render_document', { path: 'formulas.md', width: 320, height: 560 }, true), /Formula 1/)
  await tool('write_file', { path: 'formulas.md', content: String.raw`$\href{https://example.invalid/blocked}{link}$` })
  assert.match(await tool('render_document', { path: 'formulas.md', width: 320, height: 560 }, true), /not supported in document formulas/)
  await tool('write_file', { path: 'formulas.md', content: '$$' + 'x+'.repeat(60) + '1$$' })
  const wide = await tool('render_document', { path: 'formulas.md', width: 320, height: 560 })
  assert.equal(wide.horizontal_overflow, true)
  assert.match(await tool('record_document_review', { render_id: wide.render_id, verdict: 'pass', findings: ['Should not pass with a clipped formula.'] }, true), /overflow/)
  await tool('write_file', { path: 'formulas.md', content: String.raw`# 数学 / Mathematics / 수학

Inline / 行内 / 인라인: $E=mc^2$.

$$\frac{1}{2}+\frac{1}{3}=\frac{5}{6}$$

$$\begin{pmatrix}1 & 2 \\ 3 & 4\end{pmatrix}$$

$$\sum_{n=1}^{\infty}\frac{1}{n^2}=\frac{\pi^2}{6}$$

Literal code: ` + '`$x^2$`' })
  for (const width of [1024, 320]) {
    const rendered = await tool('render_document', { path: 'formulas.md', width, height: 560 })
    assert.equal(rendered.horizontal_overflow, false)
    assert.equal(rendered.missing_images, 0)
    assert.equal(rendered.blocked_resources, 0)
    await tool('record_document_review', { render_id: rendered.render_id, verdict: 'pass', findings: ['Inline and display formulas, fractions, matrices and summation fit the viewport.'] })
  }
}
