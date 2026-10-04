import assert from 'node:assert/strict'
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { chromium } from 'playwright'
import { freePort, initializeServer, serverRequest, startProcess, stopProcess, waitForHttp } from './platform-e2e-fixture.mjs'
import { localApi, until } from './model-device-fixture.mjs'
import {
  cliOutcome, cliProfile, configureModel, directoryModel, history, isolatedEnvironment, modelSelection,
  nodeBinary, noWaiting, observe, openSession, pendingQuestion, queue, screenshots, send,
  startCli, taskInput, terminal, verifyAssets, waiting,
} from './user-directory-concurrency-fixture.mjs'

test('Server account holders and their local CLI share a directory without releasing other holders or admitting another account early', { timeout: 210_000 }, async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-user-directory-browser-'))
  const artifacts = path.join(process.env.TERNILO_E2E_ARTIFACT_DIR ?? directory, 'server')
  await mkdir(artifacts, { recursive: true })
  const processes = [], clients = [], pages = []
  const observations = { errors: [], console: [], network: [], runs: [], barriers: [], assetHashes: {} }
  let browser, model
  try {
    const environment = await isolatedEnvironment(directory)
    model = await directoryModel()
    const server = await initializeServer({ directory: path.join(directory, 'server'), origin: `http://127.0.0.1:${await freePort()}`, mode: 'multi_user', environment })
    processes.push(server)
    let tenantId = server.owner.session.personal_tenant_id
    const owner = (resource, options = {}) => serverRequest(server.origin, resource, { token: server.owner.session.access_token, tenantId, ...options })
    const registration = await owner('/admin/registration')
    await owner('/admin/registration', { method: 'PATCH', body: { mode: 'open', require_approval: false, revision: registration.revision } })
    const credentials = { username: 'directory-member', email: 'directory-member@example.test', password: 'directory-member-password' }
    const { session: account } = await serverRequest(server.origin, '/auth/register', { body: credentials })
    const { tenant } = await owner('/tenants', { body: { slug: 'directory-team', display_name: 'Directory team' } })
    tenantId = tenant.tenant_id
    await owner(`/tenants/${tenantId}/members/${account.user.user_id}`, { method: 'PUT', body: { role: 'member' } })
    const member = (resource, options = {}) => serverRequest(server.origin, resource, { token: account.access_token, tenantId, ...options })
    const enrolled = await owner(`/tenants/${tenantId}/my-computer-enrollments`, { body: { name: 'directory-node', project_id: null, ttl_seconds: 600 } })
    const enrolledComputerId = enrolled.enrollment.executor_id
    const credential = await owner('/enrollments/consume', { body: { token: enrolled.enrollment.token } })
    const origin = `http://127.0.0.1:${await freePort()}`
    const node = startProcess(nodeBinary, ['serve', '--listen', new URL(origin).host, '--data-dir', path.join(directory, 'node'),
      '--node-id', enrolledComputerId, '--gateway-url', `${server.origin.replace('http:', 'ws:')}/api/v1/executors/connect`, '--allow-insecure-gateway'],
    { ...environment, TERNILO_LOCAL_TOKEN: credential.credential.token })
    processes.push(node)
    await waitForHttp(origin, node)
    observations.assetHashes.local = await verifyAssets(origin)
    observations.assetHashes.server = await verifyAssets(server.origin)
    const local = await localApi(origin)
    await configureModel(local, model)
    const folders = [path.join(directory, 'shared'), path.join(directory, 'independent')], workspaces = []
    for (const folder of folders) {
      await mkdir(folder)
      workspaces.push(await local('/workspaces', { body: { path: folder } }))
    }
    const titles = ['Owner model holder', 'Owner question holder', 'Shared member session', 'Owner peer', 'Independent directory']
    for (const [index, title] of titles.entries()) {
      const created = await local('/sessions', { body: { workspace_id: workspaces[index === 4 ? 1 : 0].workspace_id } })
      await local(`/sessions/${created.identity.session_id}`, { method: 'PATCH', body: { title, model: modelSelection } })
    }
    const state = await until(() => owner('/state'), value => titles.every(title => value.sessions.some(session => session.title === title)), 'all ordinary Node sessions map to Server')
    const sessions = titles.map(title => state.sessions.find(session => session.title === title))
    const [firstId, secondId, memberId, peerId, independentId] = sessions.map(session => session.identity.session_id)
    const workspaceId = sessions[0].workspace_id
    assert.ok(sessions.slice(0, 4).every(session => session.workspace_id === workspaceId && session.placement === 'local_node'))
    await owner(`/workspaces/${workspaceId}/sharing/user/${account.user.user_id}`, { method: 'PUT', body: { view: true, submit: true, stop: true, configure: true } })
    const sharedState = await member('/state')
    assert.ok(sharedState.workspaces.some(workspace => workspace.workspace_id === workspaceId))
    assert.ok(sharedState.sessions.some(session => session.identity.session_id === memberId))
    assert.equal(sessions[2].identity.user_id, server.owner.session.user.user_id, 'the shared session resource still belongs to the computer owner, not its submitter')

    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
    const open = async (sessionId, credentials, name) => {
      const page = await browser.newPage({ locale: 'zh-CN', viewport: { width: 1280, height: 900 }, hasTouch: true, serviceWorkers: 'block' })
      pages.push(page)
      observe(page, name, observations)
      await openSession(page, server.origin, sessionId, credentials, tenantId)
      return page
    }
    const firstPage = await open(firstId, server.owner, 'owner-first')
    const secondPage = await open(secondId, server.owner, 'owner-second')
    const memberPage = await open(memberId, credentials, 'shared-member')
    const assertAuthor = (events, userId) => {
      const input = events.find(event => event.type === 'user_message')
      assert.equal(input?.provenance?.author.kind, 'account')
      assert.equal(input.provenance.author.user_id, userId, 'directory identity follows the accepted input author, not resource ownership')
    }
    const first = await send(firstPage, 'hold-server-owner')
    await model.reached('hold-server-owner')
    const second = await send(secondPage, 'server-same-account-parallel-proof')
    const secondEvents = await terminal(owner, secondId, second.run_id)
    noWaiting(secondEvents)
    assertAuthor(secondEvents, server.owner.session.user.user_id)
    observations.runs.push(secondEvents)
    assert.equal(model.count('server-same-account-parallel-proof'), 1)
    assert.equal(model.held.has('hold-server-owner'), true)
    await secondPage.locator('article[data-role="assistant"]').filter({ hasText: 'Completed: server-same-account-parallel-proof' }).waitFor()
    await screenshots(secondPage, artifacts, 'same-account-parallel')
    const asking = await send(secondPage, '/ask Keep the other account waiting until this task finishes?')
    const question = secondPage.locator('[data-question-takeover]')
    await question.getByText('Keep the other account waiting until this task finishes?', { exact: true }).waitFor()
    const [pending] = await pendingQuestion(owner, secondId)
    await screenshots(secondPage, artifacts, 'owner-pending-question')
    noWaiting((await history(owner, secondId)).filter(event => event.run_id === asking.run_id))
    await taskInput(firstPage).fill('Owner draft remains private')

    const enterWaiting = async (accepted, marker) => {
      await waiting(memberPage).waitFor()
      const events = await until(() => history(member, memberId), events => events.some(event => event.run_id === accepted.run_id && event.type === 'workspace_execution_waiting'), 'different account waits for the actual directory lease')
      const run = events.filter(event => event.run_id === accepted.run_id)
      assertAuthor(run, account.user.user_id)
      assert.equal(run.some(event => event.type === 'workspace_execution_acquired'), false)
      assert.equal(model.count(marker), 0, 'the waiting account has not reached the model')
    }
    const stillBlocked = async (accepted, marker, label) => {
      const barrier = await queue(owner, independentId, 'independent-barrier-' + label)
      observations.runs.push(await terminal(owner, independentId, barrier.run_id))
      assert.equal(model.count('independent-barrier-' + label), 1)
      for (let attempt = 0; attempt < 8; attempt += 1) {
        const events = (await history(member, memberId)).filter(event => event.run_id === accepted.run_id)
        assert.equal(events.some(event => ['workspace_execution_acquired', 'turn_finished', 'turn_failed', 'turn_cancelled'].includes(event.type)), false, label + ': another holder still owns the directory')
        assert.equal(model.count(marker), 0, label + ': no real model request can escape the directory gate')
        assert.equal((await member(`/sessions/${memberId}/queue`)).active_run_id, accepted.run_id)
        await new Promise(resolve => setTimeout(resolve, 100))
      }
      observations.barriers.push({ label, blocked_run_id: accepted.run_id, independent_run_id: barrier.run_id })
    }
    const cancelled = await send(memberPage, 'cancel-other-account-waiter')
    await enterWaiting(cancelled, 'cancel-other-account-waiter')
    await taskInput(memberPage).fill('Member draft survives cancelling only this waiter')
    assert.equal(await taskInput(firstPage).inputValue(), 'Owner draft remains private')
    assert.equal(await memberPage.getByText('正在深入思考…', { exact: true }).count(), 0)
    await screenshots(memberPage, artifacts, 'different-account-waiting')
    await memberPage.getByRole('button', { name: '停止运行', exact: true }).click()
    const cancelledEvents = await terminal(member, memberId, cancelled.run_id, 'turn_cancelled')
    observations.runs.push(cancelledEvents)
    assert.equal(cancelledEvents.some(event => event.type === 'workspace_execution_acquired'), false)
    assert.equal(model.count('cancel-other-account-waiter'), 0)
    await waiting(memberPage).waitFor({ state: 'hidden' })
    assert.equal(await taskInput(memberPage).inputValue(), 'Member draft survives cancelling only this waiter')
    assert.equal(model.held.has('hold-server-owner'), true)
    assert.equal((await pendingQuestion(owner, secondId))[0].question.id, pending.question.id)

    const memberRun = await send(memberPage, 'member-after-last-owner-holder')
    await enterWaiting(memberRun, 'member-after-last-owner-holder')
    const writing = startCli({ directory: folders[0], environment, prompt: '/write cli-server-owner.txt owner-and-local-cli-share-a-directory' })
    clients.push(writing)
    const written = await cliOutcome(writing)
    noWaiting(written.events)
    observations.runs.push(written.events)
    assert.equal(await readFile(path.join(folders[0], 'cli-server-owner.txt'), 'utf8'), 'owner-and-local-cli-share-a-directory')
    const profile = await cliProfile(directory, model)
    const cli = startCli({ directory: folders[0], environment, profile, prompt: 'hold-owner-cli' })
    clients.push(cli)
    await model.reached('hold-owner-cli')
    const peer = await queue(owner, peerId, 'owner-can-join-own-cli-and-question')
    const peerEvents = await terminal(owner, peerId, peer.run_id)
    noWaiting(peerEvents)
    observations.runs.push(peerEvents)
    assert.equal(model.count('owner-can-join-own-cli-and-question'), 1)
    assert.equal(model.held.has('hold-server-owner'), true)
    assert.equal(model.held.has('hold-owner-cli'), true)

    await firstPage.getByRole('button', { name: '停止运行', exact: true }).click()
    const firstEvents = await terminal(owner, firstId, first.run_id, 'turn_cancelled')
    assertAuthor(firstEvents, server.owner.session.user.user_id)
    observations.runs.push(firstEvents)
    await until(() => model.held.has('hold-server-owner'), value => !value, 'the stopped owner model closes')
    assert.equal((await pendingQuestion(owner, secondId))[0].question.id, pending.question.id)
    await stillBlocked(memberRun, 'member-after-last-owner-holder', 'one-owner-cancelled')
    model.release('hold-owner-cli')
    const cliFinished = await cliOutcome(cli)
    noWaiting(cliFinished.events)
    observations.runs.push(cliFinished.events)
    assert.equal((await pendingQuestion(owner, secondId))[0].question.id, pending.question.id)
    await stillBlocked(memberRun, 'member-after-last-owner-holder', 'cli-finished-question-still-holds')
    await screenshots(memberPage, artifacts, 'question-is-last-holder')
    await question.getByRole('textbox', { name: '输入回答', exact: true }).fill('Release only this task after the other holders have finished.')
    await question.getByRole('button', { name: '提交', exact: true }).click()
    observations.runs.push(await terminal(owner, secondId, asking.run_id))
    const released = await terminal(member, memberId, memberRun.run_id)
    observations.runs.push(released)
    assert.ok(released.some(event => event.type === 'workspace_execution_acquired'))
    assert.equal(model.count('member-after-last-owner-holder'), 1)
    await memberPage.locator('article[data-role="assistant"]').filter({ hasText: 'Completed: member-after-last-owner-holder' }).waitFor()
    await screenshots(memberPage, artifacts, 'other-account-resumed')

    const crashing = startCli({ directory: folders[0], environment, profile, prompt: 'hold-cli-unexpected-exit' })
    clients.push(crashing)
    await model.reached('hold-cli-unexpected-exit')
    const afterExit = await send(memberPage, 'member-after-cli-process-exit')
    await enterWaiting(afterExit, 'member-after-cli-process-exit')
    await stillBlocked(afterExit, 'member-after-cli-process-exit', 'cli-process-still-alive')
    assert.equal(crashing.child.kill('SIGKILL'), true)
    const crash = await crashing.completion
    assert.equal(crash.error?.signal, 'SIGKILL')
    const recovered = await terminal(member, memberId, afterExit.run_id)
    observations.runs.push(recovered)
    assert.ok(recovered.some(event => event.type === 'workspace_execution_acquired'))
    assert.equal(model.count('member-after-cli-process-exit'), 1, 'OS process exit releases its last lease without restarting Node or Server')
    await memberPage.locator('article[data-role="assistant"]').filter({ hasText: 'Completed: member-after-cli-process-exit' }).waitFor()
    await screenshots(memberPage, artifacts, 'cli-exit-recovered')
    assert.deepEqual(model.errors, [])
    assert.deepEqual(observations.errors, [])
  } catch (error) {
    for (const [index, page] of pages.entries()) await page.screenshot({ path: path.join(artifacts, `failure-${index}.png`), animations: 'disabled' }).catch(() => {})
    error.message += '\n' + processes.map(process => process.diagnostics()).join('\n')
    throw error
  } finally {
    for (const client of clients) if (client.child.exitCode === null && client.child.signalCode === null) client.child.kill('SIGKILL')
    await Promise.all(clients.map(client => client.completion))
    await browser?.close()
    for (const process of processes.reverse()) await stopProcess(process)
    await model?.close()
    await writeFile(path.join(artifacts, 'observations.json'), JSON.stringify({ ...observations, calls: model?.calls, modelErrors: model?.errors }, null, 2))
    await writeFile(path.join(artifacts, 'process.log'), processes.map(process => process.diagnostics()).join('\n'))
    if (process.env.TERNILO_E2E_ARTIFACT_DIR) await rm(directory, { recursive: true, force: true })
  }
})
