import assert from 'node:assert/strict'
import { mkdir, mkdtemp, readFile, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { chromium } from 'playwright'
import { freePort, initializeServer, repository, serverRequest, startProcess, stopProcess, waitForHttp } from './platform-e2e-fixture.mjs'
import { localApi, until } from './model-device-fixture.mjs'
import { profile, upstream } from './account-node-provider-fixture.mjs'

const labels = {
  zh: { clear: '每次新会话时选择', model: /^模型/, choose: '选择模型', notice: '为当前会话选择模型' },
  en: { clear: 'Choose for each new session', model: /^Model/, choose: 'Choose a model', notice: 'Choose a model for this session' },
  ko: { clear: '새 세션마다 선택', model: /^모델/, choose: '모델 선택', notice: '현재 세션에 사용할 모델을 선택하세요' },
}

async function modelMenu(page, label) {
  const item = page.getByRole('menuitem', { name: label })
  if (await item.getAttribute('aria-haspopup') === 'menu') await item.press('ArrowRight')
  else await item.click()
}

// Real Server preferences, Node commands and model proxy; no Worker is required to create a managed session.
for (const managedExecutionEnabled of [false, true]) test(`personal defaults apply to new Server sessions without changing existing sessions or device defaults (managed=${managedExecutionEnabled})`, { timeout: 180_000 }, async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-personal-default-'))
  const artifacts = path.join(process.env.TERNILO_E2E_ARTIFACT_DIR ?? directory, managedExecutionEnabled ? 'managed-enabled' : 'computers-only')
  await mkdir(artifacts, { recursive: true })
  const processes = [], errors = []
  const model = await upstream('personal-default', 'test-key')
  let browser, page
  try {
    const server = await initializeServer({ directory: path.join(directory, 'server'), origin: `http://127.0.0.1:${await freePort()}`, mode: 'single_user', managedExecutionEnabled })
    processes.push(server)
    const tenantId = server.owner.session.personal_tenant_id
    const request = (resource, options = {}) => serverRequest(server.origin, resource, { token: server.owner.session.access_token, tenantId, ...options })
    await request('/credentials', { body: { name: 'SAME_PROVIDER_KEY', value: 'test-key' } })
    await request('/providers', { body: profile(model.baseUrl) })
    const enrollment = await request(`/tenants/${tenantId}/my-computer-enrollments`, { body: { name: 'default-node', project_id: null, ttl_seconds: 600 } })
    const credential = await request('/enrollments/consume', { body: { token: enrollment.enrollment.token } })
    const origin = `http://127.0.0.1:${await freePort()}`
    const node = startProcess(process.env.TERNILO_E2E_NODE_BINARY ?? path.join(repository, 'target/debug/ternilo'), ['serve', '--listen', new URL(origin).host, '--data-dir', path.join(directory, 'node'), '--node-id', enrollment.enrollment.executor_id, '--gateway-url', `${server.origin.replace('http:', 'ws:')}/api/v1/executors/connect`, '--allow-insecure-gateway'], { TERNILO_LOCAL_TOKEN: credential.credential.token, XDG_STATE_HOME: path.join(directory, 'state') })
    processes.push(node)
    await waitForHttp(origin, node)
    const local = await localApi(origin)
    const native = { ...profile(model.baseUrl), id: 'native', api_key_ref: null }
    await local('/providers', { body: native })
    const nativeDefault = { provider: 'named_provider', provider_id: 'native', model: 'same-model' }
    await local('/default-model', { method: 'PUT', body: nativeDefault })
    const folder = path.join(directory, 'workspace'); await mkdir(folder)
    const localWorkspace = await local('/workspaces', { body: { path: folder } })
    const initialState = await until(() => request('/state'), value => value.workspaces.length === 1, 'personal computer workspace discovered')
    const workspaceId = initialState.workspaces[0].workspace_id
    const create = () => request('/sessions', { body: { workspace_id: workspaceId } })
    const sessionState = async id => (await request('/state')).sessions.find(session => session.identity.session_id === id)
    const untouched = await create()
    assert.deepEqual(untouched.model, nativeDefault, 'untouched accounts keep the original device-default behavior')
    let managed, teamWorkspace
    const { tenant: team } = await request('/tenants', { body: { slug: 'default-team', display_name: 'Default team' } })
    if (managedExecutionEnabled) {
      const { project } = await request('/projects', { body: { name: 'Managed defaults' } })
      const { workspace } = await request('/workspaces', { body: { project_id: project.project_id, name: 'Managed defaults', placement: 'cloud' } })
      managed = () => request('/sessions', { body: { workspace_id: workspace.workspace_id } })
      const { project: teamProject } = await request('/projects', { tenantId: team.tenant_id, body: { name: 'Team project' } })
      teamWorkspace = (await request('/workspaces', { tenantId: team.tenant_id, body: { project_id: teamProject.project_id, name: 'Team workspace', placement: 'cloud' } })).workspace
    }
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
    page = await browser.newPage({ locale: 'zh-CN', viewport: { width: 1366, height: 900 }, hasTouch: true, serviceWorkers: 'block' })
    page.on('pageerror', error => errors.push(error.message))
    page.on('console', message => { if (message.type() === 'error') errors.push(message.text()) })
    page.on('response', response => { if (response.status() >= 400) errors.push(`${response.status()} ${new URL(response.url()).pathname}`) })
    await page.goto(server.origin)
    await page.getByLabel('用户名', { exact: true }).fill(server.owner.username)
    await page.getByLabel('密码', { exact: true }).fill(server.owner.password)
    await page.getByRole('button', { name: '登录', exact: true }).click()
    await page.locator('[data-space-switcher]').waitFor()
    const chosen = { provider: 'account_provider', owner_user_id: server.owner.session.user.user_id, provider_id: 'same', model: 'same-model' }
    let preservedSession
    for (const [locale, text] of Object.entries(labels)) {
      await page.evaluate(locale => localStorage.setItem('ternilo.locale', locale), locale)
      await page.setViewportSize(locale === 'en' ? { width: 1366, height: 900 } : { width: 390, height: 844 })
      await page.goto(`${server.origin}/models`)
      const picker = page.locator('[data-account-model-default] [data-model-picker]')
      await picker.click()
      await modelMenu(page, text.model)
      await page.locator('[data-model-source="account"] [data-model-provider="same"]').getByRole('menuitem').filter({ hasText: 'same-model' }).click()
      await until(() => request('/default-model'), value => value.provider_id === 'same', 'account default saved')
      assert.deepEqual(await request(`/default-model?workspace_id=${workspaceId}&for_new_session=true`), chosen)
      assert.deepEqual(await request(`/default-model?workspace_id=${workspaceId}`), nativeDefault, 'existing device-default reads keep their original scope')
      if (locale === 'en') {
        await page.goto(server.origin)
        await page.locator(`[data-sidebar-session-row][data-session-id="${untouched.identity.session_id}"]`).click()
        await page.locator('[data-sidebar-new-session]').click()
        const selected = await until(() => page.evaluate(() => localStorage.getItem('ternilo.current-session')), id => id !== untouched.identity.session_id, 'New session does not reopen an old blank with a different default')
        assert.deepEqual((await sessionState(selected)).model, chosen)
        const count = (await request('/state')).sessions.length
        await page.locator('[data-sidebar-new-session]').click()
        await until(() => page.evaluate(() => localStorage.getItem('ternilo.current-session')), id => id === selected, 'matching blank session reused')
        assert.equal((await request('/state')).sessions.length, count)
        await page.goto(`${server.origin}/models`)
      }
      const before = await create()
      assert.deepEqual(before.model, chosen, 'Server-created computer session inherits the account model, not the native model')
      if (managed) assert.deepEqual((await managed()).model, chosen)
      assert.equal((await request('/default-model', { tenantId: team.tenant_id })).provider, 'profile_default')
      if (teamWorkspace) assert.equal((await request('/sessions', { tenantId: team.tenant_id, body: { workspace_id: teamWorkspace.workspace_id } })).model.provider, 'profile_default')
      preservedSession ??= before
      await page.reload()
      await picker.filter({ hasText: 'same-model' }).waitFor()
      await picker.click()
      await page.getByRole('menuitem', { name: text.clear, exact: true }).click()
      await until(() => request('/default-model'), value => value.provider === 'profile_default', 'default cleared')
      await page.reload()
      await picker.filter({ hasText: text.clear }).waitFor()
      assert.equal(await picker.locator('span').evaluate(element => element.scrollWidth <= element.clientWidth), true, 'the reset state is fully readable on mobile')
      assert.deepEqual((await sessionState(before.identity.session_id)).model, chosen, 'existing session retains its explicit model')
      if (managed) assert.equal((await managed()).model.provider, 'profile_default')
      const after = await create()
      assert.equal(after.model.provider, 'profile_default', 'explicit clear does not silently pick the computer default')
      assert.deepEqual(await local('/default-model'), nativeDefault)
      assert.deepEqual((await local('/sessions', { body: { workspace_id: localWorkspace.workspace_id } })).model, nativeDefault)
      await page.screenshot({ path: path.join(artifacts, `default-cleared-${locale}.png`) })
      await page.evaluate(id => localStorage.setItem('ternilo.current-session', id), after.identity.session_id)
      await page.goto(server.origin)
      const notice = page.locator('[data-model-onboarding]')
      await notice.getByText(text.notice, { exact: true }).waitFor()
      await notice.getByRole('button', { name: text.choose, exact: true }).click()
      await modelMenu(page, text.model)
      await page.locator('[data-model-source="account"] [data-model-provider="same"]').getByRole('menuitem').filter({ hasText: 'same-model' }).click()
      await until(() => sessionState(after.identity.session_id), value => value.model.provider === 'account_provider', 'onboarding selects an existing model')
      await notice.waitFor({ state: 'detached' })
    }
    // An inherited account model must actually be authorized for the new Node session.
    const id = preservedSession.identity.session_id
    await request(`/sessions/${id}/queue`, { body: { content: { kind: 'prompt', input: 'Verify the inherited model by writing a file.' } } })
    const events = await until(() => request(`/sessions/${id}/events`), value => value.some(event => ['turn_finished', 'turn_failed'].includes(event.type)), 'inherited account model finishes')
    assert.ok(!events.some(event => event.type === 'turn_failed'), JSON.stringify(events.filter(event => event.type === 'turn_failed')))
    assert.equal(await readFile(path.join(folder, 'provider-source.txt'), 'utf8'), 'personal-default')
    await request('/default-model', { method: 'PUT', body: chosen })
    await request('/credentials/SAME_PROVIDER_KEY', { method: 'DELETE' })
    const countBeforeInvalid = (await local('/state')).sessions.length
    await assert.rejects(create, /400|403|409/)
    assert.equal((await local('/state')).sessions.length, countBeforeInvalid, 'an unavailable default is rejected before creating a Node session')
    await request('/default-model', { method: 'PUT', body: { provider: 'profile_default' } })
    assert.equal((await create()).model.provider, 'profile_default', 'an unavailable default can still be cleared')
    assert.deepEqual(errors, [])
  } catch (error) {
    if (page && !page.isClosed()) { console.error(await page.locator('body').innerText()); await page.screenshot({ path: path.join(artifacts, 'default-failure.png') }) }
    for (const process of processes) console.error(process.diagnostics().slice(-2500))
    throw error
  } finally {
    await browser?.close()
    for (const process of processes.reverse()) await stopProcess(process)
    await model.close()
    await rm(directory, { recursive: true, force: true })
  }
})
