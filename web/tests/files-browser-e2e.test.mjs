import { selectChoice } from './browser-select-fixture.mjs'
import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { createServer } from 'node:http'
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { chromium } from 'playwright'
import { freePort, repository, startProcess, stopProcess, waitForHttp, serverRequest } from './platform-e2e-fixture.mjs'

async function until(read, predicate, label) {
  const deadline = Date.now() + 30_000
  let value
  while (Date.now() < deadline) {
    value = await read()
    if (predicate(value)) return value
    await new Promise(resolve => setTimeout(resolve, 50))
  }
  throw new Error(`${label}: ${JSON.stringify(value)}`)
}

async function downloadedBytes(page, button) {
  const received = page.waitForEvent('download')
  await button.click()
  const download = await received
  assert.equal(await download.failure(), null)
  return { name: download.suggestedFilename(), bytes: await readFile(await download.path()) }
}

test('Local Files preserves upload bytes, immutable versions, filters, pagination and mobile navigation', { timeout: 180_000 }, async t => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-files-browser-'))
  const artifacts = process.env.TERNILO_E2E_ARTIFACT_DIR ?? directory
  await mkdir(artifacts, { recursive: true })
  const origin = `http://127.0.0.1:${await freePort()}`
  const local = startProcess(process.env.TERNILO_E2E_LOCAL_BINARY ?? path.join(repository, 'target/debug/ternilo'), ['serve', '--listen', new URL(origin).host, '--data-dir', path.join(directory, 'data')])
  const model = createServer((request, response) => {
    request.resume()
    request.on('end', () => {
      response.writeHead(200, { 'content-type': 'text/event-stream' })
      response.write(`data: ${JSON.stringify({ type: 'response.output_text.delta', output_index: 0, content_index: 0, delta: 'Files fixture' })}\n\n`)
      response.end(`data: ${JSON.stringify({ type: 'response.completed', response: { status: 'completed', output: [{ type: 'message', role: 'assistant', content: [{ type: 'output_text', text: 'Files fixture' }] }], usage: { input_tokens: 1, output_tokens: 1 } } })}\n\n`)
    })
  })
  await new Promise((resolve, reject) => { model.once('error', reject); model.listen(0, '127.0.0.1', resolve) })
  let browser
  const observations = { pageErrors: [], consoleErrors: [], httpErrors: [], failedRequests: [], assetHashes: {} }
  try {
    await waitForHttp(origin, local)
    for (const asset of ['app.js', 'app.css']) {
      const response = await fetch(`${origin}/assets/${asset}`)
      assert.equal(response.status, 200)
      const hash = bytes => createHash('sha256').update(bytes).digest('hex')
      observations.assetHashes[asset] = hash(Buffer.from(await response.arrayBuffer()))
      assert.equal(observations.assetHashes[asset], hash(await readFile(path.join(repository, 'web/dist/assets', asset))), `Local embeds current ${asset}`)
    }
    const html = await (await fetch(origin)).text()
    const token = JSON.parse(html.match(/window\.__TERNILO_BOOT__\s*=\s*(\{[^<]+\});/)[1]).apiToken
    const api = (resource, body, method) => serverRequest(origin, resource, { token, body, method })
    await api('/credentials', { name: 'TERNILO_PROVIDER_FILES_API_KEY', value: 'fixture-key' })
    await api('/providers', {
      id: 'files-fixture', display_name: 'Files Fixture', base_url: `http://127.0.0.1:${model.address().port}/v1`,
      protocol: 'openai-responses', api_key_ref: 'TERNILO_PROVIDER_FILES_API_KEY',
      defaults: { context_window: 128000, max_output_tokens: 8192 },
      models: [{ id: 'fixture-model', display_name: 'Fixture Model', settings: { mode: 'inherit' } }],
      timeout_ms: 30000, max_attempts: 1, retry_base_delay_ms: 50,
    })
    const workspaces = []
    const sessions = []
    for (const suffix of ['alpha', 'beta']) {
      const workspacePath = path.join(directory, suffix)
      await mkdir(workspacePath)
      const workspace = await api('/workspaces', { path: workspacePath })
      const session = await api('/sessions', { workspace_id: workspace.workspace_id, session_id: `files-${suffix}` })
      await api(`/sessions/${session.identity.session_id}`, { title: `Files ${suffix}`, model: { provider: 'named_provider', provider_id: 'files-fixture', model: 'fixture-model' } }, 'PATCH')
      workspaces.push(workspace)
      sessions.push(session.identity.session_id)
    }
    const turn = async (session, input, attachments = []) => {
      await api(`/sessions/${session}/queue`, { content: { kind: 'prompt', input }, attachments })
      await until(() => api(`/sessions/${session}/queue`), queue => !queue.active_run_id && !queue.items.some(item => item.placement === 'queued'), 'command completes')
    }
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
    const context = await browser.newContext({ viewport: { width: 1440, height: 960 }, serviceWorkers: 'block' })
    const page = await context.newPage()
    page.on('pageerror', error => observations.pageErrors.push(error.message))
    page.on('console', message => { if (message.type() === 'error') observations.consoleErrors.push(message.text()) })
    page.on('response', response => { if (response.status() >= 400) observations.httpErrors.push(`${response.status()} ${response.request().method()} ${response.url()}`) })
    page.on('requestfailed', request => { if (request.failure()?.errorText !== 'net::ERR_ABORTED') observations.failedRequests.push(`${request.method()} ${request.url()}: ${request.failure()?.errorText}`) })
    await page.goto(origin)
    const alpha = page.locator('[data-sidebar-workspace-group]').filter({ has: page.locator('[data-sidebar-workspace-title]').filter({ hasText: /^alpha$/ }) })
    const toggleAlpha = alpha.locator('[data-sidebar-workspace-button]')
    if (await toggleAlpha.getAttribute('aria-expanded') === 'false') await toggleAlpha.click()
    await alpha.locator(`[data-sidebar-session-row][data-session-id="${sessions[0]}"] [data-sidebar-session-button]`).click()
    await alpha.locator(`[data-session-id="${sessions[0]}"] [data-sidebar-session-active]`).waitFor()
    await page.waitForFunction(expected => document.querySelector('[data-session-workspace-path]')?.textContent === expected, workspaces[0].path)
    assert.equal((await page.reload({ waitUntil: 'networkidle' })).status(), 200)
    await alpha.locator(`[data-session-id="${sessions[0]}"] [data-sidebar-session-active]`).waitFor()
    await page.waitForFunction(expected => document.querySelector('[data-session-workspace-path]')?.textContent === expected, workspaces[0].path)
    const literal = Buffer.from('\ufeffdata:text/plain;base64,SGVsbG8=\r\n中文与 emoji 📄\r\n')
    await page.locator('input[type="file"]').setInputFiles([
      { name: 'literal-upload.txt', mimeType: 'text/plain', buffer: literal },
      { name: 'pixel.png', mimeType: 'image/png', buffer: Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jfJkAAAAASUVORK5CYII=', 'base64') },
    ])
    const pendingImage = page.getByRole('button', { name: '预览 pixel.webp', exact: true }).locator('img')
    await pendingImage.waitFor()
    const normalizedImage = Buffer.from((await pendingImage.getAttribute('src')).split(',')[1], 'base64')
    await page.getByRole('textbox', { name: '输入任务', exact: true }).fill('Keep these uploaded files for the Files library')
    await page.getByRole('button', { name: '发送', exact: true }).click()
    await until(() => api('/files'), result => result.items.some(file => file.name === 'literal-upload.txt'), 'UI uploads enter inventory')
    await until(() => api(`/sessions/${sessions[0]}/queue`), queue => !queue.active_run_id && !queue.items.some(item => item.placement === 'queued'), 'UI turn completes')
    await page.locator('article[data-role="assistant"]').filter({ hasText: 'Files fixture' }).waitFor()
    await turn(sessions[0], '/write report.txt first-version')
    await turn(sessions[0], '/write report.txt second-version')
    await turn(sessions[1], '/write beta.txt beta-content')

    await page.locator('[data-session-files]').click()
    assert.equal(new URL(page.url()).pathname, '/files')
    assert.equal(new URL(page.url()).searchParams.get('session_id'), sessions[0])
    await page.locator('[data-file-id]').first().waitFor()
    assert.equal(await page.locator('[data-file-id]').count(), 4)
    assert.equal(await page.locator('[data-app-frame]:visible').count(), 0, 'Files uses its independent page shell')
    const uploaded = page.locator('[data-file-id]').filter({ hasText: 'literal-upload.txt' })
    await uploaded.getByRole('button', { name: '预览 literal-upload.txt', exact: true }).click()
    await page.getByRole('dialog').getByLabel('文件文本内容', { exact: true }).waitFor()
    assert.match(await page.locator('[data-file-preview] pre').textContent(), /data:text\/plain;base64,SGVsbG8=/)
    const uploadedDownload = await downloadedBytes(page, page.getByRole('dialog').getByRole('button', { name: '下载', exact: true }))
    assert.equal(uploadedDownload.name, 'literal-upload.txt')
    assert.deepEqual(uploadedDownload.bytes, literal, 'BOM, CRLF, data URL text and UTF-8 survive upload and download')
    await page.getByRole('button', { name: '关闭预览', exact: true }).click()
    await page.getByRole('button', { name: '预览 pixel.webp', exact: true }).click()
    await page.waitForFunction(() => document.querySelector('[data-file-preview] img')?.naturalWidth === 1)
    const imageDownload = await downloadedBytes(page, page.getByRole('dialog').getByRole('button', { name: '下载', exact: true }))
    assert.deepEqual(imageDownload.bytes, normalizedImage, 'the retained normalized upload is downloaded without re-encoding')
    await page.getByRole('button', { name: '关闭预览', exact: true }).click()
    const generated = page.locator('[data-file-id]').filter({ hasText: 'report.txt' })
    assert.equal(await generated.count(), 2)
    const versions = []
    for (let index = 0; index < 2; index++) {
      const result = await downloadedBytes(page, generated.nth(index).getByRole('button', { name: '下载 report.txt', exact: true }))
      versions.push(result.bytes.toString('utf8'))
    }
    assert.deepEqual(versions.sort(), ['first-version', 'second-version'])
    assert.equal(await readFile(path.join(directory, 'alpha/report.txt'), 'utf8'), 'second-version')
    await page.locator('[data-files-navigation] [data-file-kind="upload"]').click()
    await until(() => page.locator('[data-file-id]').count(), count => count === 2, 'kind filter')
    await page.getByRole('button', { name: '清除筛选', exact: true }).click()
    await page.locator(`[data-files-navigation] [data-file-workspace="${workspaces[1].workspace_id}"]`).click()
    await until(() => page.locator('[data-file-id]').count(), count => count === 1, 'workspace filter')
    assert.match(await page.locator('[data-file-id]').textContent(), /beta.txt/)
    await page.getByRole('button', { name: '清除筛选', exact: true }).click()
    await page.getByRole('searchbox', { name: '搜索文件名', exact: true }).fill('literal-upload')
    await page.getByRole('button', { name: '搜索', exact: true }).click()
    await until(() => page.locator('[data-file-id]').count(), count => count === 1, 'filename search')
    assert.equal(new URL(page.url()).searchParams.get('query'), 'literal-upload')
    assert.equal((await page.reload({ waitUntil: 'networkidle' })).status(), 200, 'Local serves the independent Files route on reload')
    assert.equal(await page.getByRole('searchbox', { name: '搜索文件名', exact: true }).inputValue(), 'literal-upload')
    await page.getByRole('button', { name: '打开原会话', exact: true }).click()
    await page.locator('[data-session-files]').waitFor()
    assert.equal(new URL(page.url()).pathname, '/')
    await page.locator('[data-sidebar-files]').click()
    await page.locator('[data-files-shell]').waitFor()
    assert.equal(new URL(page.url()).search, '')

    for (let batch = 0; batch < 11; batch++) {
      const attachments = Array.from({ length: batch === 10 ? 5 : 10 }, (_, index) => ({ name: `paged-${String(batch * 10 + index).padStart(3, '0')}.txt`, media_type: 'text/plain', content: `page item ${batch * 10 + index}` }))
      await turn(sessions[0], `Retain uploaded batch ${batch}`, attachments)
    }
    await page.goto(`${origin}/files?kind=upload&query=paged-`)
    await page.getByRole('button', { name: '加载更多', exact: true }).waitFor()
    assert.equal(await page.locator('[data-file-id]').count(), 100)
    await page.getByRole('button', { name: '加载更多', exact: true }).click()
    await until(() => page.locator('[data-file-id]').count(), count => count === 105, 'second cursor page')
    assert.equal(new Set(await page.locator('[data-file-id]').evaluateAll(rows => rows.map(row => row.dataset.fileId))).size, 105)
    assert.equal(await page.getByRole('button', { name: '加载更多', exact: true }).count(), 0)
    await page.screenshot({ path: path.join(artifacts, 'files-local-desktop.png'), fullPage: true, animations: 'disabled' })

    await api(`/sessions/${sessions[0]}/archive`, {}, 'POST')
    await page.goto(`${origin}/files?session_id=${sessions[0]}&query=report.txt`)
    await until(() => page.locator('[data-file-id]').count(), count => count === 2, 'archived files remain available')
    assert.equal(await page.getByRole('button', { name: '打开原会话', exact: true }).count(), 0)
    await page.getByRole('button', { name: '查看会话文件', exact: true }).first().click()
    assert.equal(new URL(page.url()).pathname, '/files')
    assert.equal(new URL(page.url()).searchParams.get('session_id'), sessions[0])
    await page.goto(`${origin}/files?session_id=${sessions[0]}&query=report.txt`)
    await page.setViewportSize({ width: 390, height: 844 })
    const mobileFilters = page.locator('[data-files-mobile-filters]')
    await mobileFilters.locator('summary').click()
    await selectChoice(mobileFilters.getByLabel('文件类型', { exact: true }), 'upload')
    await page.getByText('没有符合条件的文件。', { exact: true }).waitFor()
    await selectChoice(mobileFilters.getByLabel('文件类型', { exact: true }), 'generated')
    await until(() => page.locator('[data-file-id]').count(), count => count === 2, 'mobile file kind filter')
    assert.equal(new URL(page.url()).searchParams.get('kind'), 'generated')
    assert.equal(await mobileFilters.getByLabel('会话', { exact: true }).inputValue(), sessions[0], 'archived session filter is retained on mobile')
    await page.screenshot({ path: path.join(artifacts, 'files-local-mobile-filters.png') })
    await mobileFilters.locator('summary').click()
    await page.getByRole('button', { name: '预览 report.txt', exact: true }).first().click()
    await page.locator('[data-file-preview] pre').waitFor()
    const dimensions = await page.evaluate(() => ({ viewport: innerWidth, width: document.documentElement.scrollWidth, dialog: document.querySelector('[role="dialog"]').getBoundingClientRect().toJSON() }))
    assert.ok(dimensions.width <= dimensions.viewport, JSON.stringify(dimensions))
    assert.ok(dimensions.dialog.x >= 0 && dimensions.dialog.right <= dimensions.viewport, JSON.stringify(dimensions))
    await page.screenshot({ path: path.join(artifacts, 'files-local-mobile-preview.png'), fullPage: true, animations: 'disabled' })
    const mobileDownload = await downloadedBytes(page, page.getByRole('dialog').getByRole('button', { name: '下载', exact: true }))
    assert.equal(mobileDownload.bytes.toString('utf8'), 'second-version')
    await page.getByRole('button', { name: '关闭预览', exact: true }).click()
    await page.getByRole('link', { name: '返回工作台', exact: true }).last().click()
    assert.equal(new URL(page.url()).pathname, '/')
    await page.locator('[data-app-frame]:visible').waitFor()
    await page.evaluate(() => localStorage.setItem('ternilo.locale', 'en'))
    await page.goto(`${origin}/files?query=absent-file-name`)
    await page.getByText('No files match these filters.', { exact: true }).waitFor()
    assert.equal(await page.getByRole('heading', { name: 'Files', exact: true }).count(), 1)
    assert.equal(await page.getByRole('searchbox', { name: 'Search filenames', exact: true }).inputValue(), 'absent-file-name')
    await page.screenshot({ path: path.join(artifacts, 'files-local-mobile-english-empty.png'), fullPage: true, animations: 'disabled' })
    assert.deepEqual(observations.pageErrors, [])
    assert.deepEqual(observations.consoleErrors, [])
    assert.deepEqual(observations.httpErrors, [])
    assert.deepEqual(observations.failedRequests, [])
    t.diagnostic(JSON.stringify(observations))
  } catch (error) {
    if (browser) for (const context of browser.contexts()) for (const page of context.pages()) await page.screenshot({ path: path.join(artifacts, 'files-local-failure.png'), fullPage: true, animations: 'disabled' }).catch(() => {})
    throw error
  } finally {
    await writeFile(path.join(artifacts, 'files-local-observations.json'), JSON.stringify(observations, null, 2))
    if (browser) await browser.close()
    model.closeAllConnections()
    await new Promise(resolve => model.close(resolve))
    await stopProcess(local)
    await rm(directory, { recursive: true, force: true })
  }
})
