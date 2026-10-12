import assert from 'node:assert/strict'
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { chromium } from 'playwright'
import { freePort, repository, startProcess, stopProcess, waitForHttp } from './platform-e2e-fixture.mjs'
import { localApi, until } from './model-device-fixture.mjs'

function longHistory(manyTurns = false) {
  const events = []
  const add = (type, fields = {}) => events.push({ seq: events.length, occurred_at_ms: 1_700_000_000_000 + events.length, run_id: 'history-position', type, ...fields })
  if (manyTurns) for (let turn = 0; turn < 450; turn++) {
    const earlier = (type, fields = {}) => add(type, { run_id: `earlier-${turn}`, ...fields })
    earlier('turn_started')
    earlier('user_message', { content: `Earlier task ${turn}` })
    for (let index = 0; index < 10; index++) earlier('assistant_reasoning_delta', { step: 1, delta: 'Earlier reasoning. ' })
    earlier('assistant_message_delta', { step: 1, delta: `Earlier answer ${turn}` })
    earlier('turn_failed', { message: 'Earlier fixture ended' })
  }
  add('turn_started')
  add('user_message', { content: 'Keep the reading position while loading history.' })
  for (let step = 1; step <= 6; step++) {
    add('step_started', { step })
    for (let index = 0; index < (manyTurns ? 1 : 1100); index++) add('assistant_reasoning_delta', { step, delta: `Reason ${step}/${index}. ` })
    const content = Array.from({ length: step === 6 ? 35 : 8 }, (_, index) => `Paragraph ${step}/${index + 1}: 历史加载时保留阅读位置。 Preserve the reading position. 읽던 위치를 유지합니다。`).join('\n\n')
    add('assistant_message', { step, response: { provider: 'fixture', model: 'fixture', content, tool_calls: [], finish_reason: 'stop' } })
  }
  add('turn_failed', { message: 'Fixture ended after output' })
  return events
}

const geometry = page => page.locator('[data-conversation-scroll]').evaluate(element => ({ top: element.scrollTop, distance: element.scrollHeight - element.clientHeight - element.scrollTop }))
async function atTail(page, label) {
  await page.waitForTimeout(120)
  const position = await geometry(page)
  assert.ok(Math.abs(position.distance) < 2, `${label}: ${JSON.stringify(position)}`)
}
async function anchor(page) {
  return page.locator('[data-conversation-scroll]').evaluate(element => {
    const top = element.getBoundingClientRect().top
    const paragraph = [...element.querySelectorAll('.markdown-body p')].find(row => row.getBoundingClientRect().bottom > top && row.getBoundingClientRect().top < top + 150)
    return { text: paragraph?.textContent, top: paragraph?.getBoundingClientRect().top - top }
  })
}

test('background history preserves the bottom and the current paragraph on desktop and mobile', { timeout: 120_000 }, async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-history-position-'))
  const origin = `http://127.0.0.1:${await freePort()}`
  const data = path.join(directory, 'data')
  const binary = process.env.TERNILO_E2E_NODE_BINARY ?? path.join(repository, 'target/debug/ternilo')
  const args = ['serve', '--listen', new URL(origin).host, '--data-dir', data]
  let app, browser, release = [], page
  const errors = []
  try {
    app = startProcess(binary, args); await waitForHttp(origin, app)
    const api = await localApi(origin)
    const folder = path.join(directory, 'workspace'); await mkdir(folder)
    const workspace = await api('/workspaces', { body: { path: folder } })
    const session = await api('/sessions', { body: { workspace_id: workspace.workspace_id } })
    const id = session.identity.session_id
    await stopProcess(app)
    await mkdir(path.join(data, 'data', 'sessions'), { recursive: true })
    await writeFile(path.join(data, 'data', 'sessions', `${Buffer.from(id).toString('hex')}.jsonl`), longHistory().map(event => JSON.stringify(event)).join('\n') + '\n')
    app = startProcess(binary, args); await waitForHttp(origin, app)
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
    for (const manyTurns of [false, true]) {
      if (manyTurns) {
        await stopProcess(app)
        await writeFile(path.join(data, 'data', 'sessions', `${Buffer.from(id).toString('hex')}.jsonl`), longHistory(true).map(event => JSON.stringify(event)).join('\n') + '\n')
        app = startProcess(binary, args); await waitForHttp(origin, app)
      }
      for (const width of [1280, 390, 320]) {
        for (const reading of [false, true]) {
          release = []
          const context = await browser.newContext({ locale: 'zh-CN', viewport: { width, height: width === 320 ? 560 : 820 }, hasTouch: width < 600, serviceWorkers: 'block' })
          page = await context.newPage()
          page.on('pageerror', error => errors.push(error.message))
          page.on('console', message => { if (message.type() === 'error') errors.push(message.text()) })
          page.on('response', response => { if (response.status() >= 400) errors.push(`${response.status()} ${response.url()}`) })
          await page.route(`**/sessions/${id}/history?*`, async route => {
            if (new URL(route.request().url()).searchParams.has('before_seq')) await new Promise(resolve => release.push(resolve))
            await route.continue().catch(() => {})
          })
          await page.goto(origin)
          await page.getByText('Paragraph 6/35:', { exact: false }).waitFor()
          await until(async () => release.length, count => count === 1, 'first delayed history page')
          const scroll = page.locator('[data-conversation-scroll]')
          await scroll.hover(); await page.mouse.wheel(0, 100_000)
          await atTail(page, `${width}px initially at bottom`)
          let saved
          if (reading) {
            await page.mouse.wheel(0, -500)
            await page.waitForTimeout(150)
            saved = await anchor(page)
            assert.ok(saved.text?.startsWith('Paragraph 6/'), JSON.stringify(saved))
          }
          for (let index = 0; index < 6; index++) {
            await until(async () => release.length, count => count > index, `delayed history page ${index}`)
            release[index]()
            if (index < 5) await until(async () => release.length, count => count > index + 1, `next history page ${index}`)
            else await page.locator('[data-history-state="backfilling"]').waitFor({ state: 'detached' })
            if (reading) {
              await page.waitForTimeout(120)
              const actual = await anchor(page)
              assert.ok(actual.text === saved.text && Math.abs(actual.top - saved.top) < 2, `${width}px reader moved on page ${index}: ${JSON.stringify({ saved, actual })}`)
            } else await atTail(page, `${width}px bottom moved on page ${index}`)
          }
          assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true)
          if (process.env.TERNILO_E2E_ARTIFACT_DIR) {
            await mkdir(process.env.TERNILO_E2E_ARTIFACT_DIR, { recursive: true })
            await page.screenshot({ path: path.join(process.env.TERNILO_E2E_ARTIFACT_DIR, `history-position-${manyTurns ? 'turns' : 'steps'}-${width}-${reading ? 'reading' : 'bottom'}.png`) })
          }
          await context.close()
        }
      }
    }
    assert.deepEqual(errors, [])
  } finally {
    release.forEach(resolve => resolve())
    await browser?.close(); await stopProcess(app); await rm(directory, { recursive: true, force: true })
  }
})
