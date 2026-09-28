import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { readFile, writeFile } from 'node:fs/promises'
import { createServer } from 'node:http'
import path from 'node:path'
import { repository } from './platform-e2e-fixture.mjs'
import { until } from './model-device-fixture.mjs'

export const unlimited = { monthly_tokens: null, max_concurrent_requests: null, expires_at_ms: null }

export async function deadline(promise, label, milliseconds = 15_000) {
  let timer
  try {
    return await Promise.race([promise, new Promise((resolve, reject) => {
      timer = setTimeout(() => reject(new Error(`Timed out: ${label}`)), milliseconds)
    })])
  } finally { clearTimeout(timer) }
}

function safeValue(value) {
  if (Array.isArray(value)) return value.map(safeValue)
  if (value && typeof value === 'object') return Object.fromEntries(Object.entries(value).map(([key, entry]) => [key,
    /^(?:token|access_token|refresh_token|device_code|password|apiToken|api_key)$/i.test(key) ? '[redacted]' : safeValue(entry),
  ]))
  return value
}

export function evidenceRecorder(artifacts) {
  const result = { status: 'preparing', checks: [], screenshots: [], assets: [], http: [], network: [], console: [], errors: [], requestFailures: [], expectedErrors: [], processes: [] }
  const tasks = []
  const requestRecords = new Map()
  const supersededReads = []
  let action = 'setup'
  let closing = false
  const recorder = {
    result,
    action(value) { action = value },
    supersededRead(page, pathname) { supersededReads.push({ page, pathname }) },
    check(name, detail = {}) { result.checks.push({ name, passed: true, ...safeValue(detail) }) },
    expectBrowserError(page, method, pathname, status, pattern) {
      const expected = { page, method, pathname, status, action, pattern: pattern.source, matched: 0, from: Date.now() }
      result.expectedErrors.push(expected)
      return expected
    },
    observe(page, name) {
      page.on('pageerror', error => result.errors.push({ page: name, action, message: error.message }))
      page.on('console', message => result.console.push({ page: name, action, at: Date.now(), type: message.type(), text: message.text(), location: message.location(), cleanup: closing }))
      page.on('request', request => {
        const url = new URL(request.url())
        const entry = { page: name, action, at: Date.now(), method: request.method(), url: request.url(), pathname: url.pathname, cleanup: closing }
        requestRecords.set(request, entry)
        result.network.push(entry)
      })
      page.on('requestfailed', request => result.requestFailures.push({ ...requestRecords.get(request), failure: request.failure(), cleanup: closing }))
      page.on('response', response => {
        tasks.push((async () => {
          const entry = requestRecords.get(response.request())
          if (!entry) throw new Error(`Unobserved browser response: ${response.url()}`)
          entry.status = response.status()
          if (entry.status >= 400) {
            entry.body = await response.text()
            const expected = result.expectedErrors.find(candidate => candidate.matched === 0
              && candidate.page === entry.page && candidate.action === entry.action
              && candidate.method === entry.method && candidate.pathname === entry.pathname
              && candidate.status === entry.status && entry.at >= candidate.from
              && new RegExp(candidate.pattern).test(entry.body))
            if (expected) { expected.matched++; entry.expected = true }
            else result.errors.push({ ...entry, message: 'Unexpected browser HTTP failure' })
          }
          if (/^\/assets\/app\.(?:js|css)$/.test(entry.pathname) && entry.status === 200) {
            const actual = createHash('sha256').update(await response.body()).digest('hex')
            const built = createHash('sha256').update(await readFile(path.join(repository, 'web/dist', entry.pathname))).digest('hex')
            assert.equal(actual, built, `Browser received current embedded ${entry.pathname}`)
            result.assets.push({ page: name, url: response.url(), sha256: actual, browser: true })
          }
        })().catch(error => result.errors.push({ page: name, message: error.message })))
      })
    },
    async capture(page, name, locator) {
      const filename = path.join(artifacts, `${name}.png`)
      if (locator) await locator.scrollIntoViewIfNeeded()
      await page.screenshot({ path: filename, fullPage: true, animations: 'disabled' })
      result.screenshots.push(filename)
    },
    async http(origin, resource, { token, method = 'GET', body, tenantId, status = 200, error, stream = false, label = action } = {}) {
      const started = Date.now()
      const response = await fetch(`${origin}${resource}`, {
        method,
        headers: { ...(token ? { authorization: `Bearer ${token}` } : {}), ...(tenantId ? { 'x-ternilo-tenant': tenantId } : {}), ...(body === undefined ? {} : { 'content-type': 'application/json' }) },
        ...(body === undefined ? {} : { body: JSON.stringify(body) }),
        signal: AbortSignal.timeout(stream ? 90_000 : 20_000),
      })
      const entry = { action: label, method, resource, status: response.status, expectedStatus: status, at: started, responseAt: Date.now(), requestId: response.headers.get('x-ternilo-request-id'), challenge: response.headers.get('www-authenticate'), cacheControl: response.headers.get('cache-control') }
      result.http.push(entry)
      if (!stream || response.status !== status) {
        const text = await response.text()
        let value
        try { value = JSON.parse(text) } catch { value = text }
        entry.body = safeValue(value)
        assert.equal(response.status, status, `${label}: ${method} ${resource}: ${JSON.stringify(entry.body)}`)
        if (status >= 400) {
          assert.ok(error instanceof RegExp, 'Each expected HTTP error needs a body matcher')
          assert.match(text, error)
          entry.expectedError = error.source
          if (status === 401) assert.equal(entry.challenge, 'Bearer', `${resource} must challenge invalid device credentials`)
        }
        return value
      }
      assert.equal(response.status, status, `${label}: ${method} ${resource}`)
      const completion = response.text().then(text => {
        entry.body = text
        entry.endedAt = Date.now()
        return { text, error: null }
      }, failure => {
        entry.failure = failure.message
        return { text: '', error: failure }
      })
      return { response, entry, completion }
    },
    async clean() {
      await Promise.all(tasks)
      assert.deepEqual(result.errors, [])
      for (const failure of result.requestFailures.filter(entry => !entry.cleanup)) {
        const replacement = failure.method === 'GET' && failure.failure?.errorText === 'net::ERR_ABORTED'
          && supersededReads.some(entry => entry.page === failure.page && entry.pathname === failure.pathname)
          && result.network.find(entry => entry.page === failure.page && entry.action === failure.action && entry.method === 'GET'
            && entry.url === failure.url && entry.status === 200 && entry.at > failure.at && entry.at - failure.at < 2_000)
        if (replacement) failure.supersededBy = { at: replacement.at, status: replacement.status, url: replacement.url }
      }
      assert.deepEqual(result.requestFailures.filter(entry => !entry.cleanup && !entry.supersededBy), [])
      for (const expected of result.expectedErrors) assert.equal(expected.matched, 1, JSON.stringify(expected))
      const unexpected = result.console.filter(entry => entry.type === 'error' && !entry.cleanup).filter(entry => {
        const match = /^Failed to load resource: the server responded with a status of (\d+)\b/.exec(entry.text)
        if (!match) return true
        return !result.network.some(request => request.expected && request.page === entry.page && request.status === Number(match[1])
          && request.url === entry.location.url && Math.abs(request.at - entry.at) < 20_000)
      })
      assert.deepEqual(unexpected, [])
    },
    beginCleanup() { closing = true },
    async save() {
      await Promise.all(tasks)
      await writeFile(path.join(artifacts, 'device-limits-results.json'), JSON.stringify(result, null, 2))
    },
  }
  return recorder
}

