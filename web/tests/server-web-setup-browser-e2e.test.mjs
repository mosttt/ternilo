import assert from 'node:assert/strict'
import test from 'node:test'
import { mkdir, mkdtemp, readFile, stat } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import { chromium } from 'playwright'
import { execute, freePort, repository, serverRequest, startPostgres, startProcess, stopProcess, waitForHttp } from './platform-e2e-fixture.mjs'

const binary = process.env.TERNILO_E2E_SERVER_BINARY ?? path.join(repository, 'target/debug/ternilo-server')
const clean = Object.fromEntries(Object.keys(process.env).filter(key => key.startsWith('TERNILO_')).map(key => [key, undefined]))
const owner = { username: 'web-owner', email: 'web-owner@example.test', password: 'web-owner-password' }

async function openSetup(page, origin, key) {
  await page.goto(`${origin}/#setup_token=${key}`)
  await page.locator('[data-server-setup]').waitFor()
  assert.equal(new URL(page.url()).hash, '', 'the Key is removed from browser history')
  await page.getByLabel('用户名', { exact: true }).fill(owner.username)
  await page.getByLabel('邮箱', { exact: true }).fill(owner.email)
  await page.getByLabel('密码', { exact: true }).fill(owner.password)
  await page.getByLabel('确认密码', { exact: true }).fill(owner.password)
}

async function signIn(page) {
  await page.getByLabel('用户名', { exact: true }).fill(owner.username)
  await page.getByLabel('密码', { exact: true }).fill(owner.password)
  await page.getByRole('button', { name: '登录', exact: true }).click()
  await page.getByRole('button', { name: '用户设置', exact: true }).waitFor()
}

function request(origin, body) {
  return fetch(`${origin}/api/v1/setup`, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(body) })
}

async function submitInViewport(page) {
  return page.locator('[data-server-setup] button[type="submit"]').evaluate(button => {
    const bounds = button.getBoundingClientRect()
    return bounds.top >= 0 && bounds.bottom <= innerHeight && bounds.left >= 0 && bounds.right <= innerWidth
  })
}

async function swipeUp(page) {
  const { width, height } = page.viewportSize()
  const cdp = await page.context().newCDPSession(page)
  const x = width - 25, y = height - 35
  await cdp.send('Input.dispatchTouchEvent', { type: 'touchStart', touchPoints: [{ x, y }] })
  for (let step = 1; step <= 8; step++) {
    await cdp.send('Input.dispatchTouchEvent', { type: 'touchMove', touchPoints: [{ x, y: y - (height - 100) * step / 8 }] })
  }
  await cdp.send('Input.dispatchTouchEvent', { type: 'touchEnd', touchPoints: [] })
  await cdp.detach()
}

test('Setup fields and submit remain reachable by wheel, keyboard and touch across viewport sizes', { timeout: 90_000 }, async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-setup-scroll-'))
  const artifacts = process.env.TERNILO_E2E_ARTIFACT_DIR ?? directory
  await mkdir(artifacts, { recursive: true })
  const origin = `http://127.0.0.1:${await freePort()}`
  let server, browser
  const errors = []
  try {
    server = startProcess(binary, ['serve', '--config-dir', directory, '--listen', new URL(origin).host], clean)
    await waitForHttp(`${origin}/readyz`, server)
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
    for (const touch of [false, true]) {
      const page = await browser.newPage({ locale: 'zh-CN', isMobile: touch, hasTouch: touch, colorScheme: touch ? 'light' : 'dark', serviceWorkers: 'block' })
      page.on('pageerror', error => errors.push(error.message))
      page.on('console', message => { if (message.type() === 'error') errors.push(message.text()) })
      page.on('requestfailed', request => errors.push(`${request.method()} ${new URL(request.url()).pathname}: ${request.failure()?.errorText}`))
      page.on('response', response => { if (response.status() >= 400) errors.push(`HTTP ${response.status()} ${new URL(response.url()).pathname}`) })
      const sizes = touch ? [[390, 844], [320, 568], [844, 390], [390, 360]] : [[1895, 842], [1366, 768], [1280, 480], [960, 420]]
      for (const [width, height] of sizes) {
        await page.setViewportSize({ width, height })
        await page.goto(origin)
        await page.locator('[data-server-setup]').waitFor()
        await page.getByRole('combobox', { name: '数据库', exact: true }).click()
        await page.getByRole('option', { name: 'PostgreSQL', exact: true }).click()
        await page.mouse.move(width - 25, height / 2)
        if (touch) {
          for (let attempt = 0; attempt < 12 && !await submitInViewport(page); attempt++) {
            await swipeUp(page)
          }
        } else {
          await page.mouse.wheel(0, 10_000)
        }
        await page.waitForFunction(() => {
          const button = document.querySelector('[data-server-setup] button[type="submit"]')
          const bounds = button?.getBoundingClientRect()
          return bounds && bounds.top >= 0 && bounds.bottom <= innerHeight
        }, null, { timeout: 3_000 })
        assert.equal(await submitInViewport(page), true, `the submit button is reachable by ${touch ? 'touch' : 'wheel'} at ${width}x${height}`)
        assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth && document.querySelector('[data-server-setup]').scrollWidth <= innerWidth), true, 'no horizontal clipping')
        await page.screenshot({ path: path.join(artifacts, `setup-scroll-${touch ? 'touch' : 'wheel'}-${width}x${height}.png`) })
        await page.getByLabel('初始化 Key', { exact: true }).focus()
        for (let step = 0; step < 10; step++) {
          await page.keyboard.press('Tab')
          if (await page.locator('[data-server-setup] button[type="submit"]').evaluate(button => button === document.activeElement)) break
        }
        assert.equal(await page.locator('[data-server-setup] button[type="submit"]').evaluate(button => button === document.activeElement), true, 'Tab reaches submit after the database and administrator fields')
        assert.equal(await submitInViewport(page), true, 'keyboard focus brings submit into the viewport')
      }
      if (touch) {
        await page.setViewportSize({ width: 390, height: 844 })
        await page.getByLabel('确认密码', { exact: true }).fill('resize-focus-password')
        await page.setViewportSize({ width: 390, height: 360 })
        for (let attempt = 0; attempt < 4 && !await submitInViewport(page); attempt++) await swipeUp(page)
        await page.waitForFunction(() => {
          const bounds = document.querySelector('[data-server-setup] button[type="submit"]')?.getBoundingClientRect()
          return bounds && bounds.top >= 0 && bounds.bottom <= innerHeight
        }, null, { timeout: 3_000 })
        assert.equal(await submitInViewport(page), true, 'submit remains reachable after resizing while an input is focused')
        await page.screenshot({ path: path.join(artifacts, 'setup-scroll-focused-resize.png') })
      }
      await page.close()
    }
    assert.deepEqual(errors, [], 'responsive setup has no browser or network errors')
  } finally {
    await browser?.close()
    await stopProcess(server)
  }
})

