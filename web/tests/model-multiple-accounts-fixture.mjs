import assert from 'node:assert/strict'
import { serverRequest } from './platform-e2e-fixture.mjs'
import { chooseModel, openModels, closeModels, until } from './model-device-fixture.mjs'

export async function addMember(server, models) {
  const registration = await models.request('/admin/registration')
  await models.request('/admin/registration', { method: 'PATCH', body: { mode: 'open', require_approval: false, revision: registration.revision } })
  const credentials = { username: 'model-member', email: 'model-member@example.test', password: 'model-member-password' }
  const { session } = await serverRequest(server.origin, '/auth/register', { body: credentials })
  const grant = await models.request('/admin/models/grants', { body: { name: 'Budget Alpha', subject: { kind: 'user', id: session.user.user_id },
    model_ids: ['account-model'], monthly_tokens: 2_000_000, max_concurrent_requests: 4, allow_resource_sharing: false } })
  return { owner: { ...credentials, session }, grant,
    request: (resource, options = {}) => serverRequest(server.origin, resource, { token: session.access_token, ...options }) }
}

export async function runTask(page, api, sessionId, provider, prompt) {
  await chooseModel(page, provider.id)
  const before = await page.locator('article[data-role="assistant"]').filter({ hasText: 'The account model completed this local task.' }).count()
  await page.getByRole('textbox', { name: '输入任务', exact: true }).fill(prompt)
  await page.getByRole('button', { name: '发送', exact: true }).click()
  await until(async () => page.locator('article[data-role="assistant"]').filter({ hasText: 'The account model completed this local task.' }).count(), count => count > before, 'explicit model task completed')
  await until(() => api(`/sessions/${sessionId}/queue`), queue => !queue.active_run_id && !queue.items.some(item => item.placement === 'queued'), 'task settled')
}

export async function refreshConnection(page, id) {
  await openModels(page)
  await page.locator(`[data-model-connection="${id}"]`).getByRole('button').first().click()
}

export async function verifySources(page, expected, screenshot) {
  await page.locator('button[title]').filter({ hasText: /Account Model|account-model/ }).last().click()
  await page.getByRole('menuitem', { name: /^模型/ }).click()
  for (const source of expected) {
    const group = page.locator(`[data-model-provider="${source.provider.id}"]`)
    assert.equal(await group.locator('[data-model-server]').textContent(), source.origin)
    assert.ok((await group.textContent()).includes(source.username))
  }
  assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth))
  await page.screenshot({ path: screenshot })
  await page.keyboard.press('Escape')
  await page.keyboard.press('Escape')
}

export async function expectOnlySelectedUsage(request, actor, grant, device) {
  const page = await request('/model-access/requests?limit=50')
  assert.ok(page.requests.length > 0)
  for (const item of page.requests) {
    assert.equal(item.actor_user_id, actor)
    assert.equal(item.grant_id, grant)
    assert.equal(item.key_id, device)
    assert.equal(item.origin, 'client_device')
  }
  assert.ok(page.requests.some(item => item.accounted_tokens > 0))
}
export { closeModels }