export async function verifyEmbeddedAssets(origins, evidence) {
  for (const origin of origins) for (const asset of ['app.js', 'app.css']) {
    const response = await fetch(`${origin}/assets/${asset}`)
    assert.equal(response.status, 200)
    const served = createHash('sha256').update(Buffer.from(await response.arrayBuffer())).digest('hex')
    const built = createHash('sha256').update(await readFile(path.join(repository, 'web/dist/assets', asset))).digest('hex')
    assert.equal(served, built, `${origin} must embed the final build, not an older bundle`)
    evidence.result.assets.push({ origin, asset, sha256: served, browser: false })
  }
}

export async function controlledUpstream(source) {
  const calls = [], controls = new Map(), failures = []
  const server = createServer(async (request, response) => {
    try {
      const chunks = []
      for await (const chunk of request) chunks.push(chunk)
      const body = Buffer.concat(chunks)
      const payload = body.length ? JSON.parse(body.toString()) : null
      if (request.url === '/v1/chat/completions') calls.push(payload)
      const control = [...controls.values()].find(entry => payload?.messages?.some(message => message.role === 'user' && message.content === entry.marker))
      if (control) {
        assert.equal(payload.stream, true)
        controls.delete(control.marker)
        const identity = { id: `limits-${calls.length}`, object: 'chat.completion.chunk', model: payload.model }
        const frame = value => `data: ${JSON.stringify({ ...identity, ...value })}\n\n`
        response.on('close', () => { control.closed = true; control.onClosed() })
        response.writeHead(200, { 'content-type': 'text/event-stream' })
        response.write(frame({ choices: [{ index: 0, delta: { role: 'assistant', content: 'Accepted synthetic stream. ' }, finish_reason: null }] }))
        control.started()
        if (control.mode === 'hold') await Promise.race([control.released, control.closedPromise])
        if (response.destroyed) return
        response.end(frame({ choices: [{ index: 0, delta: { content: 'Released successfully.' }, finish_reason: 'stop' }] })
          + (control.mode === 'unknown' ? '' : frame({ choices: [], usage: { prompt_tokens: 30, completion_tokens: 12, total_tokens: 42 } }))
          + 'data: [DONE]\n\n')
        return
      }
      const upstream = await fetch(`${new URL(source.baseUrl).origin}${request.url}`, {
        method: request.method,
        headers: { 'content-type': 'application/json', ...(request.headers.authorization ? { authorization: request.headers.authorization } : {}) },
        ...(body.length ? { body } : {}),
        signal: AbortSignal.timeout(15_000),
      })
      response.writeHead(upstream.status, { 'content-type': upstream.headers.get('content-type') ?? 'application/json' })
      response.end(Buffer.from(await upstream.arrayBuffer()))
    } catch (error) {
      failures.push(error.message)
      if (!response.headersSent) response.writeHead(500)
      response.end()
    }
  })
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve))
  return {
    baseUrl: `http://127.0.0.1:${server.address().port}/v1`, calls, failures,
    control(marker, mode = 'hold') {
      assert.ok(!controls.has(marker))
      let started, release, onClosed
      const began = new Promise(resolve => { started = resolve })
      const released = new Promise(resolve => { release = resolve })
      const closedPromise = new Promise(resolve => { onClosed = resolve })
      const control = { marker, mode, started, release, began, released, closedPromise, onClosed, closed: false }
      controls.set(marker, control)
      return control
    },
    async close() {
      for (const control of controls.values()) control.release()
      await new Promise(resolve => { server.closeAllConnections(); server.close(resolve) })
    },
  }
}

