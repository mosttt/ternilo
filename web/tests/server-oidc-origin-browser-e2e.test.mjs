import assert from 'node:assert/strict'
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { chromium } from 'playwright'
import { execute, freePort, initializeServer, repository, serverRequest, startOidcServer, startProcess, stopProcess, waitForHttp } from './platform-e2e-fixture.mjs'

const binary = process.env.TERNILO_E2E_SERVER_BINARY ?? path.join(repository, 'target/debug/ternilo-server')
const resource = '/admin/instance/authentication'

test('OIDC explains insecure or mismatched origins and preserves native recovery for an invalid public URL', { timeout: 120000 }, async () => {
  const temporary = await mkdtemp(path.join(tmpdir(), 'ternilo-oidc-origin-'))
  const artifacts = process.env.TERNILO_E2E_ARTIFACT_DIR ?? temporary
  await mkdir(artifacts, { recursive: true })
  const origin = `http://127.0.0.1:${await freePort()}`
  const provider = await startOidcServer({ audience: 'origin-browser', subject: 'origin-owner' })
  let application, browser, page
  const errors = []
  try {
    application = await initializeServer({ directory: temporary, origin, binary,
      oidc: { issuer: provider.issuer, audience: 'origin-browser', client_id: 'origin-browser' },
    })
    const ownerToken = application.owner.session.access_token
    const credentials = { username: application.owner.username, password: application.owner.password }
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined,
      args: ['--host-resolver-rules=MAP insecure.ternilo.test 127.0.0.1', '--no-proxy-server'],
    })
    page = await browser.newPage({ viewport: { width: 390, height: 844 }, serviceWorkers: 'block' })
    page.on('pageerror', error => errors.push(error.message))
    page.on('console', message => { if (message.type() === 'error') errors.push(message.text()) })
    // Resolve a non-secure origin to the isolated server for both HTTP and WebSocket.
    const insecure = `http://insecure.ternilo.test:${new URL(origin).port}`
    await page.goto(insecure)
    assert.equal(await page.evaluate(() => isSecureContext), false)
    assert.equal(await page.evaluate(() => Boolean(crypto.subtle)), false)
    await page.getByRole('button', { name: '使用组织账号登录', exact: true }).click()
    await page.getByRole('alert').filter({ hasText: '浏览器加密功能' }).waitFor()
    assert.equal(await page.evaluate(() => sessionStorage.getItem('ternilo.oidc.verifier')), null)
    assert.equal(new URL(page.url()).origin, insecure)
    await page.screenshot({ path: path.join(artifacts, 'oidc-insecure-origin.png') })
    await page.getByLabel('用户名', { exact: true }).fill(credentials.username)
    await page.getByLabel('密码', { exact: true }).fill(credentials.password)
    await page.getByRole('button', { name: '登录', exact: true }).click()
    await page.getByLabel('用户名', { exact: true }).waitFor({ state: 'hidden' })
    await page.close()

    page = await browser.newPage({ serviceWorkers: 'block' })
    page.on('pageerror', error => errors.push(error.message))
    page.on('console', message => { if (message.type() === 'error') errors.push(message.text()) })
    let settings = await serverRequest(origin, resource, { token: ownerToken })
    const update = publicUrl => ({ revision: settings.revision, public_url: publicUrl, oidc: {
      issuer: settings.oidc.issuer, audience: settings.oidc.audience, client_id: settings.oidc.client_id,
      scopes: settings.oidc.scopes, token_auth_method: settings.oidc.token_auth_method,
    }, turnstile: null })
    for (const publicUrl of ['https://0.0.0.0:4321', 'https://[::]:4321']) {
      const response = await fetch(`${origin}/api/v1${resource}`, {
        method: 'PUT', headers: { authorization: `Bearer ${ownerToken}`, 'content-type': 'application/json' }, body: JSON.stringify(update(publicUrl)),
      })
      assert.equal(response.status, 400, await response.text())
      assert.equal((await serverRequest(origin, resource, { token: ownerToken })).revision, settings.revision)
    }
    settings = await serverRequest(origin, resource, { token: ownerToken, method: 'PUT', body: update('https://ternilo.example.test') })
    await page.goto(origin)
    assert.equal(await page.evaluate(() => Boolean(crypto.subtle)), true)
    await page.getByRole('button', { name: '使用组织账号登录', exact: true }).click()
    await page.getByRole('alert').filter({ hasText: '当前访问地址与登录回调地址不一致' }).waitFor()
    assert.equal(new URL(page.url()).origin, origin)
    assert.equal(await page.evaluate(() => sessionStorage.getItem('ternilo.oidc.verifier')), null)
    await page.getByLabel('用户名', { exact: true }).fill(credentials.username)
    await page.getByLabel('密码', { exact: true }).fill(credentials.password)
    await page.getByRole('button', { name: '登录', exact: true }).click()
    await page.getByLabel('用户名', { exact: true }).waitFor({ state: 'hidden' })
    await page.goto(`${origin}/admin/instance`)
    const panel = page.getByRole('region', { name: '登录与人机验证' })
    await panel.getByLabel('Server 公网地址', { exact: true }).fill('https://0.0.0.0:4321')
    let settingsWrites = 0
    page.on('request', request => { if (request.method() === 'PUT' && request.url().endsWith(resource)) settingsWrites++ })
    await panel.getByRole('button', { name: '保存登录设置', exact: true }).click()
    await panel.getByRole('alert').filter({ hasText: '是监听地址' }).waitFor()
    assert.equal(settingsWrites, 0)
    await page.screenshot({ path: path.join(artifacts, 'oidc-invalid-public-url.png') })
    await panel.getByLabel('Server 公网地址', { exact: true }).fill(origin)
    const saveResponse = page.waitForResponse(response => response.request().method() === 'PUT' && response.url().endsWith(resource))
    await panel.getByRole('button', { name: '保存登录设置', exact: true }).click()
    assert.equal((await saveResponse).status(), 200)

    // Exercise a persisted invalid deployment URL without modifying an actual instance.
    const configPath = application.configPath
    await page.close()
    await stopProcess(application)
    const config = JSON.parse(await readFile(configPath, 'utf8'))
    config.public_url = 'https://0.0.0.0:4321'
    await writeFile(configPath, JSON.stringify(config), { mode: 0o600 })
    const cleanEnvironment = Object.fromEntries(Object.keys(process.env).filter(key => key.startsWith('TERNILO_')).map(key => [key, undefined]))
    await execute(binary, ['admin', 'reset-authentication', '--config-dir', path.dirname(configPath)], { cwd: repository, env: { ...process.env, ...cleanEnvironment } })
    application = startProcess(binary, ['serve', '--config-dir', path.dirname(configPath)], cleanEnvironment)
    await waitForHttp(`${origin}/readyz`, application)
    const metadata = await (await fetch(`${origin}/auth/config`)).json()
    assert.equal(metadata.oidc_enabled, false)
    assert.equal(metadata.native_enabled, true)
    const recovered = await serverRequest(origin, '/auth/login', { body: credentials })
    settings = await serverRequest(origin, resource, { token: recovered.access_token })
    assert.equal(settings.oidc_unavailable, true)
    assert.equal(settings.public_url, config.public_url)
    await serverRequest(origin, resource, { token: recovered.access_token, method: 'PUT', body: update(origin) })
    assert.equal((await (await fetch(`${origin}/auth/config`)).json()).oidc_enabled, true)
    assert.deepEqual(errors, [])
  } catch (error) {
    if (page && !page.isClosed()) await page.screenshot({ path: path.join(artifacts, 'oidc-origin-failure.png') }).catch(() => {})
    throw new Error(`${error.stack}\n${application?.diagnostics() ?? ''}`)
  } finally {
    await browser?.close()
    await stopProcess(application)
    await provider.close()
    await rm(temporary, { recursive: true, force: true })
  }
})
