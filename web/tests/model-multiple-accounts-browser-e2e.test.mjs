import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { mkdir, mkdtemp, readFile, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { chromium } from 'playwright'
import { freePort, initializeServer, repository, startProcess, stopProcess, waitForHttp } from './platform-e2e-fixture.mjs'
import { approveConnection, localApi, modelFixture, seedModels, until } from './model-device-fixture.mjs'
import { addMember, closeModels, expectOnlySelectedUsage, refreshConnection, runTask, verifySources } from './model-multiple-accounts-fixture.mjs'

const binary = process.env.TERNILO_E2E_NODE_BINARY ?? path.join(repository, 'target/debug/ternilo')
const serverBinary = process.env.TERNILO_E2E_SERVER_BINARY ?? path.join(repository, 'target/debug/ternilo-server')

test('multiple Servers and accounts retain exact model and budget selection through offline refresh and restart', { timeout: 240_000 }, async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-model-accounts-'))
  const artifacts = process.env.TERNILO_E2E_ARTIFACT_DIR ?? directory
  await mkdir(artifacts, { recursive: true })
  const processes = [], upstreams = [], errors = [], expectedFailures = []
  const environment = { XDG_STATE_HOME: path.join(directory, 'state') }
  let browser, page, approval, app
  try {
    const upstreamA = await modelFixture('server-a'); upstreams.push(upstreamA)
    const upstreamB = await modelFixture('server-b'); upstreams.push(upstreamB)
    const a = await initializeServer({ directory: path.join(directory, 'a'), origin: `http://127.0.0.1:${await freePort()}`, environment })
    processes.push(a)
    const b = await initializeServer({ directory: path.join(directory, 'b'), origin: `http://127.0.0.1:${await freePort()}`, environment })
    processes.push(b)
    const modelsA = await seedModels(a, upstreamA), modelsB = await seedModels(b, upstreamB)
    const member = await addMember(a, modelsA)
    const origin = `http://127.0.0.1:${await freePort()}`
    const args = ['serve', '--listen', new URL(origin).host, '--data-dir', path.join(directory, 'local')]
    app = startProcess(binary, args, environment); processes.push(app)
    await waitForHttp(origin, app)
    let api = await localApi(origin)
    const folder = path.join(directory, 'workspace'); await mkdir(folder)
    const workspace = await api('/workspaces', { body: { path: folder } })
    const session = await api('/sessions', { body: { workspace_id: workspace.workspace_id } })
    const sessionId = session.identity.session_id
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
    page = await browser.newPage({ viewport: { width: 1280, height: 900 }, hasTouch: true, serviceWorkers: 'block' })
    approval = await browser.newPage({ viewport: { width: 320, height: 844 }, hasTouch: true, serviceWorkers: 'block' })
    for (const target of [page, approval]) {
      target.on('pageerror', error => errors.push(error.message))
      target.on('console', message => { if (message.type() === 'error' && !/^Failed to load resource: the server responded with a status of 500/.test(message.text())) errors.push(message.text()) })
      target.on('response', response => { if (response.status() >= 400) { const key = `${response.status()} ${new URL(response.url()).pathname}`; const index = expectedFailures.indexOf(key); if (index < 0) errors.push(key); else expectedFailures.splice(index, 1) } })
    }
    await page.goto(origin)
    for (const target of [origin, a.origin, b.origin]) {
      const served = await (await fetch(`${target}/assets/app.js`)).arrayBuffer()
      const built = await readFile(path.join(repository, 'web/dist/assets/app.js'))
      assert.equal(createHash('sha256').update(Buffer.from(served)).digest('hex'), createHash('sha256').update(built).digest('hex'))
    }
    await approveConnection(page, approval, a, 'My platform')
    await approveConnection(page, approval, { ...a, owner: member.owner }, 'My platform')
    await approveConnection(page, approval, b, 'My platform')
    const connections = await api('/model-connections')
    assert.equal(connections.length, 3)
    const providers = await api('/providers')
    const source = (server, user, grant) => {
      const connection = connections.find(entry => entry.server_url === server.origin && entry.session.identity.user_id === user.user_id)
      assert.ok(connection)
      const provider = providers.find(entry => entry.base_url === `${server.origin}/v1/device/${grant}`)
      assert.ok(provider)
      return { connection, provider, origin: server.origin, username: user.username }
    }
    const ownerA = source(a, a.owner.session.user, modelsA.first.grant_id)
    const memberA = source(a, member.owner.session.user, member.grant.grant_id)
    const ownerB = source(b, b.owner.session.user, modelsB.first.grant_id)
    assert.equal(new Set([ownerA, memberA, ownerB].map(source => source.provider.id)).size, 3)
    for (const [selected, text] of [[ownerA, 'server-a'], [memberA, 'server-a'], [ownerB, 'server-b']]) {
      await runTask(page, api, sessionId, selected.provider, `Execute using ${selected.origin} and ${selected.username}`)
      assert.equal(await readFile(path.join(folder, 'model-device-proof.txt'), 'utf8'), text, JSON.stringify({ selected: selected.provider.id, saved: (await api('/state')).sessions.find(item => item.identity.session_id === sessionId).model, upstream: [upstreamA.calls.length, upstreamB.calls.length], tools: (await api(`/sessions/${sessionId}/events`)).filter(event => ['tool_call_started', 'tool_call_finished'].includes(event.type)) }))
    }
    await expectOnlySelectedUsage(modelsA.request, a.owner.session.user.user_id, modelsA.first.grant_id, ownerA.connection.session.identity.device_id)
    await expectOnlySelectedUsage(member.request, member.owner.session.user.user_id, member.grant.grant_id, memberA.connection.session.identity.device_id)
    await expectOnlySelectedUsage(modelsB.request, b.owner.session.user.user_id, modelsB.first.grant_id, ownerB.connection.session.identity.device_id)
    await page.setViewportSize({ width: 390, height: 844 })
    await verifySources(page, [ownerA, memberA, ownerB], path.join(artifacts, 'multiple-model-sources-mobile.png'))
    await page.setViewportSize({ width: 1280, height: 900 })
    await stopProcess(b)
    expectedFailures.push(`500 /api/v1/model-connections/${ownerB.connection.connection_id}/refresh`)
    await refreshConnection(page, ownerB.connection.connection_id)
    await page.locator('[data-model-connections]').getByRole('alert').waitFor()
    assert.equal((await api('/providers')).length, providers.length, 'an offline catalog remains saved')
    await closeModels(page)
    const otherCalls = upstreamA.calls.length
    const previousEvents = await api(`/sessions/${sessionId}/events`)
    await page.getByRole('textbox', { name: '输入任务', exact: true }).fill('Keep this task on the selected offline Server')
    await page.getByRole('button', { name: '发送', exact: true }).click()
    await until(() => api(`/sessions/${sessionId}/events`), events => events.slice(previousEvents.length).some(event => event.type === 'turn_failed'), 'offline source fails explicitly')
    assert.equal(upstreamA.calls.length, otherCalls, 'another online Server or account is never used as fallback')
    assert.equal(await readFile(path.join(folder, 'model-device-proof.txt'), 'utf8'), 'server-b')
    await stopProcess(app)
    app = startProcess(binary, args, environment); processes.push(app)
    await waitForHttp(origin, app); api = await localApi(origin)
    await page.reload()
    assert.equal((await api('/state')).sessions.find(item => item.identity.session_id === sessionId).model.provider_id, ownerB.provider.id)
    assert.equal((await api('/model-connections')).length, 3)
    assert.equal((await api('/providers')).length, providers.length)
    const restarted = startProcess(serverBinary, ['serve', '--config-dir', path.dirname(b.configPath)], environment); processes.push(restarted)
    await waitForHttp(`${b.origin}/readyz`, restarted)
    await refreshConnection(page, ownerB.connection.connection_id)
    await page.getByText('已刷新可用模型。', { exact: true }).waitFor()
    await closeModels(page)
    await runTask(page, api, sessionId, ownerB.provider, 'Continue on the original Server after reconnecting')
    const stored = await api('/model-connections')
    assert.deepEqual(stored.map(item => item.connection_id).sort(), connections.map(item => item.connection_id).sort())
    assert.equal(await readFile(path.join(folder, 'model-device-proof.txt'), 'utf8'), 'server-b')
    assert.deepEqual(expectedFailures, [])
    assert.deepEqual(errors, [])
  } catch (error) {
    await page?.screenshot({ path: path.join(artifacts, 'multiple-model-sources-failure.png') }).catch(() => {})
    await approval?.screenshot({ path: path.join(artifacts, 'multiple-model-approval-failure.png') }).catch(() => {})
    error.message += `\n${processes.map(item => item.diagnostics()).join('\n')}`
    throw error
  } finally {
    await browser?.close()
    for (const process of processes.reverse()) await stopProcess(process)
    for (const upstream of upstreams) await upstream.close()
    await rm(directory, { recursive: true, force: true })
  }
})