export async function login(page, credentials) {
  await page.getByLabel('用户名', { exact: true }).fill(credentials.username)
  await page.getByLabel('密码', { exact: true }).fill(credentials.password)
  await page.getByRole('button', { name: '登录', exact: true }).click()
  await page.getByRole('button', { name: '登录', exact: true }).waitFor({ state: 'hidden' })
}

export async function fillLimits(root, limits) {
  await root.locator('input[name="monthly_tokens"]').fill(limits.monthly_tokens?.toString() ?? '')
  await root.locator('input[name="max_concurrent_requests"]').fill(limits.max_concurrent_requests?.toString() ?? '')
  await root.locator('input[name="requests_per_minute"]').fill(limits.requests_per_minute?.toString() ?? '')
  const date = limits.expires_at_ms == null ? '' : await root.page().evaluate(timestamp => {
    const value = new Date(timestamp)
    return new Date(timestamp - value.getTimezoneOffset() * 60_000).toISOString().slice(0, -1)
  }, limits.expires_at_ms)
  await root.locator('input[name="expires_at_ms"]').fill(date)
}

export async function layouts(page, root, label, evidence, restore) {
  for (const width of [1440, 390, 320]) for (const theme of ['dark', 'light']) {
    await page.setViewportSize({ width, height: 960 })
    await page.emulateMedia({ colorScheme: theme, reducedMotion: 'reduce' })
    if (await page.evaluate(() => document.documentElement.style.colorScheme) !== theme) {
      await page.evaluate(value => localStorage.setItem('ternilo.theme', value), theme)
      await page.reload()
      await restore()
    }
    assert.equal(await page.evaluate(() => document.documentElement.style.colorScheme), theme, 'Application applies its persisted theme preference')
    await root.scrollIntoViewIfNeeded()
    await root.evaluate(element => Promise.all(element.getAnimations({ subtree: true }).map(animation => animation.finished.catch(() => {}))))
    const overflow = await root.evaluate(element => ({
      document: document.documentElement.scrollWidth > innerWidth,
      clipped: [...element.querySelectorAll('input, button, [data-device-limits], [data-device-usage]')].filter(child => {
        const bounds = child.getBoundingClientRect()
        return bounds.width && (bounds.left < -1 || bounds.right > innerWidth + 1)
      }).map(child => child.outerHTML.slice(0, 180)),
      root: element.scrollWidth > element.clientWidth + 1,
    }))
    assert.deepEqual(overflow, { document: false, clipped: [], root: false }, `${label} ${width} ${theme}`)
    await evidence.capture(page, `${label}-${width}-${theme}`, root)
  }
}

