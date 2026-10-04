import assert from 'node:assert/strict'
import { selectSpace } from './platform-e2e-fixture.mjs'
import { mkdir, mkdtemp, readFile, readdir, stat, writeFile } from 'node:fs/promises'
import path from 'node:path'
import { tmpdir } from 'node:os'
import test from 'node:test'
import { chromium } from 'playwright'
import { startScenario, until } from './execution-capacity-fixture.mjs'
import { recoveryModel } from './workspace-recovery-model.mjs'

async function findFile(root, name) {
  for (const entry of await readdir(root, { withFileTypes: true })) {
    const full = path.join(root, entry.name)
    if (entry.name === name && entry.isFile()) return full
    if (entry.isDirectory() && entry.name !== '.ternilo-occupancy') {
      const found = await findFile(full, name)
      if (found) return found
    }
  }
  return null
}

async function openSession(browser, fixture, sessionId, errors) {
  const page = await browser.newPage({ locale: 'zh-CN', viewport: { width: 1440, height: 960 }, serviceWorkers: 'block' })
  page.on('pageerror', error => errors.push(error.message))
  page.on('console', message => { if (message.type() === 'error') errors.push(message.text()) })
  page.on('response', response => { if (response.status() >= 400) errors.push(`${response.status()} ${new URL(response.url()).pathname}`) })
  await page.goto(fixture.application.origin)
  await page.getByLabel('用户名', { exact: true }).fill(fixture.memberCredentials.username)
  await page.getByLabel('密码', { exact: true }).fill(fixture.memberCredentials.password)
  await page.getByRole('button', { name: '登录', exact: true }).click()
  await selectSpace(page, fixture.tenantId)
  for (const toggle of await page.locator('[data-sidebar-workspace-button][aria-expanded="false"]').all()) await toggle.click()
  await page.locator(`[data-sidebar-session-row][data-session-id="${sessionId}"] [data-sidebar-session-button]`).click()
  await page.locator('[data-composer-input]').waitFor()
  return page
}

async function cancelWaitingTask(browser, fixture, workspaceId, errors) {
  await fixture.session('recover-cancelled', workspaceId)
  await fixture.submit('recover-cancelled', 'RECOVERY::new cancelled before execution')
  const run = fixture.runs().find(value => value.session_id === 'recover-cancelled')
  const page = await openSession(browser, fixture, 'recover-cancelled', errors)
  try {
    await page.setViewportSize({ width: 320, height: 844 })
    await page.locator('[data-queued-execution-phase="waiting_for_workspace"]').waitFor()
    const stop = page.getByRole('button', { name: '停止运行', exact: true })
    assert.equal(await stop.locator('.lucide-square').count(), 1)
    assert.equal(await stop.locator('.lucide-loader-circle').count(), 0)
    await stop.click()
    await until(() => fixture.runs().find(value => value.run_id === run.run_id)?.state,
      state => state === 'cancelled', 'waiting task cancellation remains available')
    await page.locator('[data-queued-execution-phase]').waitFor({ state: 'hidden' })
    assert.equal(fixture.query('SELECT COUNT(*) AS count FROM cloud_workspace_occupancy WHERE run_id=?', run.run_id)[0].count, 0)
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true)
  } finally { await page.close() }
}

