import { selectChoice } from './browser-select-fixture.mjs'
import assert from 'node:assert/strict'
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises'
import path from 'node:path'
import { tmpdir } from 'node:os'
import test from 'node:test'
import { chromium } from 'playwright'
import { isolatedExecutions, modelFixture, startScenario, until } from './execution-capacity-fixture.mjs'

const terminal = state => ['succeeded', 'failed', 'cancelled', 'indeterminate'].includes(state)
const unavailableChildNotice = '暂时无法从这里打开子会话；任务摘要和结果仍可查看。'

async function pageFor(browser, fixture, sessionId, errors) {
  const context = await browser.newContext({ locale: 'zh-CN', viewport: { width: 1440, height: 960 }, hasTouch: true, serviceWorkers: 'block' })
  const page = await context.newPage()
  page.on('pageerror', error => errors.push(error.message))
  page.on('console', message => { if (message.type() === 'error') errors.push(message.text()) })
  page.on('response', response => {
    if (response.status() < 400) return
    const error = { status: response.status(), method: response.request().method(), path: new URL(response.url()).pathname, body: null }
    errors.push(error)
    void response.text().then(body => { error.body = body }).catch(cause => { error.body = cause.message })
  })
  page.on('requestfailed', request => { if (!request.failure()?.errorText.includes('ERR_ABORTED')) errors.push(request.failure()?.errorText) })
  await page.goto(fixture.application.origin)
  await page.getByLabel('用户名', { exact: true }).fill(fixture.memberCredentials.username)
  await page.getByLabel('密码', { exact: true }).fill(fixture.memberCredentials.password)
  await page.getByRole('button', { name: '登录', exact: true }).click()
  await selectSession(page, fixture.tenantId, sessionId)
  return page
}

async function selectSession(page, tenantId, sessionId) {
  const selector = page.getByRole('combobox', { name: '切换空间', exact: true })
  await selector.waitFor()
  await selectChoice(selector, tenantId)
  for (const toggle of await page.locator('[data-sidebar-workspace-button][aria-expanded="false"]').all()) await toggle.click()
  const row = page.locator(`[data-sidebar-session-row][data-session-id="${sessionId}"]`)
  await row.waitFor()
  await row.locator('[data-sidebar-session-button]').click()
  await page.locator('[data-composer-input]').waitFor()
}

async function status(page, phase) {
  const element = page.locator(`[data-execution-phase="${phase}"]`)
  await element.waitFor()
  assert.equal(await page.locator('[data-execution-phase]').count(), 1, 'the conversation has one current execution status')
  assert.equal(await element.locator('.lucide-hourglass').count(), 1)
  assert.equal((await element.textContent()).includes('深入思考'), false)
  return element
}

async function tail(page) {
  const button = page.getByRole('button', { name: '回到底部', exact: true })
  if (await button.isVisible()) await button.click()
  else await page.locator('[data-conversation-scroll]').evaluate(element => { element.scrollTop = element.scrollHeight })
}

async function mobile(page, artifact, phase, width = 390) {
  await page.setViewportSize({ width, height: 844 })
  await status(page, phase)
  await tail(page)
  const bar = page.locator('[data-input-bar]')
  assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true)
  assert.equal(await bar.evaluate(element => element.scrollWidth <= element.clientWidth), true)
  const stop = bar.getByRole('button', { name: '停止运行', exact: true })
  await stop.waitFor()
  assert.equal(await stop.isEnabled(), true)
  assert.equal(await stop.locator('.lucide-square').count(), 1)
  const bounds = await stop.boundingBox()
  assert.ok(bounds.width >= 39.9 && bounds.height >= 39.9 && bounds.x >= 0 && bounds.x + bounds.width <= width)
  const send = await bar.getByRole('button', { name: '发送', exact: true }).boundingBox()
  assert.ok(send.width >= 39.9 && send.height >= 39.9 && send.x >= 0 && send.x + send.width <= width, 'the independent send button remains inside the mobile viewport')
  for (const card of await page.locator('[data-subagent-event]').all()) {
    const header = await card.evaluate(element => {
      const heading = element.firstElementChild
      const title = heading.querySelector('strong')
      const status = heading.querySelector('[data-status]').parentElement
      return {
        title: title.getBoundingClientRect().toJSON(), status: status.getBoundingClientRect().toJSON(),
        clipped: title.scrollWidth > title.clientWidth || title.scrollHeight > title.clientHeight,
        buttons: [...heading.querySelectorAll('button')].map(button => button.getBoundingClientRect().toJSON()),
      }
    })
    assert.equal(header.clipped, false, 'the complete subagent title can wrap on a narrow screen')
    assert.ok(header.status.top >= header.title.bottom - 1, 'subagent status occupies its own readable line')
    assert.ok(header.buttons.every(button => button.width >= 39.9 && button.height >= 39.9 && button.x >= 0 && button.right <= width))
    assert.equal(await card.getByText(unavailableChildNotice, { exact: true }).count(), 1)
  }
  await page.screenshot({ path: artifact, animations: 'disabled' })
}