export async function authorizeDevice(server, evidence, name, scope, limits, token = server.owner.session.access_token) {
  const authorization = await evidence.http(server.origin, '/api/v1/model-device/authorize', { method: 'POST', body: { device_name: name } })
  await evidence.http(server.origin, '/api/v1/model-access/device-authorization', { token, method: 'POST', status: 204, body: { user_code: authorization.user_code, scope, limits } })
  let poll
  for (let attempt = 0; attempt < 12; attempt++) {
    poll = await evidence.http(server.origin, '/api/v1/model-device/token', { method: 'POST', body: { device_code: authorization.device_code } })
    if (poll.status === 'authorized') break
    assert.ok(['pending', 'slow_down'].includes(poll.status), JSON.stringify(poll))
    await new Promise(resolve => setTimeout(resolve, poll.interval * 1000))
  }
  assert.equal(poll.status, 'authorized')
  assert.deepEqual(poll.session.identity.limits, limits)
  return poll
}

export function deviceClient(server, evidence, device) {
  const id = device.session.identity.device_id
  const resource = `/api/v1/model-access/devices/${id}`
  return {
    id, resource,
    patch: limits => evidence.http(server.origin, resource, { token: server.owner.session.access_token, method: 'PATCH', body: limits }),
    usage: () => evidence.http(server.origin, `${resource}/usage`, { token: server.owner.session.access_token }),
    async request(route, marker, options = {}) {
      return evidence.http(server.origin, `${route.path}/chat/completions`, {
        token: device.token, method: 'POST', body: { model: route.model, messages: [{ role: 'user', content: marker }], max_tokens: 64, stream: options.stream ?? false },
        ...options,
      })
    },
  }
}

export async function usageEquals(client, expected) {
  return until(client.usage, usage => Object.entries(expected).every(([key, value]) => usage[key] === value), `device ${client.id} usage ${JSON.stringify(expected)}`)
}

export async function assertBlocked(client, routes, reason, evidence) {
  const before = routes.map(route => route.source.calls.length)
  const usage = await client.usage()
  for (const route of routes) {
    const response = await client.request(route, `denied-${route.name}-${reason}`, { status: 429, error: new RegExp(`model device ${reason}`) })
    assert.equal(response.error.code, 'quota_exceeded')
  }
  assert.deepEqual(routes.map(route => route.source.calls.length), before, 'Denied calls never reach any upstream or fall back to another budget')
  assert.deepEqual(await client.usage(), usage, 'Denied requests do not add usage or reservations')
  evidence.check(`all-three-origins-blocked-${reason}`, { deviceId: client.id, usage })
}

export async function finishHeld(client, streams, expectedUsed) {
  for (const stream of streams) assert.equal(stream.control.closed, false, 'Lowering limits must not abort an already accepted stream')
  for (const stream of streams) stream.control.release()
  for (const stream of streams) {
    const ended = await deadline(stream.delivery.completion, 'accepted stream finishes after release')
    assert.equal(ended.error, null)
    assert.match(ended.text, /Released successfully/)
    assert.match(ended.text, /"total_tokens":42/)
    assert.match(ended.text, /\[DONE\]/)
    assert.doesNotMatch(ended.text, /"error"/)
  }
  return usageEquals(client, { used_tokens: expectedUsed, reserved_tokens: 0, active_requests: 0 })
}

export async function holdRequest(client, route, marker) {
  const control = route.source.control(marker)
  const delivery = await client.request(route, marker, { stream: true })
  await deadline(control.began, 'synthetic stream started')
  assert.ok(delivery.entry.requestId, 'Accepted gateway stream identifies its ledger entry')
  return { control, delivery, route }
}
