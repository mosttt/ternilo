import assert from 'node:assert/strict'
import path from 'node:path'
import { selectSpace, serverRequest } from './platform-e2e-fixture.mjs'
import { choose, closeSettings, computerModels, profile, settings, task } from './account-node-provider-fixture.mjs'
import { until } from './model-device-fixture.mjs'
import { workflowInterruptedTask, workflowOriginTask } from './account-node-workflow-fixture.mjs'

export async function sharedAccountTask({ browser, page, server, owner, tenantId, sessionId, folder, upstream }) {
  const instance = await owner('/admin/instance')
  await owner('/admin/instance', { method: 'PATCH', body: { mode: 'multi_user', revision: instance.revision } })
  const registration = await owner('/admin/registration')
  await owner('/admin/registration', { method: 'PATCH', body: { mode: 'open', require_approval: false, revision: registration.revision } })
  const credentials = { username: 'provider-member', email: 'provider-member@example.test', password: 'provider-member-password' }
  const { session } = await serverRequest(server.origin, '/auth/register', { body: credentials })
  await owner(`/tenants/${tenantId}/members/${session.user.user_id}`, { method: 'PUT', body: { role: 'member' } })
  const member = (path, options = {}) => serverRequest(server.origin, path, { token: session.access_token, tenantId, ...options })
  const personal = (path, options = {}) => member(path, { tenantId: session.personal_tenant_id, ...options })
  await personal('/credentials', { body: { name: 'SAME_PROVIDER_KEY', value: 'member-private-key' } })
  await personal('/providers', { body: profile(upstream.baseUrl) })
  const sharePath = `/sessions/${sessionId}/sharing/user/${session.user.user_id}`
  await owner(sharePath, { method: 'PUT', body: { view: true, submit: true, stop: true, configure: true } })
  const collaborator = await browser.newPage({ locale: 'zh-CN', viewport: { width: 1280, height: 900 }, hasTouch: true, serviceWorkers: 'block' })
  const errors = []
  collaborator.on('pageerror', error => errors.push(error.message))
  collaborator.on('console', message => { if (message.type() === 'error') errors.push(message.text()) })
  collaborator.on('response', response => { if (response.status() >= 400) errors.push(`${response.status()} ${new URL(response.url()).pathname}`) })
  try {
    await collaborator.goto(server.origin)
    await collaborator.getByLabel('用户名', { exact: true }).fill(credentials.username)
    await collaborator.getByLabel('密码', { exact: true }).fill(credentials.password)
    await collaborator.getByRole('button', { name: '登录', exact: true }).click()
    await selectSpace(collaborator, tenantId)
    const row = collaborator.locator(`[data-sidebar-session-row][data-session-id="${sessionId}"]`)
    await row.waitFor()
    if (!await row.isVisible()) await collaborator.locator('[data-sidebar-workspace-button]').first().click()
    await row.click()
    await settings(collaborator)
    await collaborator.locator('[data-provider-scope="account"]').getByText('同名 Provider', { exact: true }).waitFor()
    await computerModels(collaborator, tenantId)
    const computers = await member('/model-computers')
    assert.equal(computers.length, 1)
    assert.equal(computers[0].can_configure, false)
    assert.equal(computers[0].session_id, sessionId)
    await assert.rejects(() => member(`/providers?executor_id=${computers[0].executor_id}`), /400.*owned Node executor does not exist/)
    await assert.rejects(() => member(`/providers?executor_id=${computers[0].executor_id}`, { body: profile(upstream.baseUrl) }), /400.*owned Node executor does not exist/)
    await assert.rejects(() => member(`/providers?executor_id=${computers[0].executor_id}&session_id=${sessionId}`), /400/)
    assert.equal(await collaborator.locator('[data-provider-scope="node"]').getByRole('button', { name: '编辑', exact: true }).count(), 0, 'shared access does not grant computer configuration')
    await closeSettings(collaborator)
    await choose(collaborator, 'account')
    await task(collaborator, member, sessionId, folder, 'member-source')
    await childOriginTask({ page: collaborator, owner, member, session, sessionId, upstream })
    await workflowOriginTask({ page: collaborator, owner, member, session, sessionId, upstream, resourceOwner: server.owner.session.user.user_id })
    await workflowInterruptedTask({ page: collaborator, owner, member, session, sessionId, upstream, resourceOwner: server.owner.session.user.user_id })
    await workflowInterruptedTask({ page: collaborator, owner, member, session, sessionId, upstream, resourceOwner: server.owner.session.user.user_id,
      revoke: () => owner(sharePath, { method: 'PUT', body: { view: true, submit: true, stop: true, configure: false } }),
    })
    await owner(sharePath, { method: 'PUT', body: { view: true, submit: true, stop: true, configure: true } })
    await page.reload()
    await task(page, owner, sessionId, folder, 'member-source')
    const usage = await member('/model-access/requests?limit=50')
    assert.ok(usage.requests.some(request => request.actor_user_id === session.user.user_id && request.resource_owner_user_id === server.owner.session.user.user_id && request.model_beneficiary_user_id === session.user.user_id))
    assert.ok(usage.requests.some(request => request.actor_user_id === server.owner.session.user.user_id && request.resource_owner_user_id === server.owner.session.user.user_id && request.model_beneficiary_user_id === session.user.user_id))
    await owner(sharePath, { method: 'PUT', body: { view: true, submit: true, stop: false, configure: false } })
    const options = await owner(`/model-options?session_id=${sessionId}`)
    assert.equal(options.current.available, false, 'a Provider owner must retain session configuration permission')
    await collaborator.reload()
    await settings(collaborator)
    assert.equal(await collaborator.locator('[data-provider-scope="account"]').getByRole('button', { name: '编辑', exact: true }).count(), 1, 'own Providers remain editable after losing shared configuration permission')
    assert.deepEqual(errors, [])
    await choose(page, 'account')
  } catch (error) {
    if (process.env.TERNILO_E2E_ARTIFACT_DIR) await collaborator.screenshot({ path: path.join(process.env.TERNILO_E2E_ARTIFACT_DIR, 'shared-model-source-failure.png') }).catch(() => {})
    error.message += `\nCollaborator browser errors: ${JSON.stringify(errors)}`
    throw error
  } finally { await collaborator.close() }
}