async function noOutputFollowing(page) {
  const scroll = page.locator('[data-conversation-scroll]')
  await tail(page)
  await scroll.hover()
  const before = await scroll.evaluate(element => element.scrollTop)
  for (let index = 0; index < 4; index++) { await page.mouse.wheel(0, -4); await page.waitForTimeout(70) }
  const readerTop = await scroll.evaluate(element => element.scrollTop)
  assert.ok(readerTop < before - 8, 'slow upward scrolling escapes the conversation tail while waiting')
  await page.waitForTimeout(200)
  const stableTop = await scroll.evaluate(element => element.scrollTop)
  await scroll.evaluate(element => {
    const descriptor = Object.getOwnPropertyDescriptor(Element.prototype, 'scrollTop')
    window.capacityScrollWrites = []
    window.restoreCapacityScroll = () => Object.defineProperty(Element.prototype, 'scrollTop', descriptor)
    Object.defineProperty(Element.prototype, 'scrollTop', { ...descriptor, set(value) {
      if (this === element) window.capacityScrollWrites.push(value)
      descriptor.set.call(this, value)
    } })
  })
  await page.locator('[data-composer-input]').fill('My private draft while execution waits\nThe current task remains independent.')
  await page.waitForTimeout(1100)
  const observation = await page.evaluate(() => {
    window.restoreCapacityScroll()
    return { writes: window.capacityScrollWrites, top: document.querySelector('[data-conversation-scroll]').scrollTop }
  })
  assert.deepEqual(observation.writes, [], 'without new output, draft/status reflow does not initiate tail following')
  assert.ok(Math.abs(observation.top - stableTop) <= 2, 'waiting keeps the reader position stable')
  return { before, readerTop, ...observation }
}

async function accountEvidence(fixture, { unknownAllowed = false } = {}) {
  const rows = fixture.runs()
  const ownerId = fixture.application.owner.session.user.user_id
  assert.ok(rows.length)
  assert.ok(rows.every(row => terminal(row.state) && row.user_id === ownerId && row.actor_user_id === fixture.member.user.user_id), 'execution owner and real submitter remain distinct through descendants')
  const reservations = fixture.query('SELECT reservation.state FROM control_quota_reservations reservation JOIN cloud_runs run ON run.tenant_id=reservation.tenant_id AND run.quota_reservation_id=reservation.reservation_id WHERE run.tenant_id=?', fixture.tenantId)
  assert.ok(reservations.every(row => row.state !== 'active'), 'all terminal execution token reservations are settled')
  const requests = await until(() => fixture.query("SELECT request_id,run_id,actor_user_id,resource_owner_user_id,model_beneficiary_user_id,source,grant_id,state FROM control_model_requests WHERE origin='workload' AND tenant_id=?", fixture.tenantId), rows => rows.length > 0 && rows.every(row => row.state !== 'pending'), 'accepted model requests settle', fixture.diagnostic)
  assert.ok(requests.length > 0)
  assert.ok(requests.every(row => row.actor_user_id === fixture.member.user.user_id && row.resource_owner_user_id === ownerId
    && row.model_beneficiary_user_id === ownerId && row.grant_id === fixture.grant.grant_id && row.source === 'platform_grant' && row.state !== 'pending'), 'shared model requests use the owner grant without changing actor attribution')
  const usage = await fixture.api(`/tenants/${fixture.tenantId}/model-usage`)
  assert.equal(usage.quota.active_reserved_tokens, 0)
  if (!unknownAllowed) assert.equal(usage.quota.unknown_reserved_tokens, 0)
  assert.equal(usage.totals.total_tokens, usage.ledger.reduce((sum, entry) => sum + (entry.accounted_tokens ?? 0), 0))
  assert.equal(usage.quota.settled_tokens, usage.totals.total_tokens)
  return { runs: rows, requests, reservations, usage }
}

