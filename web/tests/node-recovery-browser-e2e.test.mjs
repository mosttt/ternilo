import assert from 'node:assert/strict'
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises'
import { createConnection, createServer } from 'node:net'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { chromium } from 'playwright'
import { choose, profile, task, upstream } from './account-node-provider-fixture.mjs'
import { localApi, until } from './model-device-fixture.mjs'
import { freePort, initializeServer, repository, serverRequest, startProcess, stopProcess, waitForHttp } from './platform-e2e-fixture.mjs'

async function controlProxy(origin) {
  const peers = new Set(), controls = new Set()
  let blocked = false
  const server = createServer(client => {
    const upstream = createConnection({ host: '127.0.0.1', port: Number(new URL(origin).port) })
    peers.add(client); peers.add(upstream)
    let headers = ''
    const classify = chunk => {
      headers += chunk.toString()
      if (!headers.includes('\r\n\r\n')) return
      client.off('data', classify)
      if (/\r\nupgrade:\s*websocket\r\n/i.test(headers)) {
        controls.add(client)
        if (blocked) client.destroy()
      }
      headers = ''
    }
    client.on('data', classify)
    for (const socket of [client, upstream]) socket.on('error', () => {})
    client.on('close', () => { peers.delete(client); controls.delete(client); upstream.destroy() })
    upstream.on('close', () => { peers.delete(upstream); client.destroy() })
    client.pipe(upstream).pipe(client)
  })
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve))
  return {
    url: `ws://127.0.0.1:${server.address().port}/api/v1/executors/connect`,
    interrupt() { blocked = true; for (const socket of controls) socket.destroy() },
    resume() { blocked = false },
    async close() { for (const socket of peers) socket.destroy(); await new Promise(resolve => server.close(resolve)) },
  }
}

