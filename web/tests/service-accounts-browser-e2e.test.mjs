import assert from 'node:assert/strict'
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { chromium } from 'playwright'
import { execute, repository, freePort, initializeServer, serverRequest, stopProcess } from './platform-e2e-fixture.mjs'

async function status(origin, resource, token, tenant, method = 'GET', body) {
  const response = await fetch(`${origin}/api/v1${resource}`, {
    method,
    headers: { authorization: `Bearer ${token}`, 'x-ternilo-tenant': tenant, ...(body === undefined ? {} : { 'content-type': 'application/json' }) },
    ...(body === undefined ? {} : { body: JSON.stringify(body) }),
  })
  return response.status
}

test('service accounts create independent identities, constrain real API access and manage credentials in desktop and mobile browsers', { timeout: 120_000 }, async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-service-accounts-'))
  const origin = `http://127.0.0.1:${await freePort()}`
  const artifacts = process.env.TERNILO_E2E_ARTIFACT_DIR
  if (artifacts) await mkdir(artifacts, { recursive: true })
  let application, browser, page
  const report = { status: 'running', errors: [], network: [], checks: [] }
  try {
    application = await initializeServer({ directory, origin, managedExecutionEnabled: true })
    const tenant = application.owner.session.personal_tenant_id
    const owner = application.owner.session.access_token
    const project = application.owner.session.personal_project_id
    const privateWorkspace = (await serverRequest(origin, '/workspaces', { token: owner, tenantId: tenant, body: { name: 'Private human workspace', project_id: project, placement: 'cloud' } })).workspace
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
    page = await browser.newPage({ viewport: { width: 1440, height: 980 } })
    page.on('pageerror', error => report.errors.push(error.message))
    page.on('console', event => { if (event.type() === 'error') report.errors.push(event.text()) })
    page.on('response', response => {
      const url = new URL(response.url())
      if (url.origin !== origin) return
      report.network.push({ method: response.request().method(), path: url.pathname, status: response.status() })
      if (response.status() >= 400) report.errors.push(`HTTP ${response.status()} ${url.pathname}`)
    })
    await page.goto(origin)
    await page.getByLabel('用户名', { exact: true }).fill(application.owner.username)
    await page.getByLabel('密码', { exact: true }).fill(application.owner.password)
    await page.getByRole('button', { name: '登录', exact: true }).click()
    await page.getByRole('button', { name: '空间管理', exact: true }).click()
    await page.getByRole('tab', { name: '服务账号', exact: true }).click()
    const panel = page.locator('[data-service-accounts]')
    await panel.getByText('还没有服务账号。', { exact: true }).waitFor()
    assert.equal(report.network.filter(row => row.path.endsWith('/credentials')).length, 0, 'credentials are loaded on demand')
    await panel.getByRole('button', { name: '创建服务账号', exact: true }).click()
    let editor = page.locator('[data-service-account-editor]')
    await editor.getByLabel('服务账号名称', { exact: true }).fill('自动化报告')
    await editor.getByLabel('备注', { exact: true }).fill('此任务使用独立账号')
    await editor.getByRole('button', { name: '创建服务账号', exact: true }).click()
    await editor.getByLabel('凭据名称', { exact: true }).waitFor()
    const account = (await serverRequest(origin, `/tenants/${tenant}/service-accounts`, { token: owner })).service_accounts[0]
    const id = account.service_account_id
    assert.match(id, /^ter_sa_/)
    assert.notEqual(id, application.owner.session.user.user_id)
    assert.equal(report.network.filter(row => row.method === 'GET' && row.path.endsWith('/credentials')).length, 1)
    await editor.getByLabel('凭据名称', { exact: true }).fill('报告读取')
    await editor.getByRole('button', { name: '创建访问凭据', exact: true }).click()
    await editor.getByLabel('新访问凭据', { exact: true }).waitFor()
    const readToken = await editor.getByLabel('新访问凭据', { exact: true }).inputValue()
    assert.match(readToken, /^ter_t_/)
    assert.equal((await serverRequest(origin, '/me', { token: readToken, tenantId: tenant })).user_id, id)
    assert.equal(await status(origin, '/auth/session', readToken, tenant), 403)
    assert.equal(await status(origin, '/me', readToken, 'another-tenant'), 401)
    assert.equal(await status(origin, '/workspaces', readToken, tenant, 'POST', {}), 403)
    assert.deepEqual((await serverRequest(origin, '/workspaces', { token: readToken, tenantId: tenant })).workspaces, [], 'creator’s private workspace is not inherited')
    report.checks.push('on-demand credentials, independent identity, read scope and space isolation')
    await editor.getByLabel('凭据名称', { exact: true }).fill('任务执行')
    await editor.getByRole('checkbox', { name: '提交与停止任务', exact: true }).check()
    const issued = page.waitForResponse(response => response.request().method() === 'POST' && new URL(response.url()).pathname.endsWith('/credentials'))
    await editor.getByRole('button', { name: '创建访问凭据', exact: true }).click()
    assert.equal((await issued).status(), 201)
    const executeToken = await editor.getByLabel('新访问凭据', { exact: true }).inputValue()
    assert.notEqual(executeToken, readToken)
    const workspace = (await serverRequest(origin, '/workspaces', { token: executeToken, tenantId: tenant, body: { name: 'Automation workspace', project_id: project, placement: 'cloud' } })).workspace
    assert.equal(workspace.owner_user_id, id)
    const session = await serverRequest(origin, '/sessions', { token: executeToken, tenantId: tenant, body: { workspace_id: workspace.workspace_id } })
    assert.equal(session.identity.user_id, id)
    assert.equal((await serverRequest(origin, `/sessions/${session.identity.session_id}/history?limit=100`, { token: executeToken, tenantId: tenant })).next_before_seq, null)
    assert.equal(await status(origin, `/workspaces/${privateWorkspace.workspace_id}`, executeToken, tenant), 403)
    const sdkEnvironment = { ...process.env, TERNILO_SDK_TEST_CONFIG: JSON.stringify({ origin, token: executeToken, tenant, service: id, session: session.identity.session_id }) }
    const typescript = await execute(process.execPath, ['--experimental-strip-types', path.join(repository, 'sdk/typescript/test/service-http-smoke.ts')], { cwd: repository, env: sdkEnvironment })
    assert.match(typescript.stdout, /TypeScript service HTTP verified/)
    const python = await execute('python3', [path.join(repository, 'sdk/python/tests/service_http_smoke.py')], { cwd: repository, env: { ...sdkEnvironment, PYTHONPATH: path.join(repository, 'sdk/python/src') } })
    assert.match(python.stdout, /Python service HTTP verified/)
    report.checks.push('TypeScript and Python SDK HTTP identity, history, resource isolation and management denial')
    report.checks.push('execute scope creates resources and session with the service identity while human resources remain private')
    await editor.getByRole('checkbox', { name: '允许服务账号访问', exact: true }).uncheck()
    await editor.getByRole('button', { name: '保存', exact: true }).click()
    await editor.getByRole('button', { name: '创建访问凭据', exact: true }).waitFor({ state: 'visible' })
    await page.waitForFunction(() => document.querySelector('[data-service-account-editor] [data-service-credentials] button[type="submit"]')?.disabled === true)
    assert.equal(await status(origin, '/me', readToken, tenant), 401)
    assert.equal(await status(origin, '/me', executeToken, tenant), 401)
    await editor.getByRole('checkbox', { name: '允许服务账号访问', exact: true }).check()
    const saved = page.waitForResponse(response => response.request().method() === 'PATCH' && new URL(response.url()).pathname.endsWith(id))
    await editor.getByRole('button', { name: '保存', exact: true }).click()
    assert.equal((await saved).status(), 200)
    assert.equal(await status(origin, '/me', readToken, tenant), 200)
    assert.equal(await status(origin, '/me', executeToken, tenant), 200)
    await editor.getByRole('button', { name: '关闭', exact: true }).first().click()
    await editor.waitFor({ state: 'detached' })
    await panel.locator(`[data-service-account="${id}"]`).getByRole('button', { name: '详情与凭据', exact: true }).click()
    editor = page.locator('[data-service-account-editor]')
    await editor.locator('[data-service-credential]').first().waitFor()
    assert.equal(await editor.locator('[data-service-token]').count(), 0, 'closing details clears the one-time token')
    for (const width of [1440, 390, 320]) {
      await page.setViewportSize({ width, height: 980 })
      assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), 'page fits viewport')
      assert.ok(await editor.evaluate(element => element.scrollWidth <= element.clientWidth + 1), 'credential editor fits viewport')
      if (artifacts) await page.screenshot({ path: path.join(artifacts, `service-accounts-${width}.png`), fullPage: true })
    }
    const credentials = (await serverRequest(origin, `/tenants/${tenant}/service-accounts/${id}/credentials`, { token: owner })).credentials
    const readCredential = credentials.find(credential => credential.name === '报告读取')
    assert.equal(credentials.length, 2)
    assert.ok(!JSON.stringify(credentials).includes(readToken))
    const row = editor.locator(`[data-service-credential="${readCredential.credential_id}"]`)
    await row.getByRole('button', { name: '撤销凭据', exact: true }).click()
    await page.locator('[data-settings-dialog]').getByRole('button', { name: '撤销凭据', exact: true }).click()
    await row.getByText('已撤销', { exact: true }).waitFor()
    assert.equal(await status(origin, '/me', readToken, tenant), 401)
    assert.equal(await status(origin, '/me', executeToken, tenant), 200)
    assert.equal(await status(origin, '/auth/session', owner, tenant), 200)
    report.checks.push('disable/re-enable, one-time token removal, three viewport layouts and individual revocation')
    await editor.getByRole('button', { name: '关闭', exact: true }).first().click()
    await editor.waitFor({ state: 'detached' })
    await page.evaluate(() => localStorage.setItem('ternilo.locale', 'en'))
    await page.reload()
    await page.getByRole('tab', { name: 'Service accounts', exact: true }).click()
    await page.locator(`[data-service-account="${id}"]`).getByRole('button', { name: 'Details and credentials', exact: true }).click()
    editor = page.locator('[data-service-account-editor]')
    await editor.getByLabel('Credential name', { exact: true }).waitFor()
    await editor.getByRole('checkbox', { name: 'Read resources', exact: true }).waitFor()
    assert.equal(await editor.locator('[data-service-token]').count(), 0)
    assert.ok(await editor.evaluate(element => element.scrollWidth <= element.clientWidth + 1))
    if (artifacts) await page.screenshot({ path: path.join(artifacts, 'service-accounts-320-english.png'), fullPage: true })
    report.checks.push('English mobile details, scopes and one-time credential state after navigation')
    assert.deepEqual(report.errors, [])
    report.status = 'passed'
  } catch (error) {
    report.status = 'failed'
    if (artifacts && page) await page.screenshot({ path: path.join(artifacts, 'service-accounts-failure.png'), fullPage: true }).catch(() => {})
    throw new Error(`${error.stack}\n${application?.diagnostics() ?? ''}`)
  } finally {
    if (artifacts) await writeFile(path.join(artifacts, 'result.json'), `${JSON.stringify(report, null, 2)}\n`)
    await browser?.close()
    await stopProcess(application)
    await rm(directory, { recursive: true, force: true })
  }
})
