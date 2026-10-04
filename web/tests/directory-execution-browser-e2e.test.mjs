import assert from 'node:assert/strict'
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { chromium } from 'playwright'
import { freePort, startProcess, stopProcess, waitForHttp } from './platform-e2e-fixture.mjs'
import { localApi, until } from './model-device-fixture.mjs'
import {
  cliOutcome, cliProfile, configureModel, directoryModel, history, isolatedEnvironment, modelSelection,
  nodeBinary, noWaiting, observe, openSession, pendingQuestion, queue, screenshots, send,
  startCli, taskInput, terminal, verifyAssets, waiting,
} from './user-directory-concurrency-fixture.mjs'

test('local same-user sessions and CLI run concurrently while model calls and questions retain directory holders', { timeout: 150_000 }, async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-directory-browser-'))
  const artifacts = path.join(process.env.TERNILO_E2E_ARTIFACT_DIR ?? directory, 'local')
  await mkdir(artifacts, { recursive: true })
  const observations = { errors: [], console: [], network: [], runs: [], assetHashes: {} }, clients = []
  let local, browser, left, right, model
  try {
    const environment = await isolatedEnvironment(directory)
    model = await directoryModel()
    const origin = 'http://127.0.0.1:' + await freePort()
    local = startProcess(nodeBinary, ['serve', '--listen', new URL(origin).host, '--data-dir', path.join(directory, 'data')], environment)
    await waitForHttp(origin, local)
    observations.assetHashes = await verifyAssets(origin)
    const request = await localApi(origin)
    await configureModel(request, model)
    const workspaces = []
    for (const name of ['shared', 'independent']) {
      const folder = path.join(directory, name)
      await mkdir(folder)
      workspaces.push(await request('/workspaces', { body: { path: folder } }))
    }
    const sessions = []
    for (const [index, title] of ['First holder', 'Second holder', 'Same-directory peer', 'Independent'].entries()) {
      const session = await request('/sessions', { body: { workspace_id: workspaces[index === 3 ? 1 : 0].workspace_id } })
      const sessionId = session.identity.session_id
      await request(`/sessions/${sessionId}`, { method: 'PATCH', body: { title, model: modelSelection } })
      sessions.push(sessionId)
    }
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
    const context = await browser.newContext({ locale: 'zh-CN', viewport: { width: 1280, height: 900 }, hasTouch: true, serviceWorkers: 'block' })
    left = await context.newPage()
    right = await context.newPage()
    observe(left, 'first-holder', observations)
    observe(right, 'second-holder', observations)
    await openSession(left, origin, sessions[0])
    await openSession(right, origin, sessions[1])
    const first = await send(left, 'hold-local-first')
    await model.reached('hold-local-first')
    const second = await send(right, 'hold-local-second')
    await model.reached('hold-local-second')
    assert.equal(model.held.size, 2, 'two ordinary sessions reach the actual model before either holder finishes')
    noWaiting(await history(request, sessions[0]))
    noWaiting(await history(request, sessions[1]))
    await taskInput(left).fill('Private draft in the first session')
    await taskInput(right).fill('Private draft in the second session')
    assert.equal(await taskInput(left).inputValue(), 'Private draft in the first session')
    await waiting(right).waitFor({ state: 'hidden' })
    await screenshots(right, artifacts, 'same-user-models-running')

    const independent = await queue(request, sessions[3], 'independent-directory-proof')
    observations.runs.push(await terminal(request, sessions[3], independent.run_id))
    assert.equal(model.count('independent-directory-proof'), 1)
    assert.equal(model.held.size, 2)
    await right.getByRole('button', { name: '停止运行', exact: true }).click()
    observations.runs.push(await terminal(request, sessions[1], second.run_id, 'turn_cancelled'))
    await until(() => model.held.has('hold-local-second'), value => !value, 'cancelled model request closes')
    assert.equal(model.held.has('hold-local-first'), true, 'cancelling one session does not cancel another same-user holder')
    assert.equal(await taskInput(right).inputValue(), 'Private draft in the second session')
    assert.equal(await taskInput(left).inputValue(), 'Private draft in the first session')

    const resumed = await send(right, 'after-cancel-same-directory-proof')
    const resumedEvents = await terminal(request, sessions[1], resumed.run_id)
    noWaiting(resumedEvents)
    observations.runs.push(resumedEvents)
    assert.equal(model.count('after-cancel-same-directory-proof'), 1)
    assert.equal(model.held.has('hold-local-first'), true)
    const asking = await send(right, '/ask Keep this directory task open?')
    const questions = await pendingQuestion(request, sessions[1])
    const question = right.locator('[data-question-takeover]')
    await question.getByText('Keep this directory task open?', { exact: true }).waitFor()
    await screenshots(right, artifacts, 'same-user-pending-question')
    noWaiting((await history(request, sessions[1])).filter(event => event.run_id === asking.run_id))

    const folder = path.join(directory, 'shared')
    const writing = startCli({ directory: folder, environment, prompt: '/write cli-parallel.txt local-cli-ran-with-both-holders' })
    clients.push(writing)
    const written = await cliOutcome(writing)
    noWaiting(written.events)
    observations.runs.push(written.events)
    assert.equal(await readFile(path.join(folder, 'cli-parallel.txt'), 'utf8'), 'local-cli-ran-with-both-holders')
    assert.equal(model.held.has('hold-local-first'), true)
    assert.equal((await pendingQuestion(request, sessions[1]))[0].question.id, questions[0].question.id)

    const cli = startCli({ directory: folder, environment, profile: await cliProfile(directory, model), prompt: 'hold-local-cli' })
    clients.push(cli)
    await model.reached('hold-local-cli')
    const peer = await queue(request, sessions[2], 'browser-runs-while-cli-holds')
    const peerEvents = await terminal(request, sessions[2], peer.run_id)
    noWaiting(peerEvents)
    observations.runs.push(peerEvents)
    assert.equal(model.count('browser-runs-while-cli-holds'), 1)
    assert.equal(model.held.has('hold-local-cli'), true)
    assert.equal(model.held.has('hold-local-first'), true)
    assert.equal((await pendingQuestion(request, sessions[1]))[0].question.id, questions[0].question.id)
    model.release('hold-local-cli')
    const cliFinished = await cliOutcome(cli)
    assert.match(cliFinished.answer, /hold-local-cli/)
    noWaiting(cliFinished.events)
    observations.runs.push(cliFinished.events)

    await left.getByRole('button', { name: '停止运行', exact: true }).click()
    observations.runs.push(await terminal(request, sessions[0], first.run_id, 'turn_cancelled'))
    assert.equal((await pendingQuestion(request, sessions[1]))[0].question.id, questions[0].question.id)
    await question.getByRole('textbox', { name: '输入回答', exact: true }).fill('The independent session and CLI have both completed.')
    await question.getByRole('button', { name: '提交', exact: true }).click()
    observations.runs.push(await terminal(request, sessions[1], asking.run_id))
    await question.waitFor({ state: 'detached' })
    await screenshots(right, artifacts, 'same-user-question-completed')
    assert.deepEqual(model.errors, [])
    assert.deepEqual(observations.errors, [])
  } catch (error) {
    await right?.screenshot({ path: path.join(artifacts, 'failure.png'), animations: 'disabled' }).catch(() => {})
    error.message += '\n' + (local?.diagnostics() ?? '')
    throw error
  } finally {
    for (const client of clients) if (client.child.exitCode === null && client.child.signalCode === null) client.child.kill('SIGKILL')
    await Promise.all(clients.map(client => client.completion))
    await browser?.close()
    await stopProcess(local)
    await model?.close()
    await writeFile(path.join(artifacts, 'observations.json'), JSON.stringify({ ...observations, calls: model?.calls, modelErrors: model?.errors }, null, 2))
    await writeFile(path.join(artifacts, 'process.log'), local?.diagnostics() ?? '')
    if (process.env.TERNILO_E2E_ARTIFACT_DIR) await rm(directory, { recursive: true, force: true })
  }
})