async function childOriginTask({ page, owner, member, session, sessionId, upstream }) {
  const before = new Set((await member('/model-access/requests?limit=100')).requests.map(request => request.request_id))
  const hold = upstream.holdNext(body => body.stream && JSON.stringify(body.messages.findLast(message => message.role === 'user')).includes('child-origin-proof'))
  try {
    const accepted = await member(`/sessions/${sessionId}/queue`, { body: { content: { kind: 'prompt', input: '/agent-bg-on in-process Write child-origin-proof using member-source' } } })
    await page.getByRole('button', { name: '允许一次', exact: true }).last().click()
    await Promise.race([hold.began, new Promise((_, reject) => setTimeout(() => reject(new Error('child did not reach the model gateway')), 15000))])
    await owner(`/sessions/${sessionId}/queue`, { body: { content: { kind: 'prompt', input: '/code "later-owner-input"' } } })
    await until(() => owner(`/sessions/${sessionId}/events`), events => events.some(event => event.type === 'user_message' && event.content.includes('later-owner-input')), 'later owner input while the child is running')
    hold.release()
    const state = await until(() => owner('/state'), state => state.sessions.some(value => value.parent_session_id === sessionId && value.subagent), 'child mapped to Server')
    const child = state.sessions.find(value => value.parent_session_id === sessionId && value.subagent)
    const events = await until(() => owner(`/sessions/${child.identity.session_id}/events`), events => events.some(event => ['turn_finished', 'turn_failed'].includes(event.type)), 'child completion')
    assert.equal(events.some(event => event.type === 'turn_failed'), false, JSON.stringify(events))
    assert.ok(events.some(event => event.type === 'user_message' && event.run_id === accepted.run_id))
    const requests = (await member('/model-access/requests?limit=100')).requests.filter(request => !before.has(request.request_id))
    assert.ok(requests.length >= 2)
    assert.ok(requests.every(request => request.actor_user_id === session.user.user_id && request.model_beneficiary_user_id === session.user.user_id), 'later parent input must not replace the original child actor')
  } finally { hold.release() }
}
