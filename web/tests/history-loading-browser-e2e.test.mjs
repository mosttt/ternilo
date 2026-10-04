import { history } from './history-loading-fixture.mjs'
import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { chromium } from 'playwright'
import { freePort, repository, startProcess, stopProcess, waitForHttp } from './platform-e2e-fixture.mjs'
import { localApi, until } from './model-device-fixture.mjs'


test('a long thinking round loads completely, switches from cache and resumes Live on desktop and mobile', { timeout: 120_000 }, async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-history-'))
  const artifacts = process.env.TERNILO_E2E_ARTIFACT_DIR ?? directory
  await mkdir(artifacts, { recursive: true })
  const binary = process.env.TERNILO_E2E_NODE_BINARY ?? path.join(repository, 'target/debug/ternilo')
  const origin = `http://127.0.0.1:${await freePort()}`
  const data = path.join(directory, 'data')
  const args = ['serve', '--listen', new URL(origin).host, '--data-dir', data]
  let app, browser, page
  const errors = [], subscriptions = [], streamed = [], historyRequests = []
  try {
    app = startProcess(binary, args)
    await waitForHttp(origin, app)
    let api = await localApi(origin)
    const folder = path.join(directory, 'workspace')
    await mkdir(folder)
    const workspace = await api('/workspaces', { body: { path: folder } })
    const session = await api('/sessions', { body: { workspace_id: workspace.workspace_id } })
    const sessionId = session.identity.session_id
    await stopProcess(app)
    const expected = history()
    await mkdir(path.join(data, 'data', 'sessions'), { recursive: true })
    await writeFile(path.join(data, 'data', 'sessions', `${Buffer.from(sessionId).toString('hex')}.jsonl`), expected.map(event => JSON.stringify(event)).join('\n') + '\n')
    app = startProcess(binary, args)
    await waitForHttp(origin, app)
    api = await localApi(origin)
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
    page = await browser.newPage({ locale: 'zh-CN', viewport: { width: 1280, height: 900 }, hasTouch: true, serviceWorkers: 'block' })
    page.on('websocket', socket => {
      socket.on('framesent', ({ payload }) => { const frame = JSON.parse(String(payload)); if (frame.type === 'subscribe') subscriptions.push(frame) })
      socket.on('framereceived', ({ payload }) => { const frame = JSON.parse(String(payload)); if (frame.type === 'event_batch' && frame.session_id === sessionId) streamed.push(...frame.events) })
    })
    page.on('pageerror', error => errors.push(error.message))
    page.on('request', request => {
      const url = new URL(request.url())
      if (url.pathname.endsWith('/history')) historyRequests.push(url)
    })
    page.on('console', message => { if (message.type() === 'error') errors.push(message.text()) })
    page.on('response', response => { if (response.status() >= 400) errors.push(`HTTP ${response.status()} ${new URL(response.url()).pathname}`) })
    const started = Date.now()
    await page.goto(origin)
    await page.getByText('History preserved.', { exact: true }).waitFor()
    console.log(`Long history visible in ${Date.now() - started} ms (${expected.length} events)`)
    for (const asset of ['app.js', 'app.css']) {
      const served = await (await fetch(`${origin}/assets/${asset}`)).arrayBuffer()
      const built = await readFile(path.join(process.env.TERNILO_E2E_ASSET_DIR ?? path.join(repository, 'web/dist/assets'), asset))
      assert.equal(createHash('sha256').update(Buffer.from(served)).digest('hex'), createHash('sha256').update(built).digest('hex'))
    }
    assert.equal((await api(`/sessions/${sessionId}/events`)).length, expected.length)
    const row = page.locator('[data-reasoning-row]')
    await row.getByRole('button').click()
    const text = expected.filter(event => event.type === 'assistant_reasoning_delta').map(event => event.delta).join('')
    await until(() => row.locator('[data-reasoning-body]').textContent(), value => value === text, 'complete thinking round')
    await until(async () => subscriptions.at(-1), frame => frame?.after_seq === expected.length - 1, 'live resumes after the bounded page')
    assert.equal(await page.getByRole('button', { name: '加载更早', exact: true }).count(), 0, 'one thinking round must not be cut into pages')
    const other = await api('/sessions', { body: { workspace_id: workspace.workspace_id } })
    await api(`/sessions/${other.identity.session_id}/commands/feedback`, { body: { text: 'Another conversation' } })
    await page.locator(`[data-session-id="${other.identity.session_id}"] [data-sidebar-session-button]`).click()
    await until(async () => subscriptions.at(-1), frame => frame?.session_id === other.identity.session_id, 'other session loaded')
    const beforeSwitch = historyRequests.length
    const switchStarted = Date.now()
    await page.locator(`[data-session-id="${sessionId}"] [data-sidebar-session-button]`).click()
    await page.getByText('History preserved.', { exact: true }).waitFor({ timeout: 2_000 })
    await until(async () => subscriptions.at(-1), frame => frame?.session_id === sessionId, 'cached session resumes Live')
    assert.equal(historyRequests.length, beforeSwitch, 'switching back must not reread long history')
    console.log(`Cached long conversation visible in ${Date.now() - switchStarted} ms without REST history`)
    const seen = []
    let before
    do {
      const result = await api(`/sessions/${sessionId}/history?limit=1000${before === undefined ? '' : `&before_seq=${before}`}`)
      assert.ok(result.events.length <= 1000)
      seen.unshift(...result.events.map(event => event.seq))
      before = result.next_before_seq
    } while (before !== null)
    assert.deepEqual(seen, expected.map(event => event.seq))
    const turnProcess = page.locator('[data-turn-process]')
    if (await turnProcess.getAttribute('aria-expanded') === 'false') await turnProcess.click()
    await row.getByRole('button').click()
    await page.getByRole('tab', { name: '轨迹', exact: true }).click()
    await page.locator('[data-trajectory-state="ready"]').waitFor()
    await page.getByRole('tab', { name: '对话', exact: true }).click()
    await page.setViewportSize({ width: 390, height: 844 })
    await page.reload()
    await page.getByText('History preserved.', { exact: true }).waitFor()
    if (await turnProcess.count() && await turnProcess.getAttribute('aria-expanded') === 'false') await turnProcess.tap()
    await row.getByRole('button').tap()
    await until(() => row.locator('[data-reasoning-body]').textContent(), value => value === text, 'complete mobile thinking round')
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true)
    const receipt = await api(`/sessions/${sessionId}/commands/feedback`, { body: { text: 'Bounded history live continuity' } })
    await until(async () => streamed, events => receipt.events.every(event => events.some(item => item.seq === event.seq)), 'new canonical events after paging and reload')
    assert.deepEqual(streamed.map(event => event.seq), receipt.events.map(event => event.seq))
    await page.screenshot({ path: path.join(artifacts, 'history-mobile.png') })
    assert.deepEqual(errors, [])
  } catch (error) {
    await page?.screenshot({ path: path.join(artifacts, 'history-failure.png') }).catch(() => {})
    error.message += `\n${app?.diagnostics() ?? ''}`
    throw error
  } finally {
    await browser?.close()
    if (app) await stopProcess(app)
    await rm(directory, { recursive: true, force: true })
  }
})