test('Node retains a long model call across control reconnects and never replays interrupted inputs after restart', { timeout: 240_000 }, async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-node-recovery-'))
  const artifacts = process.env.TERNILO_E2E_ARTIFACT_DIR ?? directory
  await mkdir(artifacts, { recursive: true })
  const processes = [], errors = [], results = []
  const environment = { XDG_STATE_HOME: path.join(directory, 'state') }
  let browser, page, server, node, proxy, remote, hold
  try {
    remote = await upstream('recovery-proof', 'recovery-private-key')
    server = await initializeServer({ directory: path.join(directory, 'server'), origin: `http://127.0.0.1:${await freePort()}`, mode: 'single_user', environment })
    processes.push(server)
    const serverOrigin = server.origin, configPath = server.configPath, account = server.owner
    const owner = (resource, options = {}) => serverRequest(serverOrigin, resource, { token: account.session.access_token, tenantId: account.session.personal_tenant_id, ...options })
    await owner('/credentials', { body: { name: 'SAME_PROVIDER_KEY', value: 'recovery-private-key' } })
    await owner('/providers', { body: profile(remote.baseUrl) })
    const enrolled = await owner(`/tenants/${account.session.personal_tenant_id}/my-computer-enrollments`, { body: { name: 'recovery-node', project_id: null, ttl_seconds: 600 } })
    const enrolledComputerId = enrolled.enrollment.executor_id
    const credential = await owner('/enrollments/consume', { body: { token: enrolled.enrollment.token } })
    proxy = await controlProxy(serverOrigin)
    const origin = `http://127.0.0.1:${await freePort()}`
    const binary = process.env.TERNILO_E2E_NODE_BINARY ?? path.join(repository, 'target/debug/ternilo')
    const args = ['serve', '--listen', new URL(origin).host, '--data-dir', path.join(directory, 'node'), '--node-id', enrolledComputerId, '--gateway-url', proxy.url, '--allow-insecure-gateway']
    const startNode = async () => {
      node = startProcess(binary, args, { ...environment, TERNILO_LOCAL_TOKEN: credential.credential.token })
      processes.push(node); await waitForHttp(origin, node)
      return localApi(origin)
    }
    let local = await startNode()
    const folder = path.join(directory, 'workspace'); await mkdir(folder)
    const workspace = await local('/workspaces', { body: { path: folder } })
    const nodeSession = await local('/sessions', { body: { workspace_id: workspace.workspace_id } })
    const state = await until(() => owner('/state'), value => value.sessions.length === 1, 'session discovered')
    const sessionId = state.sessions[0].identity.session_id, nodeSessionId = nodeSession.identity.session_id
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
    const open = async () => {
      page = await browser.newPage({ viewport: { width: 1366, height: 900 }, serviceWorkers: 'block' })
      page.on('pageerror', error => errors.push(error.message))
      page.on('console', message => { if (message.type() === 'error') errors.push(message.text()) })
      page.on('response', response => { if (response.status() >= 400) errors.push(`${response.status()} ${new URL(response.url()).pathname}`) })
      await page.goto(serverOrigin)
      await page.getByLabel('用户名', { exact: true }).fill(account.username)
      await page.getByLabel('密码', { exact: true }).fill(account.password)
      await page.getByRole('button', { name: '登录', exact: true }).click()
      await page.locator(`[data-sidebar-session-row][data-session-id="${sessionId}"]`).click()
    }
    await open()
    await choose(page, 'account')
    const connected = expected => until(() => owner('/model-computers'), values => values.some(value => value.executor_id === enrolledComputerId && value.connected === expected), `computer connection ${expected}`)
    const history = () => local(`/sessions/${nodeSessionId}/events`)
    const ledger = () => owner('/model-access/requests?limit=100')
    const begin = async marker => {
      const previous = new Set((await ledger()).requests.map(request => request.request_id))
      const calls = remote.calls.length
      hold = remote.holdNext(body => body.stream && JSON.stringify(body.messages.findLast(message => message.role === 'user')).includes(marker))
      const accepted = await owner(`/sessions/${sessionId}/queue`, { body: { content: { kind: 'prompt', input: marker } } })
      await page.getByText(marker, { exact: true }).last().waitFor()
      await Promise.race([hold.began, new Promise((_, reject) => { const timeout = setTimeout(() => reject(new Error('model request never reached the upstream')), 15000); timeout.unref() })])
      await page.close()
      return { accepted, previous, calls }
    }
    const settled = async (previous, interrupted) => {
      const fresh = value => value.requests.filter(request => !previous.has(request.request_id))
      const requests = fresh(await until(ledger, value => fresh(value).length > 0 && fresh(value).every(request => request.state !== 'pending'), 'model requests settle'))
      for (const request of requests) {
        assert.equal(request.actor_user_id, account.session.user.user_id)
        assert.equal(request.model_beneficiary_user_id, account.session.user.user_id)
        assert.equal(request.source, 'user_provider')
        if (interrupted) assert.equal(request.accounted_tokens, null)
        else assert.ok(request.accounted_tokens > 0)
      }
      return requests
    }
    const long = await begin('long-response-with-control-reconnects')
    const began = Date.now()
    for (let index = 0; index < 3; index += 1) {
      proxy.interrupt(); await connected(false)
      proxy.resume(); await connected(true)
    }
    await new Promise(resolve => setTimeout(resolve, Math.max(0, 65000 - (Date.now() - began))))
    assert.equal(remote.calls.slice(long.calls).filter(call => call.stream).length, 1)
    assert.equal((await history()).some(event => event.run_id === long.accepted.run_id && ['turn_failed', 'turn_cancelled', 'turn_finished'].includes(event.type)), false)
    hold.release()
    await until(history, events => events.some(event => event.run_id === long.accepted.run_id && event.type === 'turn_finished'), 'long request finishes without retry')
    const completed = await settled(long.previous, false)
    results.push({ case: 'long-response', held_ms: Date.now() - began, reconnects: 3, requests: completed.length })
    await open()
    for (const mode of ['server-restart', 'node-crash']) {
      const running = await begin(mode)
      if (mode === 'server-restart') {
        await stopProcess(server)
        await until(history, events => events.some(event => event.run_id === running.accepted.run_id && ['turn_failed', 'turn_cancelled'].includes(event.type)), 'Server shutdown ends the model call')
        server = startProcess(process.env.TERNILO_E2E_SERVER_BINARY ?? path.join(repository, 'target/debug/ternilo-server'), ['serve', '--config-dir', path.dirname(configPath)], environment)
        processes.push(server); await waitForHttp(`${serverOrigin}/readyz`, server)
      } else {
        const exited = new Promise(resolve => node.child.once('exit', resolve))
        node.child.kill('SIGKILL'); await exited
      }
      await settled(running.previous, true)
      hold.release()
      if (mode === 'node-crash') local = await startNode()
      await connected(true)
      await until(() => local(`/sessions/${nodeSessionId}/queue`), value => !value.active_run_id && !value.items.some(item => item.placement === 'queued'), 'interrupted input is not requeued')
      const events = await history()
      assert.equal(events.filter(event => event.type === 'user_message' && event.run_id === running.accepted.run_id).length, 1)
      assert.equal(remote.calls.slice(running.calls).filter(call => call.stream).length, 1)
      results.push({ case: mode, submitted_once: true, usage: 'unknown' })
      await open()
    }
    await task(page, owner, sessionId, folder, 'recovery-proof')
    await page.locator('[data-role="assistant"]').filter({ hasText: 'recovery-proof finished this task.' }).last().waitFor()
    await page.screenshot({ path: path.join(artifacts, 'node-recovery-desktop.png') })
    await page.setViewportSize({ width: 390, height: 844 })
    await page.locator('[data-app-frame][data-mobile="true"]').waitFor()
    await page.evaluate(async () => {
      await new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve)))
      await Promise.all(document.getAnimations().filter(animation => animation.effect?.getTiming().iterations !== Infinity).map(animation => animation.finished.catch(() => {})))
    })
    const closeSidebar = page.locator('[data-app-sidebar-column]:not([inert]) [data-mobile-sidebar-close]')
    if (await closeSidebar.count()) await closeSidebar.click()
    const latest = page.getByRole('button', { name: '回到底部', exact: true })
    if (await latest.isVisible()) await latest.click()
    await page.locator('[data-role="assistant"]').filter({ hasText: 'recovery-proof finished this task.' }).last().scrollIntoViewIfNeeded()
    assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth))
    await page.screenshot({ path: path.join(artifacts, 'node-recovery-mobile.png') })
    assert.deepEqual(errors, [])
  } catch (error) {
    if (page && !page.isClosed()) await page.screenshot({ path: path.join(artifacts, 'node-recovery-failure.png') }).catch(() => {})
    error.message += `\nBrowser errors: ${JSON.stringify(errors)}`
    throw error
  } finally {
    hold?.release()
    await browser?.close()
    for (const process of processes.reverse()) await stopProcess(process)
    await proxy?.close(); await remote?.close()
    await writeFile(path.join(artifacts, 'node-recovery-processes.log'), processes.map(process => process.diagnostics()).join('\n'))
    await writeFile(path.join(artifacts, 'node-recovery-results.json'), JSON.stringify(results, null, 2))
    if (directory !== artifacts) await rm(directory, { recursive: true, force: true })
  }
})