async function allFinished(fixture, count) {
  await until(() => { fixture.check(); return fixture.runs() }, rows => rows.length === count && rows.every(row => terminal(row.state)), `${count} real runs terminal`, fixture.diagnostic)
  await until(() => fixture.snapshot(), snapshot => snapshot.resident === 0 && snapshot.active === 0, 'real Worker cleanup releases all residents', fixture.diagnostic)
}

async function wideScenario(browser, fixture, upstream, artifacts, errors) {
  const roots = []
  for (let index = 0; index < 4; index++) roots.push(await fixture.session(`wide-root-${index}`))
  const page = await pageFor(browser, fixture, roots[0].identity.session_id, errors)
  const coldSkills = await fixture.memberApi(`/sessions/${roots[0].identity.session_id}/skills`)
  assert.deepEqual(coldSkills, { revision: 0, complete: false, skills: [] }, 'a cold read reports an incomplete catalog without starting execution')
  await page.locator('[data-composer-input]').fill('/skill ')
  await page.locator('[data-composer-menu]').getByText('技能目录尚未就绪。运行任务后重新打开此菜单查看。', { exact: true }).waitFor()
  assert.equal(await page.locator('[data-composer-menu] [role="alert"]').count(), 0)
  assert.equal(fixture.runs().length, 0)
  await page.screenshot({ path: path.join(artifacts, 'cold-skills-menu.png'), animations: 'disabled' })
  await page.locator('[data-composer-input]').fill('')
  const submissions = await Promise.all(roots.map((session, index) => fixture.submit(session.identity.session_id, `CAPACITY::wide::${index}::0`)))
  await until(() => { fixture.check(); return [...upstream.held.keys()].filter(key => /^wide-\d-2$/.test(key)) }, keys => keys.length === 4, 'four grandchildren occupy real Worker processes', fixture.diagnostic)
  const peak = fixture.snapshot()
  assert.equal(peak.resident, 12, 'four parents, four children and four grandchildren coexist instead of stalling at four JoinSet entries')
  assert.equal(peak.active, 4)
  assert.equal(peak.rows.filter(row => row.phase === 'parked').length, 8)
  assert.equal(fixture.approvals.length, 8, 'each parent and child launch passes real tool approval')
  const processes = await isolatedExecutions(fixture.worker.child.pid)
  assert.equal(processes.length, 12, 'all twelve residents are real isolated Worker execution processes')
  assert.equal(new Set(processes.map(process => process.mount)).size, 12)
  const lineage = fixture.query('SELECT run_id, root_run_id, parent_run_id, depth FROM cloud_run_lineage WHERE tenant_id=?', fixture.tenantId)
  for (const depth of [0, 1, 2]) assert.equal(lineage.filter(row => row.depth === depth).length, 4)
  assert.ok(lineage.every(row => submissions.some(submission => submission.run_id === row.root_run_id)))
  assert.equal(upstream.calls.filter(call => /^wide-\d-0$/.test(call.tag) && call.stage === 'start').length, 4, 'all initial parent model requests passed the four-way barrier')
  await status(page, 'waiting_for_subagents')
  const bindingEvents = (await fixture.api(`/sessions/${roots[0].identity.session_id}/events`))
    .filter(event => event.type === 'subagent_updated' || (event.type === 'tool_call_finished' && event.name === 'spawn_agent'))
  await writeFile(path.join(artifacts, 'wide-subagent-binding.json'), JSON.stringify(bindingEvents, null, 2))
  const childBinding = bindingEvents.find(event => event.type === 'subagent_updated')?.subagent
  const childSessionId = childBinding?.session_id
  assert.equal(typeof childSessionId, 'string')
  assert.equal(childBinding.transcript_kind, 'conversation')
  const childOwnerEvents = await fixture.api(`/sessions/${encodeURIComponent(childSessionId)}/events`)
  assert.ok(childOwnerEvents.length > 0, 'the independent child conversation really exists for its owner')
  const childAccess = await fetch(`${fixture.application.origin}/api/v1/sessions/${encodeURIComponent(childSessionId)}/events`, {
    headers: { authorization: `Bearer ${fixture.member.access_token}`, 'x-ternilo-tenant': fixture.tenantId },
  })
  const childAccessBody = await childAccess.json()
  assert.ok([400, 403, 404].includes(childAccess.status) && childAccessBody.error, 'sharing the parent alone does not grant access to the child conversation')
  await writeFile(path.join(artifacts, 'wide-subagent-access.json'), JSON.stringify({
    sharing: 'parent_session_only', childSessionId, status: childAccess.status,
    ownerEventCount: childOwnerEvents.length,
    error: childAccessBody.error ?? null, eventCount: Array.isArray(childAccessBody) ? childAccessBody.length : null,
  }, null, 2))
  const childCard = page.locator(`[data-subagent-event="${childBinding.subagent_id}"]`)
  await childCard.getByText(unavailableChildNotice, { exact: true }).waitFor()
  assert.equal((await childCard.textContent()).includes('没有发布独立会话记录'), false)
  assert.equal((await childCard.textContent()).includes(childBinding.task), true)
  assert.equal(await childCard.getByRole('button', { name: /^打开子 Agent 会话/ }).count(), 0)
  const scroll = await noOutputFollowing(page)
  await tail(page)
  await page.screenshot({ path: path.join(artifacts, 'wide-wait-desktop.png'), animations: 'disabled' })
  await mobile(page, path.join(artifacts, 'wide-wait-mobile.png'), 'waiting_for_subagents')
  await mobile(page, path.join(artifacts, 'wide-wait-320.png'), 'waiting_for_subagents', 320)
  for (let index = 0; index < 4; index++) upstream.release(`wide-${index}-2`)
  await allFinished(fixture, 12)
  assert.ok(fixture.runs().every(run => run.state === 'succeeded'), fixture.diagnostic())
  assert.equal(upstream.calls.filter(call => !call.title && call.tag.startsWith('wide-')).length, 28)
  const histories = []
  for (const submission of submissions) {
    const run = fixture.runs().find(run => run.run_id === submission.run_id)
    const events = await fixture.api(`/sessions/${run.session_id}/events`)
    for (const phase of ['waiting_for_subagents', 'waiting_for_capacity', 'running']) {
      assert.ok(events.some(event => event.run_id === run.run_id && event.type === 'execution_activity_changed' && event.phase === phase), `${phase} is a durable root event`)
    }
    assert.deepEqual(events.find(event => event.type === 'user_message' && event.run_id === run.run_id).provenance, submission.provenance)
    histories.push({ sessionId: run.session_id, events })
  }
  const evidence = await accountEvidence(fixture)
  await page.context().close()
  return { peak, processes, lineage, scroll, histories, ...evidence }
}

