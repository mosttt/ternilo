import { history } from './history-loading-fixture.mjs'
import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { chromium } from 'playwright'
import { freePort, repository, startProcess, stopProcess, waitForHttp } from './platform-e2e-fixture.mjs'
import { localApi } from './model-device-fixture.mjs'


test('long reasoning history survives desktop, trajectory and mobile reload without partial replay renders', { timeout: 120_000 }, async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-history-'))
  const artifacts = process.env.TERNILO_E2E_ARTIFACT_DIR ?? directory
  await mkdir(artifacts, { recursive: true })
  const binary = process.env.TERNILO_E2E_NODE_BINARY ?? path.join(repository, 'target/debug/ternilo')
  const origin = `http://127.0.0.1:${await freePort()}`
  const data = path.join(directory, 'data')
  const args = ['serve', '--listen', new URL(origin).host, '--data-dir', data]
  let app, browser, page
  const errors = []
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
    await mkdir(path.join(data, 'sessions'), { recursive: true })
    await writeFile(path.join(data, 'sessions', `${Buffer.from(sessionId).toString('hex')}.jsonl`), expected.map(event => JSON.stringify(event)).join('\n') + '\n')
    app = startProcess(binary, args)
    await waitForHttp(origin, app)
    api = await localApi(origin)
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
    page = await browser.newPage({ viewport: { width: 1280, height: 900 }, hasTouch: true, serviceWorkers: 'block' })
    page.on('pageerror', error => errors.push(error.message))
    page.on('console', message => { if (message.type() === 'error') errors.push(message.text()) })
    page.on('response', response => { if (response.status() >= 400) errors.push(`HTTP ${response.status()} ${new URL(response.url()).pathname}`) })
    const started = Date.now()
    await page.goto(origin)
    await page.getByText('History preserved.', { exact: true }).waitFor()
    console.log(`Long history visible in ${Date.now() - started} ms (${expected.length} events)`)
    for (const asset of ['app.js', 'app.css']) {
      const served = await (await fetch(`${origin}/assets/${asset}`)).arrayBuffer()
      const built = await readFile(path.join(repository, 'web/dist/assets', asset))
      assert.equal(createHash('sha256').update(Buffer.from(served)).digest('hex'), createHash('sha256').update(built).digest('hex'))
    }
    assert.equal((await api(`/sessions/${sessionId}/events`)).length, expected.length)
    await page.locator('[data-turn-process]').click()
    const row = page.locator('[data-reasoning-row]')
    await row.getByRole('button').click()
    const text = await row.locator('[data-reasoning-body]').textContent()
    assert.equal(text, expected.filter(event => event.type === 'assistant_reasoning_delta').map(event => event.delta).join(''))
    await row.getByRole('button').click()
    await page.getByRole('tab', { name: '轨迹', exact: true }).click()
    await page.locator('[data-trajectory-state="ready"]').waitFor()
    await page.getByRole('tab', { name: '对话', exact: true }).click()
    await page.setViewportSize({ width: 390, height: 844 })
    await page.reload()
    await page.getByText('History preserved.', { exact: true }).waitFor()
    await page.locator('[data-turn-process]').tap()
    await row.getByRole('button').tap()
    assert.equal(await row.locator('[data-reasoning-body]').textContent(), text)
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true)
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