for (const kind of ['sqlite', 'postgres']) {
  test(`Fresh serve supports protected ${kind} web setup, database retry and stable restart`, { timeout: 150_000 }, async () => {
    const directory = await mkdtemp(path.join(tmpdir(), `ternilo-web-setup-${kind}-`))
    const artifacts = process.env.TERNILO_E2E_ARTIFACT_DIR ?? directory
    await mkdir(artifacts, { recursive: true })
    const origin = `http://127.0.0.1:${await freePort()}`
    const configPath = path.join(directory, 'config.json')
    let server, browser, postgres
    const errors = []
    try {
      server = startProcess(binary, ['serve', '--config-dir', directory, '--listen', new URL(origin).host], clean)
      await waitForHttp(`${origin}/readyz`, server)
      assert.equal((await fetch(`${origin}/readyz`).then(response => response.json())).status, 'setup_required')
      assert.equal(await stat(configPath).then(() => true, () => false), false, 'setup does not need saved config or an open business database')
      const key = server.diagnostics().match(/Initialization Key: (\S+)/)?.[1]
      assert.ok(key, server.diagnostics())
      const form = { ...owner, setup_token: key, database: { kind }, public_url: origin }
      const wrong = await request(origin, { ...form, setup_token: 'invalid', database: { kind: 'postgres', url: 'postgresql://private:secret@127.0.0.1:1/secret', migration_url: null } })
      assert.equal(wrong.status, 401, 'authentication precedes database access')
      assert.equal(await stat(configPath).then(() => true, () => false), false)
      if (kind === 'postgres') {
        const failed = await request(origin, { ...form, database: { kind: 'postgres', url: 'postgresql://private:secret@127.0.0.1:1/secret?connect_timeout=1', migration_url: null } })
        assert.equal(failed.status, 500)
        assert.ok(!(await failed.text()).includes('private:secret'), 'database errors do not echo credentials')
        assert.equal(await stat(configPath).then(() => true, () => false), false)
        postgres = await startPostgres({ prefix: 'ternilo-web-setup', database: 'web_setup' })
        await postgres.query("CREATE ROLE ternilo_runtime NOLOGIN; CREATE ROLE ternilo_web_runtime LOGIN PASSWORD 'runtime-password'; GRANT ternilo_runtime TO ternilo_web_runtime; GRANT USAGE ON SCHEMA public TO ternilo_runtime; REVOKE CREATE ON SCHEMA public FROM PUBLIC;")
      }
      browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
      const page = await browser.newPage({ locale: 'zh-CN', viewport: { width: 390, height: 844 }, serviceWorkers: 'block' })
      page.on('pageerror', error => errors.push(error.message))
      page.on('console', message => { if (message.type() === 'error') errors.push(message.text()) })
      await openSetup(page, origin, key)
      if (kind === 'postgres') {
        await page.getByRole('combobox', { name: '数据库', exact: true }).click()
        await page.getByRole('option', { name: 'PostgreSQL', exact: true }).click()
        const runtime = new URL(postgres.url); runtime.username = 'ternilo_web_runtime'; runtime.password = 'runtime-password'
        await page.getByLabel('PostgreSQL 连接地址', { exact: true }).fill(runtime.toString())
        await page.getByLabel('建表账号连接地址（可选）', { exact: true }).fill(postgres.url)
      }
      await page.getByLabel('确认密码', { exact: true }).fill('different-password')
      await page.getByRole('button', { name: '保存配置并创建管理员', exact: true }).click()
      await page.getByRole('alert').getByText('两次输入的密码不一致。', { exact: true }).waitFor()
      assert.equal(await stat(configPath).then(() => true, () => false), false, 'client validation does not initialize the instance')
      await page.getByLabel('确认密码', { exact: true }).fill(owner.password)
      await page.screenshot({ path: path.join(artifacts, `setup-${kind}-mobile.png`), fullPage: true })
      await page.getByRole('button', { name: '保存配置并创建管理员', exact: true }).click()
      await page.waitForFunction(() => !window.__TERNILO_BOOT__?.setup, { timeout: 35_000 })
      const saved = await readFile(configPath, 'utf8')
      assert.ok(!saved.includes(key) && !saved.includes(owner.password), 'no plaintext Key or account password in config')
      const config = JSON.parse(saved)
      assert.equal(config.public_url, origin)
      assert.ok(config.database_url.startsWith(kind === 'sqlite' ? 'sqlite:' : 'postgres'))
      if (process.platform !== 'win32') assert.equal((await stat(configPath)).mode & 0o777, 0o600)
      await signIn(page)
      const login = await serverRequest(origin, '/auth/login', { body: { username: owner.username, password: owner.password } })
      const session = await serverRequest(origin, '/auth/session', { token: login.access_token })
      assert.equal(session.is_instance_owner, true)
      assert.equal((await serverRequest(origin, '/projects', { token: login.access_token, tenantId: session.personal_tenant_id })).projects.length, 1, 'the complete application serves authenticated routes after switching')
      assert.deepEqual(errors, [], 'mobile rendering, database selection and setup have no browser errors')
      await page.close()
      await stopProcess(server)
      server = startProcess(binary, ['serve', '--config-dir', directory], clean)
      await waitForHttp(`${origin}/auth/config`, server)
      assert.equal((await fetch(`${origin}/auth/config`).then(response => response.json())).initialized, true)
      assert.ok(!server.diagnostics().includes('Initialization Key:'), 'initialized restarts do not advertise setup')
      assert.equal(await readFile(configPath, 'utf8'), saved, 'restarts do not rewrite private settings')
      const again = await serverRequest(origin, '/auth/login', { body: { username: owner.username, password: owner.password } })
      const againSession = await serverRequest(origin, '/auth/session', { token: again.access_token })
      assert.equal(againSession.user.user_id, session.user.user_id)
      const stale = await fetch(`${origin}/api/v1/auth/setup`, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ ...owner, setup_token: key }) })
      assert.equal(stale.status, 409)
      assert.deepEqual(errors, [], 'mobile rendering, database selection and setup have no browser errors')
    } finally {
      await browser?.close()
      await stopProcess(server)
      await postgres?.stop()
    }
  })
}

