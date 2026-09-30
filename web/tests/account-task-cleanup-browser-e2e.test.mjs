import assert from 'node:assert/strict'
import { mkdir, mkdtemp, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { chromium } from 'playwright'
import { freePort, initializeServer, serverRequest, stopProcess } from './platform-e2e-fixture.mjs'

async function queueTasks(origin, ownerToken, name) {
  await serverRequest(origin, '/auth/register', { body: { username: name, email: `${name}@example.test`, password: 'cleanup-browser-password' } })
  const account = await serverRequest(origin, '/auth/login', { body: { username: name, password: 'cleanup-browser-password' } })
  const token = account.access_token
  const tenant = account.personal_tenant_id
  const { projects } = await serverRequest(origin, `/tenants/${tenant}/projects`, { token })
  const { workspace } = await serverRequest(origin, `/tenants/${tenant}/workspaces`, { token, body: { project_id: projects[0].project_id, name: `${name} workspace`, placement: 'cloud' } })
  const runs = []
  for (const suffix of ['first', 'second']) {
    const { run } = await serverRequest(origin, `/tenants/${tenant}/runs`, { token, body: {
      project_id: projects[0].project_id, workspace_id: workspace.workspace_id,
      agent_id: 'cleanup-browser-agent', session_id: `${name}-session`, run_id: `${name}-${suffix}`,
      limits: { max_steps: 8, max_tool_calls: 32 }, permissions: 'workspace_write', mode: 'execute',
      profile: { plugins: [] }, input: `Queued task ${suffix}`, attachments: [], reserved_model_tokens: 100,
    } })
    assert.equal(run.state, 'queued')
    runs.push(run.run_id)
  }
  const { accounts } = await serverRequest(origin, `/admin/accounts?query=${name}`, { token: ownerToken })
  assert.equal(accounts.length, 1)
  return { name, account: accounts[0], tenant, token, runs }
}

async function changeStatus(page, origin, user, action) {
  const labels = { ban: '封禁账号', unban: '解除封禁', remove: '注销账号' }
  const titles = { ban: '封禁这个账号？', unban: '解除账号封禁？', remove: '永久注销这个账号？' }
  const row = page.locator(`[data-admin-account="${user.account.user_id}"]`)
  await row.getByRole('button', { name: `账号“${user.name}”的操作`, exact: true }).click()
  await page.getByRole('menuitem', { name: labels[action], exact: true }).click()
  const dialog = page.getByRole('dialog', { name: titles[action], exact: true })
  if (action === 'unban') await dialog.getByText(/已取消的任务不会自动恢复/).waitFor()
  else await dialog.getByText(/托管任务和电脑任务将取消或请求停止，电脑收尾以实际确认结果为准/).waitFor()
  const response = page.waitForResponse(response => response.request().method() === 'POST'
    && new URL(response.url()).pathname === `/api/v1/admin/accounts/${user.account.user_id}/status`)
  await dialog.getByRole('button', { name: labels[action], exact: true }).click()
  assert.equal((await response).status(), 200)
  await dialog.waitFor({ state: 'hidden' })
  await row.locator(`[data-account-status="${{ ban: 'banned', unban: 'active', remove: 'removed' }[action]}"]`).waitFor()
}

async function runStates(origin, user) {
  const { runs } = await serverRequest(origin, `/tenants/${user.tenant}/runs`, { token: user.token })
  return runs.filter(run => user.runs.includes(run.run_id)).map(run => run.state)
}

test('account administration cancels queued managed tasks without reviving them or cancelling another author', { timeout: 120_000 }, async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-account-task-cleanup-'))
  const artifacts = process.env.TERNILO_E2E_ARTIFACT_DIR ?? directory
  await mkdir(artifacts, { recursive: true })
  let application, browser, page
  const errors = []
  try {
    const origin = `http://127.0.0.1:${await freePort()}`
    application = await initializeServer({ directory: path.join(directory, 'server'), origin, managedExecutionEnabled: true })
    const ownerToken = application.owner.session.access_token
    const registration = await serverRequest(origin, '/admin/registration', { token: ownerToken })
    await serverRequest(origin, '/admin/registration', { token: ownerToken, method: 'PATCH', body: { mode: 'open', require_approval: false, revision: registration.revision } })
    const alice = await queueTasks(origin, ownerToken, 'cleanup-alice')
    const bob = await queueTasks(origin, ownerToken, 'cleanup-bob')
    browser = await chromium.launch({ headless: true })
    page = await browser.newPage({ viewport: { width: 1280, height: 900 } })
    page.on('pageerror', error => errors.push(error.message))
    page.on('console', message => { if (message.type() === 'error') errors.push(message.text()) })
    page.on('response', response => { if (response.status() >= 400) errors.push(`HTTP ${response.status()}: ${new URL(response.url()).pathname}`) })
    await page.goto(`${origin}/admin/accounts`)
    await page.getByLabel('用户名', { exact: true }).fill(application.owner.username)
    await page.getByLabel('密码', { exact: true }).fill(application.owner.password)
    await page.getByRole('button', { name: '登录', exact: true }).click()
    await page.locator(`[data-admin-account="${alice.account.user_id}"]`).waitFor()
    await changeStatus(page, origin, alice, 'ban')
    assert.deepEqual(await runStates(origin, bob), ['queued', 'queued'])
    await changeStatus(page, origin, alice, 'unban')
    alice.token = (await serverRequest(origin, '/auth/login', { body: { username: alice.name, password: 'cleanup-browser-password' } })).access_token
    assert.deepEqual(await runStates(origin, alice), ['cancelled', 'cancelled'])
    await page.setViewportSize({ width: 390, height: 844 })
    await changeStatus(page, origin, alice, 'remove')
    assert.deepEqual(await runStates(origin, bob), ['queued', 'queued'])
    assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth))
    await page.screenshot({ path: path.join(artifacts, 'account-task-cleanup-mobile.png'), animations: 'disabled' })
    assert.deepEqual(errors, [])
    assert.ok(!application.diagnostics().includes('panicked'))
  } finally {
    await browser?.close()
    if (application) await stopProcess(application)
    await rm(directory, { recursive: true, force: true })
  }
})