async function pressureScenario(browser, fixture, upstream, artifacts, errors) {
  const root = await fixture.session('pressure-root')
  const blocker = await fixture.session('pressure-blocker')
  const page = await pageFor(browser, fixture, root.identity.session_id, errors)
  const submission = await fixture.submit(root.identity.session_id, 'CAPACITY::pressure::0::0')
  const resumeGate = fixture.proxy.holdResume(submission.run_id)
  await until(() => { fixture.check(); return upstream.held.has('pressure-0-1') }, Boolean, 'child model holds the second resident', fixture.diagnostic)
  await status(page, 'waiting_for_subagents')
  const processes = await isolatedExecutions(fixture.worker.child.pid)
  assert.equal(processes.length, 2, 'parent and child occupy the configured two real resident processes')
  const spare = await fixture.startPeerWorker('capacity-spare')
  const spareRegistrations = fixture.query(`SELECT worker.worker_id,worker.generation,credential.storage_id,root.root_id
    FROM cloud_workers worker JOIN cloud_worker_credentials credential ON credential.worker_id=worker.worker_id
    JOIN cloud_storage_roots root ON root.storage_id=credential.storage_id WHERE worker.worker_id IN (?,?)`, 'capacity-worker', 'capacity-spare')
  assert.equal(spareRegistrations.length, 2)
  assert.equal(new Set(spareRegistrations.map(worker => worker.storage_id)).size, 1)
  assert.equal(new Set(spareRegistrations.map(worker => worker.root_id)).size, 1)
  await tail(page)
  await page.screenshot({ path: path.join(artifacts, 'pressure-subtasks-desktop.png'), animations: 'disabled' })
  upstream.release('pressure-0-1')
  const failed = await until(() => { fixture.check(); return fixture.runs() }, runs => runs.some(run => run.state === 'failed' && String(run.error).includes('capacity_exhausted')), 'queued grandchild gets an explicit resident capacity failure', fixture.diagnostic)
  const failedChild = failed.find(run => run.state === 'failed')
  assert.ok(String(failedChild.error).includes('capacity_exhausted'))
  assert.equal(fixture.snapshot().rows.some(row => row.run_id === failedChild.run_id), false, 'the exhausted queued descendant never acquired a resident slot')
  assert.equal(upstream.calls.some(call => call.tag === 'pressure-0-2'), false, 'resident exhaustion is settled before a forbidden third process invokes a model')
  assert.deepEqual(fixture.query('SELECT run_id FROM cloud_run_execution WHERE worker_id=?', 'capacity-spare'), [], 'the idle Worker cannot execute a child pinned to another holder')
  await spare.close()
  await writeFile(path.join(artifacts, 'pressure-spare-worker.log'), spare.diagnostics())
  await until(() => resumeGate.entered, Boolean, 'root resume request reaches the transport gate', fixture.diagnostic)
  await fixture.submit(blocker.identity.session_id, 'CAPACITY::blocker::0::0')
  await until(() => upstream.held.has('blocker-0-0'), Boolean, 'independent execution occupies the released foreground slot', fixture.diagnostic)
  fixture.proxy.releaseResume(submission.run_id)
  await until(() => fixture.snapshot(), state => state.rows.some(row => row.run_id === submission.run_id && row.phase === 'resume_pending'), 'Server persists real ResumePending while another run holds F1', fixture.diagnostic)
  await status(page, 'waiting_for_capacity')
  const draft = 'Keep this private draft while the parent waits to continue.'
  await page.locator('[data-composer-input]').fill(draft)
  await page.reload()
  await status(page, 'waiting_for_capacity')
  assert.equal(await page.locator('[data-composer-input]').inputValue(), draft, 'history reload retains both the durable wait and the independent draft')
  await tail(page)
  await page.screenshot({ path: path.join(artifacts, 'pressure-capacity-desktop.png'), animations: 'disabled' })
  await mobile(page, path.join(artifacts, 'pressure-capacity-mobile.png'), 'waiting_for_capacity')
  await mobile(page, path.join(artifacts, 'pressure-capacity-320.png'), 'waiting_for_capacity', 320)
  const events = await fixture.api(`/sessions/${root.identity.session_id}/events`)
  assert.ok(events.some(event => event.type === 'execution_activity_changed' && event.phase === 'waiting_for_capacity'))
  upstream.release('blocker-0-0')
  await allFinished(fixture, 4)
  assert.equal(fixture.runs().find(run => run.run_id === submission.run_id).state, 'succeeded')
  await page.locator('article[data-role="assistant"]').filter({ hasText: 'Recovered capacity_exhausted.' }).last().waitFor()
  assert.equal(await page.locator('[data-composer-input]').inputValue(), draft)
  assert.equal(await page.locator('[data-execution-phase]').count(), 0)
  const recovered = await accountEvidence(fixture)

  await page.setViewportSize({ width: 1440, height: 960 })
  const stoppedSession = await fixture.session('stop-root')
  await selectSession(page, fixture.tenantId, stoppedSession.identity.session_id)
  const stoppedSubmission = await fixture.submit(stoppedSession.identity.session_id, 'CAPACITY::stop::0::0')
  await until(() => upstream.held.has('stop-0-1'), Boolean, 'cancellable child is really executing', fixture.diagnostic)
  await status(page, 'waiting_for_subagents')
  const stoppedDraft = 'Stopping the current run must keep my next task draft.'
  await page.locator('[data-composer-input]').fill(stoppedDraft)
  await mobile(page, path.join(artifacts, 'stop-wait-mobile.png'), 'waiting_for_subagents')
  await mobile(page, path.join(artifacts, 'stop-wait-320.png'), 'waiting_for_subagents', 320)
  await page.locator('[data-input-bar]').getByRole('button', { name: '停止运行', exact: true }).click()
  await allFinished(fixture, 6)
  assert.equal(fixture.runs().find(run => run.run_id === stoppedSubmission.run_id).state, 'cancelled')
  assert.equal(await page.locator('[data-composer-input]').inputValue(), stoppedDraft)
  await page.locator('[data-input-bar]').getByRole('button', { name: '发送', exact: true }).waitFor()
  assert.equal(await page.locator('[data-execution-phase]').count(), 0)
  const cancelled = await accountEvidence(fixture, { unknownAllowed: true })
  await page.context().close()
  return { processes, spareRegistrations, failedChild, events, recovered, cancelled }
}

