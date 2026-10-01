import assert from 'node:assert/strict'
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { chromium } from 'playwright'
import { freePort, repository, startProcess, stopProcess, waitForHttp } from './platform-e2e-fixture.mjs'
import { localApi } from './model-device-fixture.mjs'

test('ordinary histories load completely and a mobile virtual trajectory stays stable while swiping', { timeout: 90_000 }, async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-trajectory-scroll-'))
  const artifacts = process.env.TERNILO_E2E_ARTIFACT_DIR ?? directory
  await mkdir(artifacts, { recursive: true })
  const origin = `http://127.0.0.1:${await freePort()}`
  const data = path.join(directory, 'data')
  const binary = process.env.TERNILO_E2E_NODE_BINARY ?? path.join(repository, 'target/debug/ternilo')
  const args = ['serve', '--listen', new URL(origin).host, '--data-dir', data]
  let app, browser, page
  const errors = []
  try {
    app = startProcess(binary, args)
    await waitForHttp(origin, app)
    const api = await localApi(origin)
    const folder = path.join(directory, 'workspace')
    await mkdir(folder)
    const workspace = await api('/workspaces', { body: { path: folder } })
    const session = await api('/sessions', { body: { workspace_id: workspace.workspace_id } })
    await stopProcess(app)
    const events = []
    for (let turn = 0; turn < 100; turn++) {
      const add = (type, fields = {}) => events.push({ seq: events.length, occurred_at_ms: 1_700_000_000_000 + events.length,
        run_id: `turn-${turn}`, type, ...fields })
      add('turn_started')
      add('user_message', { content: `Read task ${turn}` })
      for (let part = 0; part < 10; part++) add('assistant_reasoning_delta', { step: 1, delta: `Thought ${part}` })
      add('assistant_message_delta', { step: 1, delta: `Result ${turn}` })
      add('turn_failed', { message: `Fixture stopped ${turn}` })
    }
    await mkdir(path.join(data, 'data', 'sessions'), { recursive: true })
    await writeFile(path.join(data, 'data', 'sessions', `${Buffer.from(session.identity.session_id).toString('hex')}.jsonl`), events.map(event => JSON.stringify(event)).join('\n') + '\n')
    app = startProcess(binary, args)
    await waitForHttp(origin, app)
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
    page = await browser.newPage({ viewport: { width: 390, height: 844 }, isMobile: true, hasTouch: true, serviceWorkers: 'block' })
    page.on('pageerror', error => errors.push(error.message))
    page.on('console', message => { if (message.type() === 'error') errors.push(message.text()) })
    page.on('response', response => { if (response.status() >= 400) errors.push(`${response.status()} ${response.url()}`) })
    await page.goto(origin)
    await page.getByText('Result 99', { exact: true }).waitFor()
    assert.equal(await page.getByRole('button', { name: '加载更早', exact: true }).count(), 0)
    await page.getByRole('tab', { name: '轨迹', exact: true }).tap()
    await page.locator('[data-trajectory-ledger][data-virtualized="true"]').waitFor()
    const scroller = page.locator('[data-conversation-scroll]')
    await scroller.evaluate(element => { element.scrollTop = 2500 })
    await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))))
    await page.evaluate(() => {
      const element = document.querySelector('[data-conversation-scroll]')
      window.__trajectoryScroll = []
      const sample = () => {
        window.__trajectoryScroll.push({ top: element.scrollTop, height: element.scrollHeight })
        if (window.__trajectoryScroll.length < 100) requestAnimationFrame(sample)
      }
      requestAnimationFrame(sample)
    })
    const cdp = await page.context().newCDPSession(page)
    await cdp.send('Input.dispatchTouchEvent', { type: 'touchStart', touchPoints: [{ x: 200, y: 600 }] })
    for (let step = 1; step <= 16; step++) {
      await cdp.send('Input.dispatchTouchEvent', { type: 'touchMove', touchPoints: [{ x: 200, y: 600 - step * 23 }] })
      await new Promise(resolve => setTimeout(resolve, 20))
    }
    await cdp.send('Input.dispatchTouchEvent', { type: 'touchEnd', touchPoints: [] })
    await page.waitForFunction(() => window.__trajectoryScroll.length === 100)
    const samples = await page.evaluate(() => window.__trajectoryScroll)
    assert.ok(samples.at(-1).top - samples[0].top > 200, 'touch swipes scroll the ledger')
    const backwards = samples.slice(1).map((sample, index) => samples[index].top - sample.top)
    assert.ok(Math.max(...backwards) < 60, `scroll must not jump backwards: ${Math.max(...backwards)}`)
    const settled = samples.slice(-15).map(sample => sample.top)
    assert.ok(Math.max(...settled) - Math.min(...settled) < 2, 'scroll stays put after the gesture settles')
    await writeFile(path.join(artifacts, 'trajectory-scroll.json'), JSON.stringify(samples, null, 2))
    await page.screenshot({ path: path.join(artifacts, 'trajectory-scroll-mobile.png') })
    assert.deepEqual(errors, [])
  } finally {
    await browser?.close()
    if (app) await stopProcess(app)
    await rm(directory, { recursive: true, force: true })
  }
})
