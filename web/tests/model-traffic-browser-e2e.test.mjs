import assert from 'node:assert/strict'
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { chromium } from 'playwright'
import { freePort, initializeServer, repository, serverRequest, startProcess, stopProcess, waitForHttp } from './platform-e2e-fixture.mjs'
import { modelFixture, seedModels, until } from './model-device-fixture.mjs'
import { controlledUpstream, evidenceRecorder, login, verifyEmbeddedAssets } from './model-device-limits-fixture.mjs'

const unlimited = { requests_per_minute: null, max_concurrent_requests: null }
test('model traffic policies control shared Server admission and editable account overrides', { timeout: 180000 }, async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-model-traffic-'))
  const artifacts = path.join(process.env.TERNILO_E2E_ARTIFACT_DIR ?? directory, 'model-traffic')
  await mkdir(artifacts, { recursive: true })
  const evidence = evidenceRecorder(artifacts), processes = [], upstreams = []
  let browser, page, held
  try {
    const origin = `http://127.0.0.1:${await freePort()}`
    const application = await initializeServer({ directory: path.join(directory, 'server'), origin,
      databaseUrl: process.env.TERNILO_E2E_DATABASE_URL, migrationDatabaseUrl: process.env.TERNILO_E2E_MIGRATION_DATABASE_URL })
    processes.push(application)
    let peerOrigin = origin
    if (process.env.TERNILO_E2E_CLUSTER === '1') {
      peerOrigin = `http://127.0.0.1:${await freePort()}`
      const config = JSON.parse(await readFile(application.configPath, 'utf8'))
      const folder = path.join(directory, 'peer'); await mkdir(folder)
      await writeFile(path.join(folder, 'config.json'), JSON.stringify({ ...config, listen: new URL(peerOrigin).host }), { mode: 0o600 })
      const peer = startProcess(process.env.TERNILO_E2E_SERVER_BINARY ?? path.join(repository, 'target/debug/ternilo-server'), ['serve', '--config-dir', folder])
      processes.push(peer); await waitForHttp(`${peerOrigin}/readyz`, peer)
    }
    const token = application.owner.session.access_token, userId = application.owner.session.user.user_id
    const owner = (resource, options = {}) => serverRequest(origin, resource, { token, ...options })
    const source = await modelFixture(); upstreams.push(source)
    const upstream = await controlledUpstream(source); upstreams.push(upstream)
    const seeded = await seedModels(application, upstream)
    const key = await owner('/model-access/keys', { body: { name: 'Traffic browser key', grant_id: seeded.first.grant_id, model_ids: ['account-model'] } })
    const request = async (name, status = 200, target = origin, stream = false) => {
      const response = await fetch(`${target}/v1/chat/completions`, { method: 'POST', headers: { authorization: `Bearer ${key.token}`, 'content-type': 'application/json', 'idempotency-key': name },
        body: JSON.stringify({ model: 'account-model', messages: [{ role: 'user', content: name }], stream }), signal: AbortSignal.timeout(20000) })
      assert.equal(response.status, status)
      if (status === 429) {
        assert.match(response.headers.get('retry-after'), /^\d+$/)
        assert.ok(Number(response.headers.get('retry-after')) >= 1 && Number(response.headers.get('retry-after')) <= 60)
        const value = await response.json(); assert.equal(value.error.code, 'rate_limited')
        evidence.check('HTTP 429 includes retry-after', { message: value.error.message })
        return value
      }
      return stream ? response : response.json()
    }
    const save = async (platform = unlimited, accountDefault = unlimited) => {
      const current = await owner('/admin/models/traffic')
      return owner('/admin/models/traffic', { method: 'PUT', body: { revision: current.revision, policy: { platform, account_default: accountDefault } } })
    }
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
    page = await browser.newPage({ locale: 'zh-CN', viewport: { width: 1440, height: 1000 }, serviceWorkers: 'block' })
    evidence.observe(page, 'owner')
    await page.goto(`${origin}/admin/models`); await login(page, application.owner)
    await page.getByRole('tab', { name: '请求限流', exact: true }).click()
    const panel = page.locator('[data-model-traffic-admin]')
    await panel.getByRole('group', { name: '全站限制', exact: true }).getByLabel('最大并发请求数', { exact: true }).fill('1')
    await panel.locator('form').first().getByRole('button', { name: '保存', exact: true }).click()
    await panel.getByText('请求限制已保存。', { exact: true }).waitFor()
    assert.equal((await owner('/admin/models/traffic')).policy.platform.max_concurrent_requests, 1)
    evidence.check('browser saves platform concurrency')

    evidence.action('two Servers compete for one platform slot')
    held = upstream.control('held-traffic')
    const delivery = await request('held-traffic', 200, origin, true)
    await held.began
    const reading = delivery.text()
    await request('concurrent-traffic', 429, peerOrigin)
    assert.equal(upstream.calls.length, 1, 'denied requests do not reach the upstream')
    held.release(); held = undefined
    assert.match(await reading, /\[DONE\]/)
    await until(() => owner('/model-access/traffic'), value => value.active_requests === 0, 'completed call releases concurrency')
    await save(unlimited, { ...unlimited, requests_per_minute: 1 })
    await request('account-window', 429, peerOrigin)
    await save()
    await request('after-clear', 200, peerOrigin)
    assert.equal((await owner('/model-access/traffic')).recent_requests, 2, 'editing policy preserves accepted history')
    await save({ ...unlimited, requests_per_minute: 1 })
    await request('platform-window', 429, peerOrigin)
    await save()
    evidence.check('platform and account rolling windows preserve accepted requests after edits')

    evidence.action('stale UI cannot overwrite another administrator')
    evidence.expectBrowserError('owner', 'PUT', '/api/v1/admin/models/traffic', 409, /changed; reload before saving/)
    await panel.locator('form').first().getByRole('button', { name: '保存', exact: true }).click()
    await panel.getByRole('alert').filter({ hasText: /reload before saving/ }).waitFor()
    const reloaded = page.waitForResponse(response => response.request().method() === 'GET' && new URL(response.url()).pathname === '/api/v1/admin/models/traffic')
    await panel.locator('form').first().getByRole('button', { name: '刷新', exact: true }).click()
    assert.equal((await reloaded).status(), 200)
    await panel.getByRole('group', { name: '每个账号的默认限制', exact: true }).getByLabel('每分钟请求数', { exact: true }).fill('1')
    await panel.locator('form').first().getByRole('button', { name: '保存', exact: true }).click()
    await panel.getByText('请求限制已保存。', { exact: true }).waitFor()
    assert.equal((await owner('/admin/models/traffic')).policy.account_default.requests_per_minute, 1)
    await panel.getByRole('button', { name: application.owner.username, exact: true }).click()
    const editor = panel.locator('[data-account-traffic-editor]')
    await editor.getByLabel('使用账号默认限制', { exact: true }).uncheck()
    await editor.getByLabel('每分钟请求数', { exact: true }).fill('')
    await editor.getByRole('button', { name: '保存', exact: true }).click()
    await editor.getByText('请求限制已保存。', { exact: true }).waitFor()
    await until(() => owner('/model-access/traffic'), value => value.limits?.requests_per_minute === null, 'explicit unlimited override')
    await request('explicit-unlimited', 200, peerOrigin)
    await editor.getByLabel('使用账号默认限制', { exact: true }).check()
    await editor.getByRole('button', { name: '保存', exact: true }).click()
    await until(() => owner('/model-access/traffic'), value => value.limits === null && value.effective.requests_per_minute === 1, 'inherit restores default')
    await request('inherited-window', 429, peerOrigin)
    for (const width of [1440, 390, 320]) {
      await page.setViewportSize({ width, height: 1000 })
      assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), `no overflow at ${width}`)
      await evidence.capture(page, `traffic-admin-${width}`, editor)
    }
    evidence.check('account override and inheritance work through the real browser', { userId })

    evidence.action('self-service observation is lazy')
    await page.goto(`${origin}/models?tab=access`)
    const own = page.locator('[data-own-model-traffic]'); await own.waitFor()
    assert.equal(evidence.result.network.filter(row => row.action === 'self-service observation is lazy' && row.pathname === '/api/v1/model-access/traffic').length, 0)
    await own.locator('summary').click()
    await own.getByText(/近一分钟已接受 3 个请求/).waitFor()
    await evidence.capture(page, 'traffic-own-320', own)
    await verifyEmbeddedAssets([origin], evidence)
    await evidence.clean(); evidence.result.status = 'passed'
  } catch (error) {
    evidence.result.status = 'failed'; evidence.result.failure = error.message
    if (page) await evidence.capture(page, 'failure').catch(() => {})
    throw error
  } finally {
    held?.release(); evidence.beginCleanup(); await browser?.close()
    for (const process of processes.reverse()) await stopProcess(process)
    for (const source of upstreams.reverse()) await source.close()
    await writeFile(path.join(artifacts, 'processes.log'), processes.map(process => process.diagnostics()).join('\n'))
    await evidence.save(); await rm(directory, { recursive: true, force: true })
  }
})
