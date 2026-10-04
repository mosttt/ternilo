import assert from 'node:assert/strict'
import { execFile } from 'node:child_process'
import { appendFile, mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import { promisify } from 'node:util'
import test from 'node:test'
import { chromium } from 'playwright'
import { freePort, repository, startProcess, stopProcess, waitForHttp } from './platform-e2e-fixture.mjs'
import { localApi, until } from './model-device-fixture.mjs'

const execute = promisify(execFile)

test('offline log repair keeps a backup, restores browser history and allows new canonical events', { timeout: 120_000 }, async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-log-repair-'))
  const artifacts = process.env.TERNILO_E2E_ARTIFACT_DIR ?? directory
  await mkdir(artifacts, { recursive: true })
  const binary = process.env.TERNILO_E2E_NODE_BINARY ?? path.join(repository, 'target/debug/ternilo')
  const data = path.join(directory, 'data'), folder = path.join(directory, 'workspace')
  const origin = `http://127.0.0.1:${await freePort()}`
  const args = ['serve', '--listen', new URL(origin).host, '--data-dir', data]
  let app, browser, page
  const errors = []
  try {
    await mkdir(folder)
    app = startProcess(binary, args)
    await waitForHttp(origin, app)
    let api = await localApi(origin)
    const workspace = await api('/workspaces', { body: { path: folder } })
    const session = await api('/sessions', { body: { workspace_id: workspace.workspace_id } })
    const id = session.identity.session_id
    const log = path.join(data, 'data', 'sessions', `${Buffer.from(id).toString('hex')}.jsonl`)
    await stopProcess(app)
    const events = [
      { type: 'turn_started' },
      { type: 'user_message', content: 'Keep the recovered transcript', references: [], attachments: [] },
      { type: 'step_started', step: 1 },
      { type: 'assistant_message_delta', step: 1, delta: 'Recovered history is intact.' },
      { type: 'turn_failed', message: 'Fixture ends after output' },
    ].map((event, seq) => ({ seq, occurred_at_ms: 1_700_000_000_000 + seq, run_id: 'repair-history', ...event }))
    const valid = events.map(event => JSON.stringify(event)).join('\n') + '\n'
    await mkdir(path.dirname(log), { recursive: true })
    await writeFile(log, valid)
    app = startProcess(binary, args)
    await waitForHttp(origin, app)
    const repairArgs = ['repair-session-log', '--data-dir', data, '--session-id', id]
    await assert.rejects(execute(binary, [...repairArgs, '--apply']), error => error.code === 1 && /already open by another process/.test(error.stderr))
    await stopProcess(app)
    const incomplete = '{"seq":5,"occurred_at_ms":'
    await appendFile(log, incomplete)
    const before = await readFile(log)
    const dryRun = JSON.parse((await execute(binary, repairArgs)).stdout)
    assert.equal(dryRun.action, 'discard_incomplete_tail')
    assert.equal(dryRun.applied, false)
    assert.equal(dryRun.valid_records, events.length)
    assert.deepEqual(await readFile(log), before)
    const repaired = JSON.parse((await execute(binary, [...repairArgs, '--apply'])).stdout)
    assert.equal(repaired.applied, true)
    assert.deepEqual(await readFile(repaired.backup_path), before)
    assert.equal(await readFile(log, 'utf8'), valid)
    app = startProcess(binary, args)
    await waitForHttp(origin, app)
    api = await localApi(origin)
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
    page = await browser.newPage({ locale: 'zh-CN', viewport: { width: 1280, height: 900 }, serviceWorkers: 'block' })
    page.on('pageerror', error => errors.push(error.message))
    page.on('console', message => { if (message.type() === 'error') errors.push(message.text()) })
    page.on('response', response => { if (response.status() >= 400) errors.push(`HTTP ${response.status()} ${new URL(response.url()).pathname}`) })
    await page.goto(origin)
    await page.getByText('Recovered history is intact.', { exact: true }).waitFor()
    assert.deepEqual(await api(`/sessions/${id}/events`), events)
    await api(`/sessions/${id}/commands/feedback`, { body: { text: 'Recovery accepted' } })
    await until(() => api(`/sessions/${id}/events`), value => value.length > events.length, 'new events after offline repair')
    await page.reload()
    await page.getByText('Recovered history is intact.', { exact: true }).waitFor()
    const after = await api(`/sessions/${id}/events`)
    assert.deepEqual(after.slice(0, events.length), events)
    assert.deepEqual(after.map(event => event.seq), Array.from({ length: after.length }, (_, index) => index))
    await page.screenshot({ path: path.join(artifacts, 'session-log-repaired.png') })
    assert.deepEqual(errors, [])
    await browser.close(); browser = undefined
    await stopProcess(app); app = undefined
    const clean = JSON.parse((await execute(binary, [...repairArgs, '--apply'])).stdout)
    assert.equal(clean.action, 'none')
    assert.equal(clean.applied, false)
    assert.equal(clean.backup_path, null)
  } catch (error) {
    await page?.screenshot({ path: path.join(artifacts, 'session-log-repair-failure.png') }).catch(() => {})
    error.message += `\n${app?.diagnostics() ?? ''}`
    throw error
  } finally {
    await browser?.close()
    if (app) await stopProcess(app)
    await rm(directory, { recursive: true, force: true })
  }
})
