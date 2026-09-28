import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { chromium } from 'playwright'
import { freePort, initializeServer, repository, serverRequest, startProcess, stopProcess, waitForHttp } from './platform-e2e-fixture.mjs'
import { localApi, until } from './model-device-fixture.mjs'
import { history } from './history-loading-fixture.mjs'

const binary = process.env.TERNILO_E2E_NODE_BINARY ?? path.join(repository, 'target/debug/ternilo')

test('Local and Server page a long journal, resume a suffix and retain bounded offline history', { timeout: 300_000 }, async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-relay-history-'))
  const artifacts = process.env.TERNILO_E2E_ARTIFACT_DIR ?? directory
  await mkdir(artifacts, { recursive: true })
  const processes = [], errors = []
  const environment = { XDG_STATE_HOME: path.join(directory, 'state') }
  let browser, page, node
  try {
    const server = await initializeServer({ directory: path.join(directory, 'server'), origin: `http://127.0.0.1:${await freePort()}`, mode: 'single_user', environment })
    processes.push(server)
    const tenantId = server.owner.session.personal_tenant_id
    const owner = (resource, options = {}) => serverRequest(server.origin, resource, { token: server.owner.session.access_token, tenantId, ...options })
    const enrolled = await owner(`/tenants/${tenantId}/my-computer-enrollments`, { body: { executor_id: 'history-node', project_id: null, ttl_seconds: 600 } })
    const credential = await owner('/enrollments/consume', { body: { token: enrolled.enrollment.token } })
    const origin = `http://127.0.0.1:${await freePort()}`, data = path.join(directory, 'data')
    const args = ['serve', '--listen', new URL(origin).host, '--data-dir', data, '--node-id', 'history-node', '--gateway-url', `${server.origin.replace('http:', 'ws:')}/api/v1/executors/connect`, '--allow-insecure-gateway']
    const env = { ...environment, TERNILO_LOCAL_TOKEN: credential.credential.token }
    node = startProcess(binary, args, env); processes.push(node); await waitForHttp(origin, node)
    let local = await localApi(origin)
    const folder = path.join(directory, 'workspace'); await mkdir(folder)
    const workspace = await local('/workspaces', { body: { path: folder } })
    const session = await local('/sessions', { body: { workspace_id: workspace.workspace_id } })
    const sessionId = session.identity.session_id
    const state = await until(() => owner('/state'), state => state.sessions.length === 1, 'mapped session')
    const serverId = state.sessions[0].identity.session_id
    await stopProcess(node)
    const expected = history()
    const file = path.join(data, 'sessions', `${Buffer.from(sessionId).toString('hex')}.jsonl`)
    await mkdir(path.dirname(file), { recursive: true })
    await writeFile(file, expected.map(event => JSON.stringify(event)).join('\n') + '\n')
    node = startProcess(binary, args, env); processes.push(node); await waitForHttp(origin, node)
    local = await localApi(origin)
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
    const context = await browser.newContext({ viewport: { width: 1280, height: 900 }, hasTouch: true, serviceWorkers: 'block' })
    page = await context.newPage()
    page.on('pageerror', error => errors.push(error.message))
    page.on('console', message => { if (message.type() === 'error') errors.push(message.text()) })
    page.on('response', response => { if (response.status() >= 400) errors.push(`HTTP ${response.status()} ${new URL(response.url()).pathname}`) })
    const measurements = {}
    for (const [name, target] of [['local', origin], ['server', server.origin]]) {
      await page.goto(target)
      if (name === 'server') {
        await page.getByLabel('用户名', { exact: true }).fill(server.owner.username)
        await page.getByLabel('密码', { exact: true }).fill(server.owner.password)
        await page.getByRole('button', { name: '登录', exact: true }).click()
      }
      await page.getByText('History preserved.', { exact: true }).waitFor({ timeout: 120_000 })
      for (const asset of ['app.js', 'app.css']) {
        const served = await (await fetch(`${target}/assets/${asset}`)).arrayBuffer()
        const built = await readFile(path.join(process.env.TERNILO_E2E_ASSET_DIR ?? path.join(repository, 'web/dist/assets'), asset))
        assert.equal(createHash('sha256').update(Buffer.from(served)).digest('hex'), createHash('sha256').update(built).digest('hex'))
      }
      measurements[name] = []
      for (let index = 0; index < 3; index++) {
        const start = Date.now()
        await page.reload()
        await page.getByText('History preserved.', { exact: true }).waitFor({ timeout: 120_000 })
        measurements[name].push(Date.now() - start)
      }
      const events = name === 'local' ? await local(`/sessions/${sessionId}/events`) : await owner(`/sessions/${serverId}/events`)
      assert.equal(events.length, expected.length)
      assert.deepEqual(events.map(event => event.seq), expected.map(event => event.seq))
      const request = name === 'local' ? local : owner
      const id = name === 'local' ? sessionId : serverId
      const latest = await request(`/sessions/${id}/history?limit=200`)
      assert.deepEqual(latest.events.map(event => event.seq), expected.slice(-200).map(event => event.seq))
      const older = await request(`/sessions/${id}/history?limit=200&before_seq=${latest.next_before_seq}`)
      assert.deepEqual(older.events.map(event => event.seq), expected.slice(-400, -200).map(event => event.seq))
    }
    console.log(JSON.stringify({ events: expected.length, reload_ms: measurements }))
    await page.setViewportSize({ width: 390, height: 844 })
    await page.reload()
    await page.getByText('History preserved.', { exact: true }).waitFor({ timeout: 120_000 })
    assert.equal(await page.locator('[data-turn-process]').count(), 0, 'partial history stays expanded')
    await page.locator('[data-reasoning-row]').getByRole('button').tap()
    assert.equal(await page.locator('[data-reasoning-body]').textContent(), expected.slice(-200).filter(event => event.type === 'assistant_reasoning_delta').map(event => event.delta).join(''))
    await page.screenshot({ path: path.join(artifacts, 'server-history-mobile.png') })
    assert.deepEqual(errors, [])
    await page.close()
    await stopProcess(node)
    const offline = await owner(`/sessions/${serverId}/events`)
    assert.equal(offline.length, expected.length)
    const offlinePage = await owner(`/sessions/${serverId}/history?limit=200`)
    assert.deepEqual(offlinePage.events.map(event => event.seq), expected.slice(-200).map(event => event.seq))
    assert.deepEqual(errors, [])
    await writeFile(path.join(artifacts, 'history-timings.json'), JSON.stringify(measurements, null, 2))
  } catch (error) {
    await page?.screenshot({ path: path.join(artifacts, 'history-relay-failure.png') }).catch(() => {})
    error.message += `\n${processes.map(item => item.diagnostics()).join('\n')}`
    throw error
  } finally {
    await browser?.close()
    for (const process of processes.reverse()) await stopProcess(process)
    await rm(directory, { recursive: true, force: true })
  }
})
