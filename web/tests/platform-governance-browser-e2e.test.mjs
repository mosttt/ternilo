import { selectChoice } from './browser-select-fixture.mjs'
import assert from 'node:assert/strict'
import path from 'node:path'
import { mkdir, mkdtemp, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import test from 'node:test'
import { chromium } from 'playwright'

import { selectSpace,
  freePort,
  initializeServer,
  repository,
  registerOidcUser,
  serverRequest,
  startOidcServer,
  startPostgres,
  stopProcess,
  waitForHttp,
} from './platform-e2e-fixture.mjs'

const controlBinary = process.env.TERNILO_E2E_SERVER_BINARY ?? path.join(repository, 'target', 'debug', 'ternilo-server')

async function captureArtifact(page, name) {
  const directory = process.env.TERNILO_E2E_ARTIFACT_DIR
  if (!directory) return
  await mkdir(directory, { recursive: true })
  await page.screenshot({ path: path.join(directory, name), fullPage: true })
}

function observe(page) {
  const pageErrors = []
  const consoleErrors = []
  const failedRequests = []
  const failedResponses = []
  const successfulMutations = new Set()
  page.on('pageerror', error => pageErrors.push(error.message))
  page.on('console', message => {
    if (message.type() === 'error') consoleErrors.push(message.text())
  })
  page.on('requestfailed', request => {
    failedRequests.push({
      request,
      description: `${request.method()} ${request.url()} ${request.failure()?.errorText ?? ''}`,
    })
  })
  page.on('response', response => {
    if (response.status() < 400 && response.request().method() !== 'GET') {
      successfulMutations.add(response.request())
    }
    if (response.status() >= 400) {
      failedResponses.push(`${response.status()} ${response.request().method()} ${response.url()}`)
    }
  })
  return { pageErrors, consoleErrors, failedRequests, failedResponses, successfulMutations }
}

async function createIdentitySpace(browser, origin, oidc, identity, space) {
  oidc.selectIdentity(identity)
  const context = await browser.newContext({ locale: 'zh-CN', viewport: { width: 1440, height: 900 } })
  const page = await context.newPage()
  const observations = observe(page)
  await page.goto(origin, { waitUntil: 'domcontentloaded' })
  await page.getByRole('button', { name: '使用组织账号登录' }).click()
  const spaces = page.getByRole('combobox', { name: '切换空间' })
  await spaces.waitFor({ timeout: 30_000 })
  assert.equal(await page.getByRole('dialog', { name: '创建 Ternilo 空间' }).count(), 0)
  if (space.existing) await spaces.selectOption({ label: `${space.name} · 团队` })
  const token = await page.evaluate(() => sessionStorage.getItem('ternilo.oidc.access'))
  const response = await page.request.get(`${origin}/api/v1/auth/session`, { headers: { authorization: `Bearer ${token}` } })
  assert.equal(response.status(), 200)
  const authenticated = await response.json()
  const tenantId = await page.getByRole('combobox', { name: '切换空间', exact: true }).getAttribute('data-space-id')
  if (!space.existing) assert.equal(tenantId, authenticated.personal_tenant_id)
  return { context, page, observations, tenantId, personalTenantId: authenticated.personal_tenant_id, userId: authenticated.user.user_id }
}

async function openSpaceManagement(page, language = 'zh') {
  await page.getByRole('button', { name: language === 'en' ? 'Space management' : '空间管理', exact: true }).click()
  const management = page.locator('[data-space-management]')
  await management.locator('[data-platform-settings]').waitFor()
  return management
}

async function closeSpaceManagement(page, language = 'zh') {
  await page.getByRole('link', { name: language === 'en' ? 'Back to workbench' : '返回工作台', exact: true }).click()
  await page.locator('[data-space-management]').waitFor({ state: 'detached' })
}

async function addMember(settings, page, userId, role) {
  await settings.getByLabel('用户 ID').fill(userId)
  await selectChoice(settings.getByLabel('角色', { exact: true }), role)
  const response = page.waitForResponse(candidate => (
    candidate.request().method() === 'PUT'
      && new URL(candidate.url()).pathname.endsWith(`/members/${encodeURIComponent(userId)}`)
  ))
  await settings.getByRole('button', { name: '添加或更新成员' }).click()
  assert.equal((await response).status(), 204)
  await settings.locator(`[data-platform-member="${userId}"]`).waitFor()
}

async function assertNoOverflow(page, settings, label) {
  const dimensions = await page.evaluate(() => ({
    viewport: innerWidth,
    document: document.documentElement.scrollWidth,
    body: document.body.scrollWidth,
  }))
  assert.ok(
    dimensions.document <= dimensions.viewport && dimensions.body <= dimensions.viewport,
    `${label} document overflow: ${JSON.stringify(dimensions)}`,
  )
  const settingsDimensions = await settings.evaluate(element => ({
    client: element.clientWidth,
    scroll: element.scrollWidth,
  }))
  assert.ok(
    settingsDimensions.scroll <= settingsDimensions.client,
    `${label} settings overflow: ${JSON.stringify(settingsDimensions)}`,
  )
  const contentDimensions = await settings.locator('[data-platform-settings]').evaluate(element => ({
    client: element.clientWidth,
    scroll: element.scrollWidth,
  }))
  assert.ok(
    contentDimensions.scroll <= contentDimensions.client,
    `${label} content overflow: ${JSON.stringify(contentDimensions)}`,
  )
}

async function seedModelUsage(postgres, { tenantId, ownerUserId, adminUserId, otherTenantId }) {
  assert.match(tenantId, /^ten_/)
  assert.match(ownerUserId, /^usr_/)
  assert.match(adminUserId, /^usr_/)
  assert.match(otherTenantId, /^ten_/)
  const createdAt = Date.UTC(2026, 7, 10)
  const finishedAt = createdAt + 30_000

  const sql = value => value === null ? 'NULL' : typeof value === 'number' ? String(value) : `'${String(value).replaceAll("'", "''")}'`
  const insert = (table, columns, rows) => `INSERT INTO ${table} (${columns.join(',')}) VALUES ${rows.map(row => `(${row.map(sql).join(',')})`).join(',')};`
  const fixtures = [
    { prefix: 'usage-browser', suffix: 'owner', tenant: tenantId, owner: ownerUserId, actor: ownerUserId, lease: 7, input: 100, output: 20, cached: 40, provider: 'browser-provider', model: 'browser-model-owner', upstream: 'provider-browser-owner' },
    { prefix: 'usage-browser', suffix: 'admin', tenant: tenantId, owner: ownerUserId, actor: adminUserId, lease: 8, input: 50, output: 10, cached: 10, provider: 'browser-provider', model: 'browser-model-admin', upstream: 'provider-browser-admin', unknown: 80 },
    { prefix: 'usage-leak', suffix: 'other', tenant: otherTenantId, owner: adminUserId, actor: adminUserId, lease: 10, input: 900, output: 99, cached: 0, provider: 'leak-provider', model: 'leak-model', upstream: 'provider-leak' },
  ]
  const statements = ['BEGIN;', insert('control_projects', ['tenant_id', 'project_id', 'name', 'created_by', 'created_at_ms'], [
    [tenantId, 'usage-browser-project', 'Usage browser project', ownerUserId, createdAt],
    [otherTenantId, 'usage-leak-project', 'Usage leak project', adminUserId, createdAt],
  ]), insert('control_workspaces', ['tenant_id', 'workspace_id', 'project_id', 'owner_user_id', 'name', 'placement', 'storage', 'created_at_ms', 'updated_at_ms'], [
    [tenantId, 'usage-browser-workspace', 'usage-browser-project', ownerUserId, 'Usage browser workspace', 'cloud', 'cloud_volume', createdAt, createdAt],
    [otherTenantId, 'usage-leak-workspace', 'usage-leak-project', adminUserId, 'Usage leak workspace', 'cloud', 'cloud_volume', createdAt, createdAt],
  ])]
  const reservationColumns = ['tenant_id', 'reservation_id', 'user_id', 'run_id', 'period_start', 'reserved_model_tokens', 'state', 'created_at_ms', 'expires_at_ms', 'committed_model_tokens', 'unknown_model_tokens']
  const runColumns = ['tenant_id', 'run_id', 'user_id', 'actor_user_id', 'authorization_session_id', 'project_id', 'workspace_id', 'agent_id', 'session_id', 'spec', 'spec_digest', 'quota_reservation_id', 'state', 'available_at_ms', 'created_at_ms', 'updated_at_ms', 'lease_token', 'finished_at_ms']
  const attemptColumns = ['request_id', 'attempt', 'state', 'attempted', 'reserved_tokens', 'accounted_tokens', 'usage_json', 'input_tokens', 'output_tokens', 'cached_input_tokens', 'cache_write_tokens', 'reasoning_tokens', 'upstream_request_id', 'created_at_ms', 'settled_at_ms']
  for (const fixture of fixtures) {
    const id = `${fixture.prefix}-${fixture.suffix}`, run = `${fixture.prefix}-run-${fixture.suffix}`, session = `${fixture.prefix}-session-${fixture.suffix}`
    const reservation = `${fixture.prefix}-reservation-${fixture.suffix}`
    const principal = { tenant_id: fixture.tenant, project_id: `${fixture.prefix}-project`, workspace_id: `${fixture.prefix}-workspace`, session_id: session, authorization_session_id: session, run_id: run, actor_user_id: fixture.actor, resource_owner_user_id: fixture.owner, execution_owner_user_id: fixture.owner, execution_reservation_id: reservation, worker_id: 'usage-fixture-worker', worker_generation: 1, lease_token: fixture.lease, writer_fencing_token: 1, model: { source: 'user_provider', tenant_id: fixture.tenant, owner_user_id: fixture.owner, provider_id: fixture.provider, model: fixture.model }, run_token_limit: 1000 }
    statements.push(insert('control_quota_reservations', reservationColumns, [[fixture.tenant, reservation, fixture.owner, run, '2026-08-01', 1000, 'committed', createdAt, createdAt + 60_000, fixture.input + fixture.output, fixture.unknown ?? 0]]))
    statements.push(insert('cloud_runs', runColumns, [[fixture.tenant, run, fixture.owner, fixture.actor, session, `${fixture.prefix}-project`, `${fixture.prefix}-workspace`, 'agent', session, '{}', `\\x${'07'.repeat(32)}`, reservation, 'succeeded', createdAt, createdAt, finishedAt, fixture.lease, finishedAt]]))
    statements.push(insert('control_model_requests', ['request_id', 'origin', 'source', 'caller_scope', 'request_key', 'payload_hash', 'actor_user_id', 'resource_owner_user_id', 'model_beneficiary_user_id', 'workload_json', 'tenant_id', 'project_id', 'session_id', 'run_id', 'execution_reservation_id', 'budget_period_start', 'model_id', 'provider_id', 'upstream_model', 'protocol', 'route_snapshot_json', 'max_attempts', 'state', 'month', 'created_at_ms', 'expires_at_ms', 'settled_at_ms'], [[id, 'workload', 'user_provider', run, id, 'fixture-payload', fixture.actor, fixture.owner, fixture.owner, JSON.stringify(principal), fixture.tenant, principal.project_id, session, run, reservation, '2026-08-01', fixture.model, fixture.provider, fixture.model, 'openai-chat-completions', '{}', 2, 'completed', '2026-08', createdAt, createdAt + 60_000, finishedAt]]))
    const usage = { input_tokens: fixture.input, output_tokens: fixture.output, cached_input_tokens: fixture.cached, cache_write_tokens: 0, reasoning_tokens: 2 }
    statements.push(insert('control_model_attempts', attemptColumns, [[id, 1, 'completed', 1, 1000, fixture.input + fixture.output, JSON.stringify(usage), fixture.input, fixture.output, fixture.cached, 0, 2, fixture.upstream, createdAt, finishedAt]]))
    if (fixture.unknown) statements.push(insert('control_model_attempts', attemptColumns, [[id, 2, 'failed', 1, fixture.unknown, null, null, null, null, null, null, null, 'provider-browser-unknown', createdAt, finishedAt]]))
  }
  statements.push(insert('control_quota_reservations', reservationColumns, [[tenantId, 'usage-browser-reservation-stale', adminUserId, 'usage-browser-run-stale', '2026-08-01', 250, 'active', createdAt + 2, createdAt + 3, null, 0]]))
  statements.push(insert('cloud_runs', runColumns, [[tenantId, 'usage-browser-run-stale', adminUserId, adminUserId, 'usage-browser-session-stale', 'usage-browser-project', 'usage-browser-workspace', 'agent', 'usage-browser-session-stale', '{}', `\\x${'09'.repeat(32)}`, 'usage-browser-reservation-stale', 'failed', createdAt + 2, createdAt + 2, finishedAt + 2, 9, finishedAt + 2]]))
  statements.push(`INSERT INTO control_quota_usage (tenant_id,period_start,used_model_tokens,unknown_model_tokens) VALUES (${sql(tenantId)},'2026-08-01',180,80) ON CONFLICT (tenant_id,period_start) DO UPDATE SET used_model_tokens=EXCLUDED.used_model_tokens,unknown_model_tokens=EXCLUDED.unknown_model_tokens;`, 'COMMIT;')
  await postgres.query(statements.join('\n'))
  return { tenantId, ownerUserId, adminUserId }
}

function assertClean(label, observations) {
  const failedRequests = observations.failedRequests.filter(entry => {
    return entry.request.failure()?.errorText !== 'net::ERR_ABORTED'
      || !observations.successfulMutations.has(entry.request)
  }).map(entry => entry.description)
  assert.deepEqual({
    pageErrors: observations.pageErrors,
    consoleErrors: observations.consoleErrors,
    failedRequests,
    failedResponses: observations.failedResponses,
  }, {
    pageErrors: [],
    consoleErrors: [],
    failedRequests: [],
    failedResponses: [],
  }, `${label} browser diagnostics`)
}

test('platform quota, usage, and audit form a real owner/admin/member browser flow', { timeout: 240_000 }, async () => {
  const postgres = await startPostgres({
    prefix: 'ternilo-platform-governance-browser',
    database: 'ternilo_platform_governance_browser_test',
  })
  const oidc = await startOidcServer({
    audience: 'ternilo-platform-governance-e2e',
    identities: {
      owner: {
        subject: 'platform-governance-owner',
        email: 'governance-owner@example.com',
        name: 'Governance Owner',
      },
      admin: {
        subject: 'platform-governance-admin',
        email: 'governance-admin@example.com',
        name: 'Governance Admin',
      },
      member: {
        subject: 'platform-governance-member',
        email: 'governance-member@example.com',
        name: 'Governance Member',
      },
    },
    initialIdentity: 'owner',
  })
  const port = await freePort()
  const origin = `http://127.0.0.1:${port}`
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-platform-governance-'))
  let control, browser
  const contexts = []
  try {
    control = await initializeServer({
      directory, origin, binary: controlBinary,
      databaseUrl: postgres.url, migrationDatabaseUrl: postgres.url,
      oidc: { issuer: oidc.issuer, audience: 'ternilo-platform-governance-e2e', client_id: 'ternilo-platform-governance-browser', allow_insecure: true },
      ownerOidcToken: oidc.accessToken('owner'), mode: 'multi_user',
    })
    const registrationPolicy = await serverRequest(origin, '/admin/registration', { token: control.owner.session.access_token })
    await serverRequest(origin, '/admin/registration', { token: control.owner.session.access_token, method: 'PATCH', body: { mode: 'open', require_approval: false, revision: registrationPolicy.revision } })
    for (const account of ['admin', 'member']) await registerOidcUser(origin, oidc.accessToken(account), `governance-${account}`)
    await waitForHttp(`${origin}/health`, control)
    const created = await fetch(`${origin}/api/v1/tenants`, {
      method: 'POST',
      headers: { authorization: `Bearer ${control.owner.session.access_token}`, 'content-type': 'application/json' },
      body: JSON.stringify({ slug: 'governance-e2e', display_name: 'Governance E2E' }),
    })
    assert.equal(created.status, 201)
    browser = await chromium.launch({ headless: true })

    const owner = await createIdentitySpace(browser, origin, oidc, 'owner', {
      name: 'Governance E2E', slug: 'governance-e2e', existing: true,
    })
    contexts.push(owner.context)
    const admin = await createIdentitySpace(browser, origin, oidc, 'admin', {
      name: 'Admin Bootstrap', slug: 'governance-admin-bootstrap',
    })
    contexts.push(admin.context)
    const adminUserId = admin.userId
    const member = await createIdentitySpace(browser, origin, oidc, 'member', {
      name: 'Member Bootstrap', slug: 'governance-member-bootstrap',
    })
    contexts.push(member.context)
    const memberUserId = member.userId
    oidc.selectIdentity('owner')

    let ownerSettings = await openSpaceManagement(owner.page)
    await addMember(ownerSettings, owner.page, adminUserId, 'admin')
    await addMember(ownerSettings, owner.page, memberUserId, 'member')

    await ownerSettings.getByRole('tab', { name: '配额', exact: true }).click()
    const ownerQuota = ownerSettings.locator('[data-platform-quota][data-platform-list-state="ready"]')
    await ownerQuota.waitFor()
    await ownerSettings.getByRole('heading', { name: '空间配额' }).waitFor()
    const expectedQuota = {
      max_nodes: '17',
      max_concurrent_runs: '9',
      monthly_model_tokens: '765432',
      max_secrets: '23',
    }
    for (const [field, value] of Object.entries(expectedQuota)) {
      await ownerQuota.locator(`#platform-quota-${field}`).fill(value)
    }
    const quotaUpdate = owner.page.waitForResponse(response => (
      response.request().method() === 'PUT'
        && new URL(response.url()).pathname.endsWith('/quota')
    ))
    await ownerQuota.getByRole('button', { name: '保存配额' }).click()
    assert.equal((await quotaUpdate).status(), 204)
    await ownerQuota.getByText('空间配额已保存', { exact: true }).waitFor()
    await assertNoOverflow(owner.page, ownerSettings, 'owner desktop quota')
    assert.equal(owner.userId, control.owner.session.user.user_id, 'OIDC retains the initialized native owner')
    assert.equal(admin.userId, adminUserId)
    assert.equal(member.userId, memberUserId)
    const usageFixture = await seedModelUsage(postgres, {
      tenantId: owner.tenantId, ownerUserId: owner.userId, adminUserId, otherTenantId: admin.tenantId,
    })
    await closeSpaceManagement(owner.page)

    await owner.page.reload({ waitUntil: 'domcontentloaded' })
    await selectSpace(owner.page, owner.tenantId)
    ownerSettings = await openSpaceManagement(owner.page)
    await ownerSettings.getByRole('tab', { name: '配额', exact: true }).click()
    const persistedQuota = ownerSettings.locator('[data-platform-quota][data-platform-list-state="ready"]')
    await persistedQuota.waitFor()
    for (const [field, value] of Object.entries(expectedQuota)) {
      assert.equal(await persistedQuota.locator(`#platform-quota-${field}`).inputValue(), value)
    }

    const ownerUsageResponse = owner.page.waitForResponse(response => {
      const url = new URL(response.url())
      return response.request().method() === 'GET'
        && url.pathname.endsWith('/model-usage')
        && url.searchParams.get('period') === '2026-08'
    })
    await ownerSettings.getByRole('tab', { name: '用量', exact: true }).click()
    await ownerSettings.getByLabel('UTC 月份').fill('2026-08')
    assert.equal((await ownerUsageResponse).status(), 200)
    const ownerUsage = ownerSettings.locator('[data-platform-usage][data-platform-list-state="ready"]')
    await ownerUsage.waitFor()
    await ownerUsage.getByText(usageFixture.tenantId, { exact: true }).waitFor()
    await ownerUsage.getByText('2026-08', { exact: true }).waitFor()
    await ownerUsage.getByText(usageFixture.ownerUserId, { exact: true }).first().waitFor()
    await ownerUsage.getByText(usageFixture.adminUserId, { exact: true }).first().waitFor()
    await ownerUsage.locator('[data-platform-usage-ledger="usage-browser-owner:1"]').waitFor()
    await ownerUsage.locator('[data-platform-usage-ledger="usage-browser-admin:1"]').waitFor()
    const unknownAttempt = ownerUsage.locator('[data-platform-usage-ledger="usage-browser-admin:2"]')
    await unknownAttempt.waitFor()
    assert.match(await unknownAttempt.textContent(), /尚未上报/)
    assert.match(await ownerUsage.textContent(), /调用预留（用量待确认）/)
    assert.match(await ownerUsage.textContent(), /其中推理（已包含在输出）/)
    assert.match(await ownerUsage.textContent(), /资源所有者/)
    await ownerUsage.locator('[data-platform-usage-anomaly="usage-browser-reservation-stale"]').waitFor()
    assert.equal((await ownerUsage.textContent() ?? '').includes('leak-provider'), false)
    assert.match(await ownerUsage.textContent() ?? '', /其中缓存输入（已包含在输入）/)
    assert.match(await ownerUsage.textContent() ?? '', /全时段异常未结算项/)
    await assertNoOverflow(owner.page, ownerSettings, 'owner desktop usage')
    await captureArtifact(owner.page, 'governance-usage-desktop.png')

    await ownerSettings.getByRole('tab', { name: '审计', exact: true }).click()
    const ownerAudit = ownerSettings.locator('[data-platform-audit][data-platform-list-state="ready"]')
    await ownerAudit.waitFor()
    await ownerSettings.getByRole('heading', { name: '审计记录' }).waitFor()
    const quotaEntry = ownerAudit.locator('[data-platform-audit-entry]').filter({ hasText: 'quota.update' })
    await quotaEntry.waitFor()
    assert.match(await quotaEntry.textContent() ?? '', /765432/)
    const membershipEntries = ownerAudit.locator('[data-platform-audit-entry]').filter({ hasText: 'membership.set' })
    assert.equal(await membershipEntries.count(), 2)
    assert.equal((await ownerAudit.textContent() ?? '').includes(adminUserId), true)
    assert.equal((await ownerAudit.textContent() ?? '').includes(memberUserId), true)
    await assertNoOverflow(owner.page, ownerSettings, 'owner desktop audit')

    await closeSpaceManagement(owner.page)
    await owner.page.getByRole('button', { name: '用户设置', exact: true }).click()
    const ownerPreferences = owner.page.locator('[data-user-settings]')
    await ownerPreferences.getByRole('button', { name: '通用', exact: true }).click()
    await selectChoice(ownerPreferences.getByLabel('语言'), 'en')
    await owner.page.locator('[data-user-settings]').getByRole('button', { name: 'Back to workbench' }).click()
    ownerSettings = await openSpaceManagement(owner.page, 'en')
    await ownerSettings.getByRole('tab', { name: 'Quota', exact: true }).click()
    await ownerSettings.getByRole('heading', { name: 'Space quota' }).waitFor()
    await ownerSettings.getByRole('tab', { name: 'Audit', exact: true }).click()
    await ownerSettings.getByRole('heading', { name: 'Audit records' }).waitFor()
    await owner.page.setViewportSize({ width: 390, height: 844 })
    await assertNoOverflow(owner.page, ownerSettings, 'owner English mobile audit')
    const ownerMobileUsageResponse = owner.page.waitForResponse(response => {
      const url = new URL(response.url())
      return response.request().method() === 'GET'
        && url.pathname.endsWith('/model-usage')
        && url.searchParams.get('period') === '2026-08'
    })
    await ownerSettings.getByRole('tab', { name: 'Usage', exact: true }).click()
    await ownerSettings.getByLabel('UTC month').fill('2026-08')
    assert.equal((await ownerMobileUsageResponse).status(), 200)
    const ownerMobileUsage = ownerSettings.locator('[data-platform-usage][data-platform-list-state="ready"]')
    await ownerMobileUsage.waitFor()
    await ownerMobileUsage.getByRole('heading', { name: 'Model usage & settlement' }).waitFor()
    assert.match(await ownerMobileUsage.textContent() ?? '', /Cached input \(included in input\)/)
    const refreshBox = await ownerMobileUsage.getByRole('button', { name: 'Refresh', exact: true }).boundingBox()
    assert.ok(refreshBox && refreshBox.height >= 40, `mobile Usage refresh target: ${JSON.stringify(refreshBox)}`)
    const ledgerRegion = ownerMobileUsage.locator('[data-platform-usage-table="ledger"]')
    const ledgerDimensions = await ledgerRegion.evaluate(element => ({
      client: element.clientWidth,
      scroll: element.scrollWidth,
      tabIndex: element.tabIndex,
    }))
    assert.equal(ledgerDimensions.tabIndex, 0)
    assert.ok(ledgerDimensions.scroll > ledgerDimensions.client, `mobile ledger is not horizontally scrollable: ${JSON.stringify(ledgerDimensions)}`)
    await ledgerRegion.focus()
    assert.equal(await ledgerRegion.evaluate(element => document.activeElement === element), true)
    const ledgerScrollLeft = await ledgerRegion.evaluate(element => {
      element.scrollLeft = element.scrollWidth
      return element.scrollLeft
    })
    assert.ok(ledgerScrollLeft > 0)
    await assertNoOverflow(owner.page, ownerSettings, 'owner English mobile usage')
    await ledgerRegion.evaluate(element => { element.scrollLeft = 0 })
    await captureArtifact(owner.page, 'governance-usage-mobile.png')

    await admin.page.reload({ waitUntil: 'domcontentloaded' })
    await selectSpace(admin.page, owner.tenantId)
    assert.equal(await admin.page.getByRole('button', { name: '平台管理', exact: true }).count(), 0, 'team administration does not grant platform administration')
    const adminSettings = await openSpaceManagement(admin.page)
    await adminSettings.getByRole('tab', { name: '配额', exact: true }).click()
    const adminQuota = adminSettings.locator('[data-platform-quota][data-platform-list-state="ready"]')
    await adminQuota.waitFor()
    for (const [field, value] of Object.entries(expectedQuota)) {
      const input = adminQuota.locator(`#platform-quota-${field}`)
      assert.equal(await input.inputValue(), value)
      assert.equal(await input.getAttribute('readonly'), '')
    }
    assert.equal(await adminQuota.getByRole('button', { name: '保存配额' }).count(), 0)
    const adminUsageResponse = admin.page.waitForResponse(response => {
      const url = new URL(response.url())
      return response.request().method() === 'GET'
        && url.pathname.endsWith('/model-usage')
        && url.searchParams.get('period') === '2026-08'
    })
    await adminSettings.getByRole('tab', { name: '用量', exact: true }).click()
    await adminSettings.getByLabel('UTC 月份').fill('2026-08')
    assert.equal((await adminUsageResponse).status(), 200)
    const adminUsage = adminSettings.locator('[data-platform-usage][data-platform-list-state="ready"]')
    await adminUsage.waitFor()
    await adminUsage.getByText(usageFixture.ownerUserId, { exact: true }).first().waitFor()
    await adminUsage.getByText(usageFixture.adminUserId, { exact: true }).first().waitFor()
    await adminUsage.locator('[data-platform-usage-ledger="usage-browser-owner:1"]').waitFor()
    assert.equal((await adminUsage.textContent() ?? '').includes('leak-provider'), false)
    await adminSettings.getByRole('tab', { name: '审计', exact: true }).click()
    const adminAudit = adminSettings.locator('[data-platform-audit][data-platform-list-state="ready"]')
    await adminAudit.waitFor()
    await adminAudit.getByText('quota.update', { exact: true }).waitFor()
    await adminAudit.getByText('membership.set', { exact: true }).first().waitFor()
    await assertNoOverflow(admin.page, adminSettings, 'admin desktop governance')

    await member.page.reload({ waitUntil: 'domcontentloaded' })
    await selectSpace(member.page, owner.tenantId)
    assert.equal(await member.page.getByRole('button', { name: '空间管理', exact: true }).count(), 0)
    assert.equal(await member.page.getByRole('button', { name: '平台管理', exact: true }).count(), 0)
    await member.page.getByRole('button', { name: '用户设置', exact: true }).click()
    const memberSettings = member.page.locator('[data-user-settings]')
    await memberSettings.waitFor()
    assert.equal(await memberSettings.getByRole('button', { name: '平台管理', exact: true }).count(), 0)
    assert.equal(await memberSettings.getByText('空间配额', { exact: true }).count(), 0)
    assert.equal(await memberSettings.getByText('审计记录', { exact: true }).count(), 0)

    await closeSpaceManagement(admin.page)
    await admin.page.getByRole('button', { name: '用户设置', exact: true }).click()
    const adminPreferences = admin.page.locator('[data-user-settings]')
    await adminPreferences.getByRole('button', { name: '通用', exact: true }).click()
    await selectChoice(adminPreferences.getByLabel('语言'), 'en')
    const adminSettingsEnglish = admin.page.locator('[data-user-settings]')
    await adminSettingsEnglish.getByRole('button', { name: 'Back to workbench' }).click()
    await adminSettingsEnglish.waitFor({ state: 'detached' })
    await admin.page.setViewportSize({ width: 390, height: 844 })
    assert.equal(await admin.page.getByRole('combobox', { name: 'Switch space' }).inputValue(), owner.tenantId)
    const staleTenantSnapshot = 'logout-tenant-sentinel'
    const authBeforeLogout = await admin.page.evaluate(() => {
      localStorage.setItem('ternilo.current-tenant', 'logout-tenant-sentinel')
      localStorage.setItem('ternilo.current-workspace', 'logout-workspace-sentinel')
      localStorage.setItem('ternilo.current-session', 'logout-session-sentinel')
      return {
        token: sessionStorage.getItem('ternilo.oidc.access'),
        tenant: localStorage.getItem('ternilo.current-tenant'),
      }
    })
    assert.ok(authBeforeLogout.token)
    assert.equal(authBeforeLogout.tenant, staleTenantSnapshot)
    await admin.page.getByRole('button', { name: 'Open sidebar' }).click()
    oidc.selectIdentity('admin')
    await admin.page.getByRole('button', { name: 'Sign out', exact: true }).click()
    await admin.page.getByRole('button', { name: 'Sign in with organization account' }).waitFor({ timeout: 30_000 })
    assert.deepEqual(await admin.page.evaluate(() => ({
      access: sessionStorage.getItem('ternilo.oidc.access'),
      refresh: sessionStorage.getItem('ternilo.oidc.refresh'),
      expires: sessionStorage.getItem('ternilo.oidc.expires'),
      tenant: localStorage.getItem('ternilo.current-tenant'),
      workspace: localStorage.getItem('ternilo.current-workspace'),
      session: localStorage.getItem('ternilo.current-session'),
    })), {
      access: null,
      refresh: null,
      expires: null,
      tenant: null,
      workspace: null,
      session: null,
    })
    const tenantRosterAfterLogin = admin.page.waitForRequest(request => (
      request.method() === 'GET'
        && new URL(request.url()).pathname === '/api/v1/tenants'
    ))
    await admin.page.getByRole('button', { name: 'Sign in with organization account' }).click()
    assert.equal((await tenantRosterAfterLogin).headers()['x-ternilo-tenant'], undefined)
    const tenantAfterLogin = admin.page.getByRole('combobox', { name: 'Switch space' })
    await tenantAfterLogin.waitFor({ timeout: 30_000 })
    assert.equal(await tenantAfterLogin.inputValue(), admin.personalTenantId, 'a new login starts in the account personal space')
    assert.notEqual(await tenantAfterLogin.inputValue(), staleTenantSnapshot)
    assert.deepEqual(await admin.page.evaluate(() => ({
      workspace: localStorage.getItem('ternilo.current-workspace'),
      session: localStorage.getItem('ternilo.current-session'),
    })), { workspace: null, session: null })

    assertClean('owner', owner.observations)
    assertClean('admin', admin.observations)
    assertClean('member', member.observations)
    assert.equal(control.diagnostics().includes('panicked'), false, control.diagnostics())
  } finally {
    await Promise.all(contexts.map(context => context.close().catch(() => {})))
    await browser?.close()
    await stopProcess(control)
    await oidc.close()
    await postgres.stop()
    await rm(directory, { recursive: true, force: true })
  }
})
