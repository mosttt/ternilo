import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { chromium } from 'playwright'
import { choose, profile, upstream } from './account-node-provider-fixture.mjs'
import { localApi, until } from './model-device-fixture.mjs'
import { freePort, initializeServer, repository, selectSpace, serverRequest, startProcess, stopProcess, waitForHttp } from './platform-e2e-fixture.mjs'

test('scheduled account models retain their creator across later input, revocation, stop and restart', { timeout: 180_000 }, async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-scheduled-model-'))
  const artifacts = process.env.TERNILO_E2E_ARTIFACT_DIR ?? directory
  await mkdir(artifacts, { recursive: true })
  const processes = [], errors = []
  const environment = { XDG_STATE_HOME: path.join(directory, 'state') }
  let browser, page, remote, node
  try {
    remote = await upstream('scheduled-source', 'scheduled-private-key')
    const server = await initializeServer({ directory: path.join(directory, 'server'), origin: `http://127.0.0.1:${await freePort()}`, mode: 'multi_user', environment })
    processes.push(server)
    let tenantId = server.owner.session.personal_tenant_id
    const owner = (resource, options = {}) => serverRequest(server.origin, resource, { token: server.owner.session.access_token, tenantId, ...options })
    const registration = await owner('/admin/registration')
    await owner('/admin/registration', { method: 'PATCH', body: { mode: 'open', require_approval: false, oidc_only: false, revision: registration.revision } })
    const credentials = { username: 'schedule-member', email: 'schedule@example.test', password: 'schedule-member-password' }
    const { session: account } = await serverRequest(server.origin, '/auth/register', { body: credentials })
    const { tenant } = await owner('/tenants', { body: { slug: 'schedules', display_name: 'Scheduled team' } })
    tenantId = tenant.tenant_id
    await owner(`/tenants/${tenantId}/members/${account.user.user_id}`, { method: 'PUT', body: { role: 'member' } })
    const member = (resource, options = {}) => serverRequest(server.origin, resource, { token: account.access_token, tenantId, ...options })
    const personal = (resource, options = {}) => member(resource, { tenantId: account.personal_tenant_id, ...options })
    await personal('/credentials', { body: { name: 'SAME_PROVIDER_KEY', value: 'scheduled-private-key' } })
    await personal('/providers', { body: profile(remote.baseUrl) })
    const enrolled = await owner(`/tenants/${tenantId}/my-computer-enrollments`, { body: { name: 'schedule-node', project_id: null, ttl_seconds: 600 } })
    const enrolledComputerId = enrolled.enrollment.executor_id
    const credential = await owner('/enrollments/consume', { body: { token: enrolled.enrollment.token } })
    const origin = `http://127.0.0.1:${await freePort()}`
    const binary = process.env.TERNILO_E2E_NODE_BINARY ?? path.join(repository, 'target/debug/ternilo')
    const args = ['serve', '--listen', new URL(origin).host, '--data-dir', path.join(directory, 'node'), '--node-id', enrolledComputerId, '--gateway-url', `${server.origin.replace('http:', 'ws:')}/api/v1/executors/connect`, '--allow-insecure-gateway']
    const env = { ...environment, TERNILO_LOCAL_TOKEN: credential.credential.token }
    const start = async () => { node = startProcess(binary, args, env); processes.push(node); await waitForHttp(origin, node) }
    await start()
    const local = await localApi(origin)
    const folder = path.join(directory, 'workspace'); await mkdir(folder)
    const workspace = await local('/workspaces', { body: { path: folder } })
    await local('/sessions', { body: { workspace_id: workspace.workspace_id } })
    const state = await until(() => owner('/state'), value => value.sessions.length === 1, 'scheduled session maps to Server')
    const sessionId = state.sessions[0].identity.session_id
    const sharing = configure => owner(`/sessions/${sessionId}/sharing/user/${account.user.user_id}`, { method: 'PUT', body: { view: true, submit: true, stop: true, configure } })
    await sharing(true)
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
    const open = async () => {
      page = await browser.newPage({ locale: 'zh-CN', viewport: { width: 1366, height: 900 }, serviceWorkers: 'block' })
      page.on('pageerror', error => errors.push(error.message))
      page.on('console', message => { if (message.type() === 'error') errors.push(message.text()) })
      page.on('response', response => { if (response.status() >= 400) errors.push(`${response.status()} ${new URL(response.url()).pathname}`) })
      await page.goto(server.origin)
      await page.getByLabel('用户名', { exact: true }).fill(credentials.username)
      await page.getByLabel('密码', { exact: true }).fill(credentials.password)
      await page.getByRole('button', { name: '登录', exact: true }).click()
      await selectSpace(page, tenantId)
      await page.locator(`[data-sidebar-session-row][data-session-id="${sessionId}"]`).click()
    }
    await open()
    await choose(page, 'account')
    const history = () => owner(`/sessions/${sessionId}/events`)
    const queue = (request, input) => request(`/sessions/${sessionId}/queue`, { body: { content: { kind: 'prompt', input } } })
    const terminal = run => until(history, events => events.some(event => event.run_id === run && ['turn_finished', 'turn_failed', 'turn_cancelled'].includes(event.type)), 'scheduled turn terminates')
    const schedule = async (marker, seconds = 5) => {
      const accepted = await queue(member, `/schedule-after ${seconds} ${marker}`)
      await page.getByRole('button', { name: '允许一次', exact: true }).last().click()
      const events = await terminal(accepted.run_id)
      const created = events.find(event => event.run_id === accepted.run_id && event.type === 'schedule_changed' && event.change.operation === 'create')
      assert.ok(created, JSON.stringify(events.filter(event => event.run_id === accepted.run_id)))
      return created
    }
    const dispatched = created => until(history, events => events.some(event => event.type === 'schedule_changed' && event.change.operation === 'dispatch' && event.change.id === created.change.schedule.id), 'scheduled dispatch is synchronized')
    const occurrence = (events, created) => events.find(event => event.type === 'schedule_changed' && event.change.operation === 'dispatch' && event.change.id === created.change.schedule.id).change.run_id
    const ledgerIds = async () => new Set((await member('/model-access/requests?limit=100')).requests.map(request => request.request_id))
    const assertLedger = async (previous, interrupted = false, grantId) => {
      const fresh = value => value.requests.filter(request => !previous.has(request.request_id))
      const entries = fresh(await until(() => member('/model-access/requests?limit=100'), value => fresh(value).length > 0 && fresh(value).every(request => request.state !== 'pending'), 'scheduled ledger settles'))
      for (const entry of entries) {
        assert.equal(entry.actor_user_id, account.user.user_id)
        assert.equal(entry.model_beneficiary_user_id, account.user.user_id)
        assert.equal(entry.resource_owner_user_id, server.owner.session.user.user_id)
        assert.equal(entry.source, grantId ? 'platform_grant' : 'user_provider')
        if (grantId) assert.equal(entry.grant_id, grantId)
        if (interrupted) assert.equal(entry.accounted_tokens, null)
        else assert.ok(entry.accounted_tokens > 0)
      }
    }
    for (const mode of ['complete', 'stop', 'revoke']) {
      const previous = await ledgerIds()
      const marker = `scheduled-${mode}-proof`
      const count = remote.calls.length
      const hold = remote.holdNext(body => body.stream && JSON.stringify(body.messages.findLast(message => message.role === 'user')).includes(marker))
      try {
        const created = await schedule(marker)
        const later = await queue(owner, '/code "later input from Node owner"')
        await terminal(later.run_id)
        await Promise.race([hold.began, new Promise((_, reject) => setTimeout(() => reject(new Error(`${marker} never reached the gateway`)), 15_000))])
        const run = occurrence(await dispatched(created), created)
        if (mode === 'stop') await member(`/sessions/${sessionId}/turns/${run}`, { method: 'DELETE' })
        else if (mode === 'revoke') await sharing(false)
        else hold.release()
        const events = await terminal(run)
        const expected = mode === 'complete' ? 'turn_finished' : mode === 'stop' ? 'turn_cancelled' : 'turn_failed'
        assert.ok(events.some(event => event.run_id === run && event.type === expected), JSON.stringify(events.filter(event => event.run_id === run)))
        const input = events.find(event => event.run_id === run && event.type === 'user_message')
        assert.equal(input.source.created_seq, created.seq)
        assert.equal(input.provenance.author.source, 'schedule')
        await assertLedger(previous, mode !== 'complete')
        if (mode !== 'complete') assert.equal(remote.calls.slice(count).filter(call => call.stream).length, 1, 'interruption never retries or silently switches models')
        if (mode === 'revoke') await sharing(true)
      } finally { hold.release() }
    }
    await owner('/admin/models/providers', { body: { profile: { ...profile(remote.baseUrl), id: 'platform-source', api_key_ref: null }, enabled: true, api_key: 'scheduled-private-key' } })
    await owner('/admin/models/publications', { body: { model_id: 'scheduled-platform', display_name: '定时平台模型', provider_id: 'platform-source', upstream_model: 'same-model', enabled: true } })
    const grant = await owner('/admin/models/grants', { body: { name: '定时任务预算', subject: { kind: 'user', id: account.user.user_id }, model_ids: ['scheduled-platform'], monthly_tokens: 100000, max_concurrent_requests: 2, allow_resource_sharing: true } })
    const platformLedger = await ledgerIds()
    const platformSchedule = await schedule('explicit-model-change-before-trigger', 8)
    await page.locator('[data-input-bar] [data-model-picker]').click()
    await page.getByRole('menuitem', { name: /^模型/ }).click()
    await page.locator('[data-model-source="platform"]').getByRole('menuitem', { name: /定时平台模型/ }).click()
    await until(() => owner(`/model-options?session_id=${sessionId}`), value => value.current?.selection.grant_id === grant.grant_id, 'explicit platform choice saved before trigger')
    const platformRun = occurrence(await dispatched(platformSchedule), platformSchedule)
    assert.ok((await terminal(platformRun)).some(event => event.run_id === platformRun && event.type === 'turn_finished'))
    await assertLedger(platformLedger, false, grant.grant_id)
    await choose(page, 'account')
    const deleted = await schedule('deleted-must-not-execute', 15)
    const deletion = await queue(member, `/schedule-delete ${deleted.change.schedule.id}`)
    await page.getByRole('button', { name: '允许一次', exact: true }).last().click()
    await terminal(deletion.run_id)
    const previous = await ledgerIds()
    const pending = await schedule('restart-pending-proof', 4)
    await page.close()
    await stopProcess(node)
    await new Promise(resolve => setTimeout(resolve, 4500))
    await start()
    const resumed = occurrence(await dispatched(pending), pending)
    const recovered = await terminal(resumed)
    assert.ok(recovered.some(event => event.run_id === resumed && event.type === 'turn_finished'), JSON.stringify(recovered.filter(event => event.run_id === resumed)))
    await new Promise(resolve => setTimeout(resolve, Math.max(0, deleted.change.schedule.scheduled_at_ms - Date.now() + 100)))
    assert.equal((await history()).some(event => event.type === 'schedule_changed' && event.change.operation === 'dispatch' && event.change.id === deleted.change.schedule.id), false)
    await assertLedger(previous)
    const beforeRestart = remote.calls.length
    await stopProcess(node)
    await start()
    await open()
    await page.getByRole('textbox', { name: '输入任务', exact: true }).waitFor()
    await page.locator('[data-role="assistant"]').filter({ hasText: 'scheduled-source finished this task.' }).last().waitFor()
    await page.mouse.move(1300, 800)
    assert.equal(remote.calls.length, beforeRestart, 'accepted one-shot occurrences are not replayed after restart')
    assert.equal((await history()).filter(event => event.type === 'schedule_changed' && event.change.operation === 'dispatch' && event.change.id === pending.change.schedule.id).length, 1)
    await page.screenshot({ path: path.join(artifacts, 'scheduled-model-desktop.png') })
    await page.setViewportSize({ width: 390, height: 844 })
    await page.locator('[data-app-frame][data-mobile="true"]').waitFor()
    await page.mouse.move(380, 800)
    await page.evaluate(async () => {
      await new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve)))
      await Promise.all(document.getAnimations().filter(animation => animation.effect?.getTiming().iterations !== Infinity).map(animation => animation.finished.catch(() => {})))
    })
    const closeSidebar = page.locator('[data-app-sidebar-column]:not([inert]) [data-mobile-sidebar-close]')
    if (await closeSidebar.count()) await closeSidebar.click()
    await page.locator('[data-role="assistant"]').filter({ hasText: 'scheduled-source finished this task.' }).last().waitFor()
    assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth))
    await page.screenshot({ path: path.join(artifacts, 'scheduled-model-mobile.png') })
    for (const target of [origin, server.origin]) for (const asset of ['app.js', 'app.css']) {
      const served = Buffer.from(await (await fetch(`${target}/assets/${asset}`)).arrayBuffer())
      const built = await readFile(path.join(repository, 'web/dist/assets', asset))
      assert.equal(createHash('sha256').update(served).digest('hex'), createHash('sha256').update(built).digest('hex'))
    }
    assert.deepEqual(errors, [])
  } catch (error) {
    if (page && !page.isClosed()) await page.screenshot({ path: path.join(artifacts, 'scheduled-model-failure.png') }).catch(() => {})
    error.message += `\nBrowser errors: ${JSON.stringify(errors)}`
    throw error
  } finally {
    await browser?.close()
    for (const process of processes.reverse()) await stopProcess(process)
    await writeFile(path.join(artifacts, 'scheduled-model-processes.log'), processes.map(process => process.diagnostics()).join('\n'))
    await remote?.close()
    if (artifacts !== directory) await rm(directory, { recursive: true, force: true })
  }
})
