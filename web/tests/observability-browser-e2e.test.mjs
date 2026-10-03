import { selectChoice } from './browser-select-fixture.mjs'
import assert from 'node:assert/strict'
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import { spawn } from 'node:child_process'
import test from 'node:test'
import { chromium } from 'playwright'

const webRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..')
const repository = path.resolve(webRoot, '..')
const binary = path.join(repository, 'target', 'debug', 'ternilo')

function startTernilo(dataDirectory) {
  const child = spawn(binary, ['serve', '--listen', '127.0.0.1:0', '--data-dir', dataDirectory], {
    cwd: repository, stdio: ['ignore', 'pipe', 'pipe'],
  })
  let diagnostics = ''
  child.stderr.setEncoding('utf8')
  child.stderr.on('data', chunk => { diagnostics += chunk })
  const origin = new Promise((resolve, reject) => {
    let output = ''
    child.stdout.setEncoding('utf8')
    child.stdout.on('data', chunk => {
      output += chunk
      const match = output.match(/Ternilo local web: (http:\/\/[^\s]+)/)
      if (match) resolve(match[1])
    })
    child.once('exit', code => reject(new Error(`Ternilo exited ${code}: ${diagnostics}`)))
    child.once('error', reject)
  })
  return { child, origin }
}

async function stopProcess(child) {
  if (child.exitCode !== null) return
  child.kill('SIGINT')
  await Promise.race([
    new Promise(resolve => child.once('exit', resolve)),
    new Promise(resolve => setTimeout(resolve, 5_000)).then(() => child.kill('SIGKILL')),
  ])
}

async function api(page, endpoint, init = {}) {
  return page.evaluate(async ({ endpoint, init }) => {
    const token = window.__TERNILO_BOOT__?.apiToken ?? ''
    const response = await fetch(`/api/v1${endpoint}`, {
      ...init,
      headers: { ...(token ? { authorization: `Bearer ${token}` } : {}), ...(init.body === undefined ? {} : { 'content-type': 'application/json' }) },
      body: init.body === undefined ? undefined : JSON.stringify(init.body),
    })
    if (!response.ok) throw new Error(`${endpoint}: ${response.status} ${await response.text()}`)
    return response.status === 204 ? null : response.json()
  }, { endpoint, init })
}

async function send(page, command) {
  const input = page.getByRole('textbox', { name: '输入任务' })
  await input.waitFor()
  await input.fill(command)
  await page.getByRole('button', { name: '发送' }).click()
}

async function approve(page, command, verifyPending = false) {
  await send(page, command)
  const approval = page.locator('[data-tool-approval]')
  await approval.waitFor()
  if (verifyPending) {
    const activeRow = page.locator('[data-sidebar-session-row]').filter({ has: page.locator('[data-sidebar-session-active]') })
    await activeRow.locator('[data-state="warning"]').waitFor({ timeout: 8_000 })
    await activeRow.hover()
    await page.getByText('等待授权', { exact: true }).last().waitFor()
  }
  await approval.getByRole('button', { name: '允许一次' }).click()
  await approval.waitFor({ state: 'detached' })
  await page.getByRole('textbox', { name: '输入任务' }).waitFor()
}

async function noOverflow(page, label) {
  const size = await page.evaluate(() => ({
    viewport: document.documentElement.clientWidth,
    document: document.documentElement.scrollWidth,
    body: document.body.scrollWidth,
  }))
  assert.equal(size.document <= size.viewport, true, `${label}: ${JSON.stringify(size)}`)
  assert.equal(size.body <= size.viewport, true, `${label}: ${JSON.stringify(size)}`)
}