test('Terminal setup without an owner issues fresh log Keys until owner setup completes', { timeout: 60_000 }, async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-deferred-web-setup-'))
  const origin = `http://127.0.0.1:${await freePort()}`
  await execute(binary, ['setup', '--config-dir', directory, '--non-interactive', '--listen', new URL(origin).host], { env: { ...process.env, ...clean } })
  let server
  try {
    server = startProcess(binary, ['serve', '--config-dir', directory], clean)
    await waitForHttp(`${origin}/auth/config`, server)
    const firstKey = server.diagnostics().match(/Initialization Key: (\S+)/)?.[1]
    assert.ok(firstKey)
    assert.equal((await fetch(`${origin}/auth/config`).then(response => response.json())).initialized, false)
    await stopProcess(server)
    server = startProcess(binary, ['serve', '--config-dir', directory], clean)
    await waitForHttp(`${origin}/auth/config`, server)
    const secondKey = server.diagnostics().match(/Initialization Key: (\S+)/)?.[1]
    assert.ok(secondKey && secondKey !== firstKey)
    const old = await fetch(`${origin}/api/v1/auth/setup`, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ ...owner, setup_token: firstKey }) })
    assert.equal(old.status, 401)
    await serverRequest(origin, '/auth/setup', { body: { ...owner, setup_token: secondKey } })
    assert.equal((await fetch(`${origin}/auth/config`).then(response => response.json())).initialized, true)
  } finally { await stopProcess(server) }
})
