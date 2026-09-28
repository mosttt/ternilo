import assert from 'node:assert/strict'
import { execFile } from 'node:child_process'
import { mkdir, mkdtemp, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import { promisify } from 'node:util'
import test from 'node:test'
import { chromium } from 'playwright'
import { freePort, initializeServer, repository, serverRequest, stopProcess } from './platform-e2e-fixture.mjs'

const execute = promisify(execFile)
const binary = process.env.TERNILO_E2E_SERVER_BINARY ?? path.join(repository, 'target/debug/ternilo-server')
const environment = Object.fromEntries(Object.entries(process.env).filter(([key]) => !key.startsWith('TERNILO_')))
const replacement = ' recovered owner password '

function resetPassword(config, username, password, fromStdin = true) {
  const result = execute(binary, ['admin', 'reset-password', '--config', config, '--username', username,
    ...(fromStdin ? ['--password-stdin'] : [])], { cwd: repository, env: environment, timeout: 30_000 })
  result.child.stdin.end(`${password}\r\n`)
  return result
}

async function login(page, username, password) {
  await page.getByLabel('用户名', { exact: true }).fill(username)
  await page.getByLabel('密码', { exact: true }).fill(password)
  const [response] = await Promise.all([
    page.waitForResponse(response => new URL(response.url()).pathname === '/api/v1/auth/login' && response.request().method() === 'POST'),
    page.getByRole('button', { name: '登录', exact: true }).click(),
  ])
  assert.equal(response.status(), 200)
  await page.getByRole('button', { name: '用户设置', exact: true }).waitFor()
  return response.json()
}

test('operator password recovery revokes old browser access and preserves the owner and resources', { timeout: 120_000 }, async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-password-recovery-'))
  const artifacts = process.env.TERNILO_E2E_ARTIFACT_DIR ?? directory
  await mkdir(artifacts, { recursive: true })
  let application, browser
  const errors = []
  let revocationStarted = false
  try {
    const origin = `http://127.0.0.1:${await freePort()}`
    application = await initializeServer({ directory, origin, binary })
    const owner = application.owner
    const { project } = await serverRequest(origin, `/tenants/${owner.session.personal_tenant_id}/projects`, {
      token: owner.session.access_token, body: { name: 'Retained recovery project' },
    })
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
    const page = await browser.newPage({ viewport: { width: 1280, height: 900 }, serviceWorkers: 'block' })
    page.on('pageerror', error => errors.push(error.message))
    page.on('console', message => {
      if (message.type() === 'error' && !(revocationStarted && message.text().includes('401'))) errors.push(message.text())
    })
    page.on('response', response => {
      if (response.status() >= 400 && !(revocationStarted && response.status() === 401)) errors.push(`HTTP ${response.status()}: ${new URL(response.url()).pathname}`)
    })
    await page.goto(origin)
    const before = await login(page, owner.username, owner.password)
    await assert.rejects(resetPassword(application.configPath, owner.username, 'short'), error => {
      assert.equal(error.code, 1)
      assert.match(error.stderr, /8 to 1024 bytes/)
      return true
    })
    await assert.rejects(resetPassword(application.configPath, owner.username, replacement, false), error => {
      assert.equal(error.code, 1)
      assert.match(error.stderr, /--password-stdin/)
      assert.ok(!`${error.stdout}${error.stderr}`.includes(replacement))
      return true
    })
    assert.equal((await serverRequest(origin, '/auth/session', { token: before.access_token })).user.user_id, before.user.user_id)
    revocationStarted = true
    const result = await resetPassword(application.configPath, owner.username, replacement)
    assert.match(result.stdout, /Password reset/)
    assert.ok(result.stdout.includes(before.user.user_id))
    assert.ok(!`${result.stdout}${result.stderr}`.includes(replacement))
    const denied = await fetch(`${origin}/api/v1/auth/session`, { headers: { authorization: `Bearer ${before.access_token}` } })
    assert.equal(denied.status, 401)
    const oldLogin = await fetch(`${origin}/api/v1/auth/login`, { method: 'POST', headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ username: owner.username, password: owner.password }) })
    assert.equal(oldLogin.status, 401)
    await page.getByLabel('用户名', { exact: true }).waitFor()
    const after = await login(page, owner.username, replacement)
    assert.equal(after.user.user_id, before.user.user_id)
    assert.equal(after.personal_tenant_id, before.personal_tenant_id)
    assert.equal(after.personal_project_id, before.personal_project_id)
    assert.equal(after.is_instance_owner, true)
    const projects = await serverRequest(origin, '/projects', { token: after.access_token, tenantId: after.personal_tenant_id })
    assert.ok(projects.projects.some(item => item.project_id === project.project_id))
    await page.screenshot({ path: path.join(artifacts, 'server-password-recovered.png'), fullPage: true })
    assert.deepEqual(errors, [])
  } finally {
    if (browser) await browser.close()
    if (application) await stopProcess(application)
    await rm(directory, { recursive: true, force: true })
  }
})