test('real Bubblewrap capacity supports nested parents and fails resident deadlocks without losing browser control', { timeout: 300_000 }, async context => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-execution-capacity-'))
  const artifacts = process.env.TERNILO_E2E_ARTIFACT_DIR ?? directory
  await mkdir(artifacts, { recursive: true })
  const browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
  const errors = [], results = {}
  let fixture, upstream, stage = 'setup'
  try {
    for (const [name, active, resident, verify] of [['wide', 4, 16, wideScenario], ['pressure', 1, 2, pressureScenario]]) {
      stage = name
      upstream = await modelFixture()
      fixture = await startScenario(path.join(directory, name), active, resident, upstream)
      context.diagnostic(`${name}: Server + Bubblewrap Worker ready with F${active}/R${resident}`)
      results[name] = await verify(browser, fixture, upstream, artifacts, errors)
      fixture.check()
      Object.assign(results[name], { assetHashes: fixture.assetHashes, samples: fixture.samples, approvals: fixture.approvals, modelCalls: upstream.calls, rpc: fixture.proxy.calls })
      await writeFile(path.join(artifacts, `${name}-worker.log`), fixture.worker.diagnostics())
      await writeFile(path.join(artifacts, `${name}-server.log`), fixture.application.diagnostics())
      await writeFile(path.join(artifacts, `${name}-observations.json`), JSON.stringify(results[name], null, 2))
      await fixture.close(); fixture = null
      await upstream.close(); upstream = null
    }
    assert.deepEqual(errors, [])
    await writeFile(path.join(artifacts, 'browser-observations.json'), JSON.stringify({ errors, stages: Object.keys(results) }, null, 2))
  } catch (error) {
    for (const [index, page] of browser.contexts().flatMap(value => value.pages()).entries()) {
      await page.screenshot({ path: path.join(artifacts, `failure-${stage}-${index}.png`), animations: 'disabled' }).catch(() => {})
    }
    await writeFile(path.join(artifacts, 'failure.json'), JSON.stringify({ stage, error: error.stack, errors,
      modelCalls: upstream?.calls, modelFailures: upstream?.failures, heldModels: upstream ? [...upstream.held.keys()] : [],
      rpc: fixture?.proxy.calls, proxyFailures: fixture?.proxy.failures, samples: fixture?.samples, runs: fixture?.runs(),
      worker: fixture?.worker.diagnostics(), server: fixture?.application.diagnostics(),
    }, null, 2))
    throw error
  } finally {
    await browser.close()
    await fixture?.close()
    await upstream?.close()
    if (!process.env.TERNILO_E2E_KEEP_TEMP) await rm(directory, { recursive: true, force: true })
  }
})