test('a paused Worker cannot lose its directory until physical exit, then another Worker recovers it', { timeout: 150_000 }, async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-workspace-recovery-'))
  const artifacts = process.env.TERNILO_E2E_ARTIFACT_DIR ?? directory
  await mkdir(artifacts, { recursive: true })
  const upstream = await recoveryModel()
  const errors = []
  let fixture, browser, page, oldWrites, newer, oldRun, newRun
  try {
    fixture = await startScenario(directory, 2, 4, upstream)
    const original = await fixture.session('recover-old')
    await fixture.submit('recover-old', 'RECOVERY::old')
    oldWrites = await until(() => findFile(path.join(directory, 'workspaces'), 'old-writes'), Boolean, 'the old isolated shell is writing', fixture.diagnostic)
    oldRun = fixture.runs().find(run => run.session_id === 'recover-old')
    const priorSize = (await stat(oldWrites)).size
    assert.ok(fixture.worker.child.kill('SIGSTOP'))
    const peer = await fixture.startPeerWorker('recovery-peer')
    assert.ok(original.workspace_id, 'the second session must reuse the exact workspace')
    newer = await fixture.session('recover-new', original.workspace_id)
    assert.equal(newer.workspace_id, original.workspace_id)
    await fixture.submit('recover-new', 'RECOVERY::new')
    newRun = fixture.runs().find(run => run.session_id === 'recover-new')
    browser = await chromium.launch({ executablePath: process.env.TERNILO_BROWSER_EXECUTABLE, headless: true })
    page = await openSession(browser, fixture, 'recover-new', errors)
    await page.locator('[data-composer-input]').fill('A private draft survives workspace recovery.')
    await until(() => fixture.runs().find(run => run.run_id === oldRun.run_id)?.state,
      state => state === 'indeterminate', 'Server expires the old lease without assuming physical exit', fixture.diagnostic)
    await until(() => fixture.proxy.calls.filter(call => call.worker_id === 'recovery-peer' && call.operation === 'workspace_recovery_candidates' && call.status === 200).length,
      count => count >= 2, 'the replacement actually checks recovery')
    await new Promise(resolve => setTimeout(resolve, 2300))
    assert.ok((await stat(oldWrites)).size > priorSize, 'suspending the trusted supervisor does not stop its isolated writer')
    assert.equal(fixture.runs().find(run => run.run_id === newRun.run_id).state, 'queued')
    assert.equal(upstream.calls.some(call => call.marker === 'new'), false, 'no model or process starts while the old writer is alive')
    assert.equal(fixture.query('SELECT state FROM cloud_workspace_occupancy WHERE run_id=?', oldRun.run_id)[0].state, 'cleanup')
    const waiting = page.locator('[data-queued-execution-phase="waiting_for_workspace"]')
    await waiting.waitFor()
    assert.equal(await waiting.textContent(), '等待目录空闲')
    assert.equal(await page.locator('[data-chat-flow] [data-execution-phase]').count(), 0, 'the waiting task is not a fabricated model turn')
    for (const width of [390, 320]) {
      await page.setViewportSize({ width, height: 844 })
      await page.mouse.move(width - 1, 100)
      assert.equal(await waiting.isVisible(), true)
      assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true)
      await page.screenshot({ path: path.join(artifacts, `workspace-waiting-${width}.png`) })
    }
    await page.setViewportSize({ width: 1440, height: 960 })
    await page.mouse.move(1439, 100)
    await page.screenshot({ path: path.join(artifacts, 'workspace-held-desktop.png') })
    await cancelWaitingTask(browser, fixture, original.workspace_id, errors)
    assert.equal(upstream.calls.some(call => call.marker === 'new'), false)
    assert.ok(fixture.worker.child.kill('SIGKILL'))
    await until(() => fixture.runs().find(run => run.run_id === newRun.run_id)?.state,
      state => state === 'succeeded', 'replacement recovers the directory and executes the queued task', fixture.diagnostic)
    assert.equal(await readFile(path.join(path.dirname(oldWrites), 'new-writes'), 'utf8'), 'new')
    const stoppedSize = (await stat(oldWrites)).size
    await new Promise(resolve => setTimeout(resolve, 300))
    assert.equal((await stat(oldWrites)).size, stoppedSize, 'old writes remain stopped after handoff')
    assert.equal(upstream.calls.filter(call => !call.title && call.marker === 'old').length, 1, 'the unknown old task is never replayed')
    const occupancy = fixture.query('SELECT run_id,state,occupation_epoch FROM cloud_workspace_occupancy WHERE run_id IN (?,?)', oldRun.run_id, newRun.run_id)
    assert.equal(occupancy.find(row => row.run_id === oldRun.run_id).state, 'released')
    assert.ok(occupancy.find(row => row.run_id === newRun.run_id).occupation_epoch > occupancy.find(row => row.run_id === oldRun.run_id).occupation_epoch)
    const audits = fixture.query("SELECT actor_kind,actor_user_id,metadata FROM control_audit_log WHERE action='workspace.recovered' AND resource_id=?", oldRun.run_id)
    assert.equal(audits.length, 1)
    assert.equal(audits[0].actor_kind, 'worker')
    assert.equal(audits[0].actor_user_id, null)
    assert.equal(JSON.parse(audits[0].metadata).reporter_worker_id, 'recovery-peer')
    await page.getByText('Completed new writer.', { exact: true }).waitFor()
    assert.equal(await page.locator('[data-queued-execution-phase]').count(), 0)
    assert.equal(await page.locator('[data-composer-input]').inputValue(), 'A private draft survives workspace recovery.')
    for (const width of [390, 320]) {
      await page.setViewportSize({ width, height: 844 })
      await page.mouse.move(width - 1, 100)
      assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true)
      await page.screenshot({ path: path.join(artifacts, `workspace-recovered-${width}.png`) })
    }
    assert.deepEqual(errors, [])
    assert.deepEqual(upstream.failures, [])
    assert.deepEqual(fixture.proxy.failures, [])
    assert.deepEqual(fixture.proxy.calls.filter(call => call.status >= 400), [])
    assert.equal(fixture.runs().find(run => run.run_id === newRun.run_id).actor_user_id, fixture.member.user.user_id)
    assert.equal(peer.child.exitCode, null, peer.diagnostics())
    await writeFile(path.join(artifacts, 'summary.json'), JSON.stringify({ errors, occupancy, audits, calls: upstream.calls,
      runs: fixture.runs(), rpc: fixture.proxy.calls, assetHashes: fixture.assetHashes }, null, 2))
  } finally {
    if (fixture) {
      await writeFile(path.join(artifacts, 'worker.log'), fixture.worker.diagnostics())
      await writeFile(path.join(artifacts, 'rpc.json'), JSON.stringify(fixture.proxy.calls, null, 2))
      await writeFile(path.join(artifacts, 'runs.json'), JSON.stringify(fixture.runs(), null, 2))
      fixture.worker.child.kill('SIGCONT')
    }
    await browser?.close()
    await fixture?.close()
    await upstream.close()
  }
})
