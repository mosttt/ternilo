import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { createServer } from 'node:http'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { chromium } from 'playwright'
import { freePort, repository, serverRequest, startProcess, stopProcess, waitForHttp } from './platform-e2e-fixture.mjs'

async function until(read, ready, label) {
  const deadline = Date.now() + 20_000
  while (Date.now() < deadline) {
    const value = await read()
    if (ready(value)) return value
    await new Promise(resolve => setTimeout(resolve, 50))
  }
  throw new Error('Timed out: ' + label)
}

const writerSource = [
  'import pathlib, subprocess, sys, time',
  'if len(sys.argv) == 1:',
  '    child = subprocess.Popen([sys.executable, __file__, "descendant"])',
  '    child.wait()',
  'else:',
  '    while not pathlib.Path("fixture-stop").exists():',
  '        with open("heartbeat", "a", encoding="utf-8") as output:',
  '            output.write("tick\\n")',
  '        time.sleep(0.03)',
].join('\n')

test('browser Stop ends the managed Shell descendants and the next task remains usable', {
  timeout: 120_000, skip: process.platform === 'win32',
}, async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-process-browser-'))
  const workspacePath = path.join(directory, 'workspace')
  const artifacts = process.env.TERNILO_E2E_ARTIFACT_DIR ?? directory
  await mkdir(workspacePath)
  await mkdir(artifacts, { recursive: true })
  await writeFile(path.join(workspacePath, 'writer.py'), writerSource)
  const errors = [], assetHashes = {}
  let local, browser, page
  const sse = event => 'data: ' + JSON.stringify(event) + '\n\n'
  const model = createServer((request, response) => {
    const chunks = []
    request.on('data', chunk => chunks.push(chunk))
    request.on('end', () => {
      const body = JSON.parse(Buffer.concat(chunks).toString())
      const title = body.instructions?.includes('You name software-agent conversations')
      const input = body.input?.filter(item => item.role === 'user').at(-1)?.content
        ?.filter(item => item.type === 'input_text').map(item => item.text).join('\n') ?? ''
      const tool = !title && input.includes('start-controlled-writer')
      const text = title ? 'Process cleanup' : 'The next task works after cancellation.'
      const call = { type: 'function_call', call_id: 'controlled-shell', name: 'shell', arguments: JSON.stringify({ command: 'python3 writer.py', timeout_ms: 30_000 }) }
      const output = tool ? [call] : [{ type: 'message', role: 'assistant', content: [{ type: 'output_text', text }] }]
      response.writeHead(200, { 'content-type': 'text/event-stream' })
      if (tool) response.write(sse({ type: 'response.output_item.added', output_index: 0, item: { ...call, arguments: '' } })
        + sse({ type: 'response.function_call_arguments.done', output_index: 0, arguments: call.arguments }))
      else response.write(sse({ type: 'response.output_text.delta', output_index: 0, content_index: 0, delta: text }))
      response.end(sse({ type: 'response.completed', response: { status: 'completed', output, usage: { input_tokens: 20, output_tokens: 10 } } }))
    })
  })
  await new Promise(resolve => model.listen(0, '127.0.0.1', resolve))
  try {
    const origin = 'http://127.0.0.1:' + await freePort()
    local = startProcess(path.join(repository, 'target/debug/ternilo'), ['serve', '--listen', new URL(origin).host, '--data-dir', path.join(directory, 'data')])
    await waitForHttp(origin, local)
    for (const name of ['app.js', 'app.css']) {
      const response = await fetch(origin + '/assets/' + name)
      assert.equal(response.status, 200)
      const digest = bytes => createHash('sha256').update(bytes).digest('hex')
      assetHashes[name] = digest(Buffer.from(await response.arrayBuffer()))
      assert.equal(assetHashes[name], digest(await readFile(path.join(repository, 'web/dist/assets', name))))
    }
    const html = await (await fetch(origin)).text()
    const token = JSON.parse(html.match(/window\.__TERNILO_BOOT__\s*=\s*(\{[^<]+\});/)[1]).apiToken
    const api = (resource, options = {}) => serverRequest(origin, resource, { token, ...options })
    const workspace = await api('/workspaces', { body: { path: workspacePath } })
    const session = await api('/sessions', { body: { workspace_id: workspace.workspace_id } })
    const sessionId = session.identity.session_id
    await api('/credentials', { body: { name: 'PROCESS_FIXTURE_KEY', value: 'fixture-key' } })
    await api('/providers', { body: {
      id: 'process-fixture', display_name: 'Process fixture', base_url: 'http://127.0.0.1:' + model.address().port + '/v1',
      protocol: 'openai-responses', api_key_ref: 'PROCESS_FIXTURE_KEY',
      defaults: { context_window: 128000, max_output_tokens: 4096 },
      models: [{ id: 'fixture', settings: { mode: 'inherit' } }], timeout_ms: 60_000, max_attempts: 1, retry_base_delay_ms: 50,
    } })
    await api('/sessions/' + sessionId, { method: 'PATCH', body: { permissions: 'full_access', model: { provider: 'named_provider', provider_id: 'process-fixture', model: 'fixture' } } })
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
    page = await browser.newPage({ locale: 'zh-CN', viewport: { width: 1280, height: 900 }, serviceWorkers: 'block' })
    page.on('pageerror', error => errors.push('page: ' + error.message))
    page.on('console', message => { if (message.type() === 'error') errors.push('console: ' + message.text()) })
    page.on('response', response => { if (response.status() >= 400) errors.push('HTTP ' + response.status() + ' ' + new URL(response.url()).pathname) })
    page.on('requestfailed', request => { if (!request.failure()?.errorText.includes('ERR_ABORTED')) errors.push('request: ' + request.url() + ' ' + request.failure()?.errorText) })
    await page.goto(origin)
    await page.locator('[data-sidebar-session-row][data-session-id="' + sessionId + '"] [data-sidebar-session-button]').click()
    const input = page.getByRole('textbox', { name: '输入任务', exact: true })
    await input.fill('start-controlled-writer')
    await page.getByRole('button', { name: '发送', exact: true }).click()
    const heartbeat = () => readFile(path.join(workspacePath, 'heartbeat')).then(bytes => bytes.length).catch(error => { if (error.code === 'ENOENT') return 0; throw error })
    await until(heartbeat, bytes => bytes >= 20, 'the real descendant is writing')
    await page.getByRole('button', { name: '停止运行', exact: true }).click()
    await page.getByRole('button', { name: '发送', exact: true }).waitFor()
    await until(() => api('/sessions/' + sessionId + '/queue'), inbox => !inbox.active_run_id, 'cancelled run settles')
    await new Promise(resolve => setTimeout(resolve, 150))
    const stoppedSize = await heartbeat()
    await new Promise(resolve => setTimeout(resolve, 350))
    assert.equal(await heartbeat(), stoppedSize, 'Stop must end the descendant writer, not only its immediate parent')
    await input.fill('continue-after-cleanup')
    await page.getByRole('button', { name: '发送', exact: true }).click()
    await page.locator('article[data-role="assistant"]').filter({ hasText: 'The next task works after cancellation.' }).waitFor()
    await until(() => api('/sessions/' + sessionId + '/queue'), inbox => !inbox.active_run_id, 'subsequent run finishes')
    await page.setViewportSize({ width: 390, height: 844 })
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true)
    await page.screenshot({ path: path.join(artifacts, 'cancelled-shell-and-next-task-mobile.png'), animations: 'disabled' })
    assert.deepEqual(errors, [])
  } catch (error) {
    await page?.screenshot({ path: path.join(artifacts, 'process-cleanup-failure.png'), animations: 'disabled' }).catch(() => {})
    process.stderr.write(local?.diagnostics() ?? '')
    throw error
  } finally {
    await writeFile(path.join(workspacePath, 'fixture-stop'), '')
    await writeFile(path.join(artifacts, 'process-cleanup-observations.json'), JSON.stringify({ assetHashes, errors }, null, 2))
    await browser?.close()
    await stopProcess(local)
    model.closeAllConnections()
    await new Promise(resolve => model.close(resolve))
    await rm(directory, { recursive: true, force: true })
  }
})