test('Observability surfaces use real Salvo events across desktop and mobile', { timeout: 120_000 }, async () => {
  const dataDirectory = await mkdtemp(path.join(tmpdir(), 'ternilo-observability-'))
  const workspacePath = path.join(dataDirectory, 'workspace')
  await mkdir(workspacePath)
  const slowAcpFixture = path.join(workspacePath, 'slow-acp-fixture.py')
  await writeFile(slowAcpFixture, `import json
import sys
import time

for line in sys.stdin:
    frame = json.loads(line)
    method = frame.get("method")
    request_id = frame.get("id")
    if method == "initialize":
        result = {"protocolVersion": 1}
    elif method == "session/new":
        result = {"sessionId": "slow-browser-session"}
    elif method == "session/prompt":
        time.sleep(30)
        result = {"stopReason": "end_turn"}
    else:
        continue
    print(json.dumps({"jsonrpc": "2.0", "id": request_id, "result": result}, separators=(",", ":")), flush=True)
`)
  const ternilo = startTernilo(dataDirectory)
  let browser
  try {
    const origin = await ternilo.origin
    browser = await chromium.launch({ headless: true })
    const page = await browser.newPage({ viewport: { width: 1440, height: 900 } })
    const pageErrors = []
    page.on('pageerror', error => pageErrors.push(error.message))
    await page.goto(origin, { waitUntil: 'networkidle' })

    const workspace = await api(page, '/workspaces', { method: 'POST', body: { path: workspacePath } })
    const background = await api(page, '/sessions', { method: 'POST', body: { workspace_id: workspace.workspace_id, agent_preset: 'standard' } })
    const observed = await api(page, '/sessions', { method: 'POST', body: { workspace_id: workspace.workspace_id, agent_preset: 'standard' } })
    const backgroundId = background.identity.session_id
    const observedId = observed.identity.session_id
    await api(page, `/sessions/${encodeURIComponent(backgroundId)}`, { method: 'PATCH', body: { title: 'Background status' } })
    await api(page, `/sessions/${encodeURIComponent(observedId)}`, { method: 'PATCH', body: {
      title: 'Observability',
      profile_plugins: [{
        id: 'slow-browser-acp', kind: 'ternilo.subagents.acp', enabled: true,
        config: {
          provider_name: 'slow-browser', command: 'python3', args: [slowAcpFixture],
          shutdown_grace_ms: 100,
        },
      }],
    } })
    await page.evaluate(id => localStorage.setItem('ternilo.current-session', id), observedId)
    await page.evaluate(() => localStorage.setItem('ternilo.transcript-view', 'normal'))
    await page.reload({ waitUntil: 'domcontentloaded' })
    await page.getByRole('textbox', { name: '输入任务' }).waitFor()

    // A non-current real run drives running -> completed/unviewed -> cleared-on-open.
    await page.evaluate(({ sessionId }) => {
      const token = window.__TERNILO_BOOT__?.apiToken ?? ''
      window.__observabilityBackgroundTurn = fetch(`/api/v1/sessions/${encodeURIComponent(sessionId)}/turns`, {
        method: 'POST',
        headers: {
          'content-type': 'application/json',
          ...(token ? { authorization: `Bearer ${token}` } : {}),
        },
        body: JSON.stringify({
          run_id: 'sidebar-background-run',
          input: '/shell sleep 8; echo inactive-done',
          attachments: [],
        }),
      })
      return true
    }, { sessionId: backgroundId })
    const backgroundRow = page.locator('[data-sidebar-session-row]').filter({
      hasNot: page.locator('[data-sidebar-session-active]'),
    }).first()
    await backgroundRow.locator('[data-state="running"]').waitFor({ timeout: 15_000 })
    await backgroundRow.locator('[data-state="completed"]').waitFor({ timeout: 15_000 }).catch(async cause => {
      const events = await api(page, `/sessions/${encodeURIComponent(backgroundId)}/events`)
      const tail = events.slice(-8).map(event => ({ type: event.type, name: event.name, output: event.output, answer: event.answer }))
      throw new Error(`background completion missing: ${await backgroundRow.innerText()}; events=${JSON.stringify(tail)}`, { cause })
    })
    await backgroundRow.hover()
    await page.getByText('已完成，尚未查看', { exact: true }).waitFor()
    await backgroundRow.locator('[data-sidebar-session-button]').click()
    const currentRow = page.locator('[data-sidebar-session-row]').filter({
      has: page.locator('[data-sidebar-session-active]'),
    })
    await currentRow.locator('[data-state="running"], [data-state="warning"], [data-state="completed"]').waitFor({ state: 'detached' })
    await page.locator('[data-sidebar-session-row]').filter({
      hasNot: page.locator('[data-sidebar-session-active]'),
    }).first().locator('[data-sidebar-session-button]').click()

    // Job list is absent before use, then follows canonical lifecycle events without /jobs.
    assert.equal(await page.locator('[data-job-list-action]').count(), 0)
    await send(page, '/job sleep 0.2; echo observed-job')
    const jobs = page.locator('[data-job-list-action]')
    await jobs.waitFor()
    await jobs.getByRole('button', { name: /1 个后台任务/ }).click()
    await jobs.locator('[data-job-status="running"]').waitFor()
    await page.keyboard.press('Escape')
    await jobs.getByRole('button', { name: /1 个后台任务/ }).click()
    await jobs.locator('[data-job-status="completed"]').waitFor({ timeout: 5_000 })
    assert.match(await jobs.locator('[data-job-status="completed"]').textContent(), /observed-job/)
    await page.keyboard.press('Escape')

    // Dangerous schedule/subagent/workflow commands prove pending attention and real projections.
    await approve(page, '/schedule-after 120 inspect release', true)
    const schedule = page.locator('[data-schedule-operation="create"]')
    await schedule.waitFor()
    assert.match(await schedule.textContent(), /单次/)
    assert.match(await schedule.textContent(), /inspect release/)

    await approve(page, '/agent inspect workspace')
    const subagent = page.locator('[data-subagent-event]').first()
    await subagent.waitFor()
    await page.waitForFunction(() => document.querySelector('[data-subagent-event]')?.getAttribute('data-status') === 'idle')
    assert.match(await subagent.textContent(), /inspect workspace/)
    await subagent.getByRole('button', { name: '打开子 Agent 会话“inspect workspace”' }).waitFor()
    await subagent.getByRole('button', { name: /在 Agent Team 中打开/ }).click()
    let team = page.getByRole('dialog', { name: 'Agent Team' })
    await team.waitFor()
    let member = team.locator('[data-agent-team-member]').filter({ hasText: 'inspect workspace' })
    const inProcessAgentId = await member.getAttribute('data-agent-team-member')
    assert.ok(inProcessAgentId)
    assert.equal(await member.getAttribute('data-status'), 'idle')
    assert.match(await member.textContent(), /in-process/)
    await member.getByRole('button', { name: '打开独立会话' }).waitFor()
    const initialOutput = await member.locator('[data-agent-team-output] pre').textContent()
    assert.ok(initialOutput)

    // The canonical Team endpoint owns a shared task board and mailbox. Create
    // and fully replace a task from the root before continuing the live Agent.
    await team.locator('#agent-team-tasks-tab').click()
    await team.getByRole('button', { name: '新建任务' }).click()
    let taskForm = team.locator('[data-agent-team-task-form="create"]')
    await taskForm.getByLabel('任务名称').fill('Review browser Team')
    await taskForm.getByLabel('详细说明').fill('Verify the shared task board')
    await selectChoice(taskForm.getByLabel('负责人'), { label: 'inspect workspace' })
    await taskForm.getByRole('button', { name: '保存' }).click()
    let sharedTask = team.locator('[data-agent-team-task]').filter({ hasText: 'Review browser Team' })
    await sharedTask.waitFor()
    assert.equal(await sharedTask.getAttribute('data-status'), 'pending')
    await sharedTask.getByRole('button', { name: '编辑任务“Review browser Team”' }).click()
    taskForm = team.locator('[data-agent-team-task-form="edit"]')
    await selectChoice(taskForm.getByLabel('状态'), 'in_progress')
    await taskForm.getByRole('button', { name: '保存' }).click()
    sharedTask = team.locator('[data-agent-team-task]').filter({ hasText: 'Review browser Team' })
    await page.waitForFunction(() => document.querySelector('[data-agent-team-task]')?.getAttribute('data-status') === 'in_progress')

    await team.locator('#agent-team-mailbox-tab').click()
    await selectChoice(team.getByLabel('收件人'), { label: 'inspect workspace' })
    await team.getByRole('textbox', { name: '消息', exact: true }).fill('Mailbox from root')
    await team.getByRole('button', { name: '发送', exact: true }).click()
    const outgoing = team.locator('[data-agent-team-mailbox] li').filter({ hasText: 'Mailbox from root' })
    await outgoing.waitFor()
    assert.match(await outgoing.textContent(), /未读/)
    assert.equal(await outgoing.getByRole('button', { name: '标为已读' }).count(), 0)
    await team.locator('#agent-team-roster-tab').click()
    await team.getByRole('textbox', { name: '后续任务' }).fill('verify browser follow-up')
    await team.getByRole('button', { name: '发送', exact: true }).click()
    await team.waitFor({ state: 'detached' })
    await page.waitForFunction(({ id, previous }) => {
      const card = document.querySelector(`[data-subagent-event="${CSS.escape(id)}"]`)
      const output = card?.querySelector('details pre')?.textContent
      return card?.getAttribute('data-status') === 'idle' && Boolean(output) && output !== previous
    }, { id: inProcessAgentId, previous: initialOutput })

    const lineage = page.locator('[data-session-lineage]')
    await lineage.waitFor()
    await lineage.getByRole('button', { name: /1 个子 Agent/ }).click()
    await lineage.getByRole('button', { name: '打开子 Agent 会话“inspect workspace”' }).waitFor()
    await lineage.getByRole('button', { name: /在 Agent Team 中打开/ }).click()
    team = page.getByRole('dialog', { name: 'Agent Team' })
    await team.waitFor()
    member = team.locator(`[data-agent-team-member="${inProcessAgentId}"]`)
    assert.equal(await member.getAttribute('data-selected'), 'true')
    assert.notEqual(await member.locator('[data-agent-team-output] pre').textContent(), initialOutput)
    const stateWithChild = await api(page, '/state')
    const childSession = stateWithChild.sessions.find(session => (
      session.parent_session_id === observedId
      && session.subagent?.subagent_id === inProcessAgentId
      && session.subagent?.transcript_kind === 'conversation'
    ))
    assert.ok(childSession, 'in-process subagent must persist a canonical conversation Session')
    assert.notEqual(childSession.identity.session_id, inProcessAgentId)
    await team.getByRole('button', { name: '打开独立会话' }).click()
    await team.waitFor({ state: 'detached' })
    await page.waitForFunction(id => localStorage.getItem('ternilo.current-session') === id, childSession.identity.session_id)
    await page.getByText('ternilo: verify browser follow-up', { exact: false }).waitFor()

    // Opening Team from a child uses that explicit Session context, resolves
    // the same Team, and exposes only the child member's mailbox slice.
    await page.getByRole('button', { name: '打开 Agent Team' }).click()
    team = page.getByRole('dialog', { name: 'Agent Team' })
    await team.waitFor()
    assert.doesNotMatch(await team.textContent(), new RegExp(childSession.identity.session_id.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')))
    await team.locator('#agent-team-tasks-tab').click()
    sharedTask = team.locator('[data-agent-team-task]').filter({ hasText: 'Review browser Team' })
    await sharedTask.waitFor()
    assert.equal(await sharedTask.getAttribute('data-status'), 'in_progress')
    await team.locator('#agent-team-mailbox-tab').click()
    const incoming = team.locator('[data-agent-team-mailbox] li[data-unread]').filter({ hasText: 'Mailbox from root' })
    await incoming.waitFor()
    await incoming.getByRole('button', { name: '标为已读' }).click()
    await incoming.waitFor({ state: 'detached' })
    await team.getByRole('button', { name: '关闭' }).click()
    await team.waitFor({ state: 'detached' })

    // A child is a normal, recursively capable Harness Session. Its nested
    // child keeps the real parent relation and remains navigable after reload.
    await approve(page, '/agent nested browser child')
    const nestedCard = page.locator('[data-subagent-event]').filter({ hasText: 'nested browser child' })
    await nestedCard.waitFor()
    await page.waitForFunction(() => [...document.querySelectorAll('[data-subagent-event]')]
      .some(card => card.textContent?.includes('nested browser child') && card.getAttribute('data-status') === 'idle'))
    const stateWithGrandchild = await api(page, '/state')
    const grandchildSession = stateWithGrandchild.sessions.find(session => (
      session.parent_session_id === childSession.identity.session_id
      && session.subagent?.transcript_kind === 'conversation'
    ))
    assert.ok(grandchildSession, 'nested subagent must persist beneath the child Session')
    await page.reload({ waitUntil: 'domcontentloaded' })
    await page.getByText('ternilo: verify browser follow-up', { exact: false }).waitFor()
    await page.locator('[data-session-lineage]').getByRole('button').first().click()
    await page.locator('[data-session-lineage]').getByRole('button', { name: '打开子 Agent 会话“nested browser child”' }).waitFor()
    await page.keyboard.press('Escape')

    await page.evaluate(id => localStorage.setItem('ternilo.current-session', id), observedId)
    await page.reload({ waitUntil: 'domcontentloaded' })
    await page.getByRole('textbox', { name: '输入任务' }).waitFor()
    await lineage.getByRole('button', { name: /1 个子 Agent/ }).click()
    await page.locator(`[data-lineage-session="${grandchildSession.identity.session_id}"]`).waitFor()
    await page.keyboard.press('Escape')

    // A one-shot ACP child still exposes Stop while running. Follow-up support
    // controls only the follow-up composer, never interruption.
    await approve(page, '/agent-bg-on slow-browser hold until stopped')
    const slowCard = page.locator('[data-subagent-event][data-status="running"]').filter({ hasText: 'hold until stopped' })
    await slowCard.waitFor()
    const slowAgentId = await slowCard.getAttribute('data-subagent-event')
    assert.ok(slowAgentId)
    await slowCard.getByRole('button', { name: /在 Agent Team 中打开/ }).click()
    team = page.getByRole('dialog', { name: 'Agent Team' })
    await team.waitFor()
    const slowMember = team.locator('[data-agent-team-member][data-status="running"]').filter({ hasText: 'hold until stopped' })
    assert.match(await slowMember.textContent(), /slow-browser/)
    assert.match(await slowMember.textContent(), /一次性 Agent/)
    assert.equal(await slowMember.getByRole('textbox', { name: '后续任务' }).count(), 0)

    // Keep the owner Session genuinely busy while addressing the child. Stop
    // must bypass both the owner turn gate and its durable submission queue.
    await page.evaluate(({ sessionId }) => {
      const token = window.__TERNILO_BOOT__?.apiToken ?? ''
      window.__observabilityOwnerBusyTurn = fetch(`/api/v1/sessions/${encodeURIComponent(sessionId)}/turns`, {
        method: 'POST',
        headers: {
          'content-type': 'application/json',
          ...(token ? { authorization: `Bearer ${token}` } : {}),
        },
        body: JSON.stringify({
          run_id: 'owner-busy-direct-subagent',
          input: '/shell sleep 30; echo owner-finished',
          attachments: [],
        }),
      })
      return true
    }, { sessionId: observedId })
    await page.locator('[data-composer-card][data-busy]').waitFor({ timeout: 10_000 })
    const ownerQueueBeforeStop = await api(page, `/sessions/${encodeURIComponent(observedId)}/queue`)
    assert.equal(ownerQueueBeforeStop.active_run_id, 'owner-busy-direct-subagent')
    const eventsBeforeStop = await api(page, `/sessions/${encodeURIComponent(observedId)}/events`)
    const ownerTurnsBeforeStop = eventsBeforeStop.filter(event => event.type === 'turn_started').length

    await slowMember.getByRole('button', { name: '停止', exact: true }).click()
    await team.waitFor({ state: 'detached' })
    await page.waitForFunction(() => {
      const cards = [...document.querySelectorAll('[data-subagent-event]')]
      return cards.some(card => card.textContent?.includes('hold until stopped') && card.getAttribute('data-status') === 'cancelled')
    })
    const ownerQueueAfterStop = await api(page, `/sessions/${encodeURIComponent(observedId)}/queue`)
    assert.equal(ownerQueueAfterStop.active_run_id, 'owner-busy-direct-subagent')
    assert.equal(ownerQueueAfterStop.items.some(item => item.content?.input?.startsWith('/agent-stop ')), false)
    const eventsAfterStop = await api(page, `/sessions/${encodeURIComponent(observedId)}/events`)
    assert.equal(eventsAfterStop.filter(event => event.type === 'turn_started').length, ownerTurnsBeforeStop)
    assert.equal(eventsAfterStop.some(event => event.type === 'user_message' && event.content?.startsWith('/agent-stop ')), false)
    await api(page, `/sessions/${encodeURIComponent(observedId)}/turns/owner-busy-direct-subagent`, { method: 'DELETE' })
    await page.locator('[data-composer-card][data-busy]').waitFor({ state: 'detached', timeout: 10_000 })

    const stateWithLifecycle = await api(page, '/state')
    const lifecycleSession = stateWithLifecycle.sessions.find(session => (
      session.parent_session_id === observedId
      && session.subagent?.subagent_id === slowAgentId
      && session.subagent?.transcript_kind === 'process_lifecycle'
    ))
    assert.ok(lifecycleSession, 'ACP subagent must persist a truthful lifecycle Session')
    const slowSettledCard = page.locator(`[data-subagent-event="${slowAgentId}"]`)
    await slowSettledCard.getByRole('button', { name: /打开子 Agent 会话/ }).click()
    await page.locator('[data-subagent-lifecycle-only]').waitFor()
    assert.equal(await page.getByRole('textbox', { name: '输入任务' }).count(), 0)
    await page.reload({ waitUntil: 'domcontentloaded' })
    await page.locator('[data-subagent-lifecycle-only]').waitFor()
    await page.evaluate(id => localStorage.setItem('ternilo.current-session', id), observedId)
    await page.reload({ waitUntil: 'domcontentloaded' })
    await page.getByRole('textbox', { name: '输入任务' }).waitFor()

    // Agent Team remains a usable bounded dialog in both phone orientations.
    for (const viewport of [{ width: 390, height: 844 }, { width: 844, height: 390 }]) {
      await page.setViewportSize(viewport)
      await page.locator(`[data-subagent-event="${inProcessAgentId}"]`).getByRole('button', { name: /在 Agent Team 中打开/ }).click()
      team = page.getByRole('dialog', { name: 'Agent Team' })
      await team.waitFor()
      await team.evaluate(async element => {
        await Promise.all(element.getAnimations({ subtree: true }).map(animation => animation.finished))
      })
      const geometry = await team.evaluate(element => {
        const box = element.getBoundingClientRect()
        return { left: box.left, top: box.top, right: box.right, bottom: box.bottom, width: innerWidth, height: innerHeight }
      })
      assert.ok(geometry.left >= 0 && geometry.top >= 0 && geometry.right <= geometry.width && geometry.bottom <= geometry.height, JSON.stringify(geometry))
      const sendBox = await team.getByRole('button', { name: '发送', exact: true }).boundingBox()
      const closeBox = await team.getByRole('button', { name: '关闭' }).boundingBox()
      assert.ok(sendBox && sendBox.height >= 40, `Agent Team send target ${viewport.width}x${viewport.height}: ${JSON.stringify(sendBox)}`)
      assert.ok(closeBox && closeBox.width >= 40 && closeBox.height >= 40, `Agent Team close target ${viewport.width}x${viewport.height}: ${JSON.stringify(closeBox)}`)
      await noOverflow(page, `Agent Team ${viewport.width}x${viewport.height}`)
      await team.getByRole('button', { name: '关闭' }).click()
      await team.waitFor({ state: 'detached' })
    }
    await page.setViewportSize({ width: 1440, height: 900 })

    const workflow = JSON.stringify({
      meta: { name: 'review-one', description: 'Review one input', phases: [{ title: 'scan', detail: 'Inspect input' }] },
      script: 'phase("scan"); agent("review", #{ label: "reviewer" })', args: {},
    })
    await approve(page, `/workflow ${workflow}`)
    const workflowPanel = page.locator('[data-workflow-run]').first()
    await workflowPanel.waitFor()
    await page.waitForFunction(() => document.querySelector('[data-workflow-run]')?.getAttribute('data-status') === 'completed')
    const workflowHeader = workflowPanel.getByRole('button').first()
    if (await workflowHeader.getAttribute('aria-expanded') === 'false') await workflowHeader.click()
    assert.match(await workflowPanel.textContent(), /Workflow · review-one/)
    assert.match(await workflowPanel.textContent(), /scan/)
    assert.match(await workflowPanel.textContent(), /1 个成员/)

    // Phone, short portrait, and landscape keep popovers in the viewport.
    for (const viewport of [{ width: 390, height: 844 }, { width: 390, height: 430 }, { width: 844, height: 390 }]) {
      await page.setViewportSize(viewport)
      await noOverflow(page, `${viewport.width}x${viewport.height}`)
    }
    await page.setViewportSize({ width: 390, height: 844 })
    await jobs.getByRole('button').click()
    const jobMenuBox = await jobs.getByRole('list').boundingBox()
    assert.ok(jobMenuBox && jobMenuBox.x >= 0 && jobMenuBox.x + jobMenuBox.width <= 390)
    await page.keyboard.press('Escape')
    await lineage.getByRole('button').click()
    const lineageBox = await lineage.getByRole('tree').boundingBox()
    assert.ok(lineageBox && lineageBox.x >= 0 && lineageBox.x + lineageBox.width <= 390)
    await page.keyboard.press('Escape')
    await noOverflow(page, 'open mobile observability controls')

    await page.evaluate(() => localStorage.setItem('ternilo.locale', 'en'))
    await page.reload({ waitUntil: 'domcontentloaded' })
    await page.getByRole('button', { name: /background job record/ }).waitFor()
    const englishLineage = page.locator('[data-session-lineage]').getByRole('button').first()
    await englishLineage.waitFor()
    assert.match(await englishLineage.getAttribute('aria-label') ?? '', /subagents?/i)
    await page.getByText('Workflow · review-one', { exact: false }).waitFor()
    await noOverflow(page, 'English mobile')
    assert.deepEqual(pageErrors, [])
  } finally {
    if (browser) await browser.close()
    await stopProcess(ternilo.child)
    await rm(dataDirectory, { recursive: true, force: true })
  }
})
