import { selectChoice } from './browser-select-fixture.mjs'
import assert from 'node:assert/strict'
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises'
import path from 'node:path'
import { tmpdir } from 'node:os'
import test from 'node:test'
import { chromium } from 'playwright'
import { freePort, initializeServer, repository, serverRequest, startProcess, stopProcess, waitForHttp } from './platform-e2e-fixture.mjs'
import { approveConnection, localApi, modelFixture, seedModels } from './model-device-fixture.mjs'

async function settled(locator) {
  await locator.evaluate(async element => {
    await Promise.all(element.getAnimations({ subtree: true }).filter(animation => animation.effect?.getTiming().iterations !== Infinity).map(animation => animation.finished.catch(() => {})))
  })
}

async function assertSwitch(toggle, mobile, checked) {
  await toggle.scrollIntoViewIfNeeded()
  await settled(toggle)
  assert.equal(await toggle.getAttribute('aria-checked'), String(checked))
  const geometry = await toggle.evaluate(element => {
    const root = element.getBoundingClientRect()
    const thumb = element.firstElementChild.getBoundingClientRect()
    const track = getComputedStyle(element, '::before')
    return { width: root.width, height: root.height, trackWidth: track.width, trackHeight: track.height, thumbWidth: thumb.width, thumbHeight: thumb.height, thumbLeft: thumb.left - root.left, thumbTop: thumb.top - root.top }
  })
  assert.equal(geometry.thumbWidth, 16)
  assert.equal(geometry.thumbHeight, 16)
  assert.equal(geometry.thumbLeft, checked ? 18 : 2)
  assert.equal(geometry.thumbTop, (geometry.height - 16) / 2)
  if (mobile) {
    assert.ok(geometry.width >= 40 && geometry.height >= 40, 'touch target remains large')
    assert.equal(geometry.trackWidth, '36px')
    assert.equal(geometry.trackHeight, '20px')
  } else {
    assert.equal(geometry.width, 36, 'modal button sizing must not stretch the switch')
    assert.equal(geometry.height, 20, 'desktop switch stays a pill, not a circle')
  }
}

async function assertLayout(page, surface) {
  await surface.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))))
  await settled(surface)
  assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth))
  assert.ok(await surface.evaluate(element => element.scrollWidth <= element.clientWidth))
  const box = await surface.boundingBox()
  assert.ok(box.x >= 0 && box.y >= 0 && box.x + box.width <= page.viewportSize().width + 1 && box.y + box.height <= page.viewportSize().height + 1, JSON.stringify(box))
}

async function assertNoOverlays(page) {
  await settled(page.locator('body'))
  assert.equal(await page.locator('[role="dialog"], [role="alertdialog"], [role="menu"], [role="listbox"], [role="tooltip"], [data-radix-popper-content-wrapper], [data-ternilo-dismiss-layer]').count(), 0, 'closed model dialogs and menus must not leave overlays')
}

test('model grants create missing prerequisites in place and reasoning switches retain geometry and saved values', { timeout: 180_000 }, async context => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-model-admin-flow-'))
  const artifacts = process.env.TERNILO_E2E_ARTIFACT_DIR ?? directory
  await mkdir(artifacts, { recursive: true })
  let application, browser, page
  const errors = []
  try {
    const origin = `http://127.0.0.1:${await freePort()}`
    application = await initializeServer({ directory, origin })
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
    page = await browser.newPage({ viewport: { width: 1440, height: 1000 }, serviceWorkers: 'block' })
    page.on('pageerror', error => errors.push(error.message))
    page.on('console', message => { if (message.type() === 'error') errors.push(message.text()) })
    page.on('response', response => { if (response.status() >= 400) errors.push(`${response.status()} ${new URL(response.url()).pathname}`) })
    await page.goto(`${origin}/admin/models`)
    await page.getByLabel('用户名', { exact: true }).fill(application.owner.username)
    await page.getByLabel('密码', { exact: true }).fill(application.owner.password)
    await page.getByRole('button', { name: '登录', exact: true }).click()
    await page.getByRole('button', { name: '分配使用权限与额度', exact: true }).click()
    await page.getByRole('button', { name: '分配模型与额度', exact: true }).click()
    const grant = page.getByRole('dialog', { name: '分配模型与额度', exact: true })
    assert.equal(await grant.locator('#model-grant-kind').getAttribute('data-choice-value'), 'user', 'one account does not require a group')
    await grant.locator('#model-grant-name').fill('Browser access')
    await grant.locator('#model-grant-tokens').fill('250000')
    await grant.getByRole('button', { name: new RegExp(application.owner.username) }).click()
    await grant.getByRole('button', { name: '发布模型', exact: true }).click()
    const publication = page.getByRole('dialog', { name: '发布模型', exact: true })
    await publication.locator('#publication-id').fill('browser-model')
    await publication.locator('#publication-name').fill('Browser model')
    await publication.getByRole('button', { name: '添加上游', exact: true }).click()
    const provider = page.getByRole('dialog', { name: '添加上游', exact: true })
    await provider.getByLabel('Provider ID', { exact: true }).fill('browser-provider')
    await provider.getByLabel('显示名称', { exact: true }).fill('Browser provider')
    await provider.getByLabel('API 地址', { exact: true }).fill('https://model.example.test/v1')
    await selectChoice(provider.getByLabel('API 协议', { exact: true }), 'deepseek-responses')
    await provider.getByPlaceholder('模型 ID', { exact: true }).fill('upstream-model')
    const toggle = provider.getByRole('switch')
    await assertSwitch(toggle, false, false)
    await toggle.click()
    await assertSwitch(toggle, false, true)
    await toggle.press('Space')
    await assertSwitch(toggle, false, false)
    await toggle.press('Space')
    await selectChoice(provider.locator('[id$="-default-effort"]'), 'high')
    for (const width of [390, 320]) {
      await page.setViewportSize({ width, height: 844 })
      await assertSwitch(toggle, true, true)
      await toggle.click()
      await assertSwitch(toggle, true, false)
      await toggle.click()
      await assertSwitch(toggle, true, true)
      await selectChoice(provider.locator('[id$="-default-effort"]'), 'high')
      await assertLayout(page, provider)
      await page.screenshot({ path: path.join(artifacts, `reasoning-${width}.png`) })
    }
    await page.setViewportSize({ width: 1440, height: 1000 })
    await page.screenshot({ path: path.join(artifacts, 'reasoning-desktop.png') })
    const createdProvider = page.waitForResponse(response => response.request().method() === 'POST' && new URL(response.url()).pathname === '/api/v1/admin/models/providers')
    await provider.getByRole('button', { name: '添加 Provider', exact: true }).click()
    assert.equal((await (await createdProvider).json()).profile.defaults.reasoning.default_effort, 'high')
    await provider.waitFor({ state: 'hidden' })
    assert.equal(await publication.locator('#publication-name').inputValue(), 'Browser model', 'publishing draft survives provider creation')
    assert.equal(await publication.locator('#publication-upstream-model').getAttribute('data-choice-value'), 'upstream-model')
    await publication.getByRole('button', { name: '保存', exact: true }).click()
    await publication.waitFor({ state: 'hidden' })
    assert.equal(await grant.locator('#model-grant-name').inputValue(), 'Browser access')
    assert.equal(await grant.locator('#model-grant-tokens').inputValue(), '250000')
    assert.equal(await grant.getByRole('button', { name: /Browser model.*browser-model/ }).getAttribute('aria-pressed'), 'true')
    const createdGrant = page.waitForResponse(response => response.request().method() === 'POST' && new URL(response.url()).pathname === '/api/v1/admin/models/grants')
    await grant.getByRole('button', { name: '保存', exact: true }).click()
    const directGrant = await (await createdGrant).json()
    assert.equal(directGrant.subject.kind, 'user')
    assert.deepEqual(directGrant.model_ids, ['browser-model'])
    assert.equal(directGrant.quota.limit_tokens, 250000)
    await grant.waitFor({ state: 'hidden' })
    await assertNoOverlays(page)

    await page.getByRole('button', { name: '分配模型与额度', exact: true }).click()
    await grant.locator('#model-grant-name').fill('Group budget')
    await selectChoice(grant.locator('#model-grant-kind'), 'group')
    await grant.getByRole('button', { name: '新建用户组', exact: true }).click()
    let group = page.getByRole('dialog', { name: '新建用户组', exact: true })
    await group.locator('#model-group-name').fill('Browser members')
    await group.getByRole('button', { name: '保存', exact: true }).click()
    group = page.getByRole('dialog', { name: '管理用户组', exact: true })
    await group.waitFor()
    assert.equal(await group.getByRole('button', { name: '使用此用户组' }).isDisabled(), true)
    await group.getByRole('button', { name: '添加账号', exact: true }).click()
    await group.getByRole('button', { name: '加入', exact: true }).click()
    await group.getByRole('button', { name: '使用此用户组', exact: true }).click()
    await group.waitFor({ state: 'hidden' })
    assert.equal(await grant.locator('#model-grant-name').inputValue(), 'Group budget')
    assert.equal(await grant.getByRole('button', { name: /Browser members/ }).getAttribute('aria-pressed'), 'true')
    await grant.getByRole('button', { name: /Browser model.*browser-model/ }).click()
    await page.setViewportSize({ width: 390, height: 844 })
    await assertLayout(page, grant)
    await page.screenshot({ path: path.join(artifacts, 'grant-mobile.png') })
    await grant.getByRole('button', { name: '保存', exact: true }).click()
    await grant.waitFor({ state: 'hidden' })
    await assertNoOverlays(page)
    await page.screenshot({ path: path.join(artifacts, 'grant-mobile-closed.png') })
    const grants = await serverRequest(origin, '/admin/models/grants', { token: application.owner.session.access_token })
    assert.equal(grants.grants.find(value => value.name === 'Group budget').subject.kind, 'group')

    await page.setViewportSize({ width: 1440, height: 1000 })
    await page.getByRole('tab', { name: '上游接入', exact: true }).click()
    await page.locator('[data-model-provider="browser-provider"]').getByRole('button', { name: '编辑', exact: true }).click()
    const editor = page.getByRole('dialog', { name: '编辑上游', exact: true })
    await editor.locator('summary').filter({ hasText: '自定义设置' }).click()
    await assertSwitch(editor.getByRole('switch'), false, true)
    assert.equal(await editor.locator('[id$="-default-effort"]').getAttribute('data-choice-value'), 'high')
    await editor.getByRole('switch').click()
    await editor.getByRole('button', { name: '保存', exact: true }).click()
    await editor.waitFor({ state: 'hidden' })
    await assertNoOverlays(page)
    const saved = await serverRequest(origin, '/admin/models/providers/browser-provider', { token: application.owner.session.access_token })
    assert.equal(saved.profile.defaults.reasoning ?? null, null)
    for (const width of [1440, 390]) {
      await page.setViewportSize({ width, height: width === 1440 ? 1000 : 844 })
      await page.goto(`${origin}/models?source=platform`)
      await page.locator(`[data-model-entitlement="${directGrant.grant_id}"]`).getByRole('button', { name: '创建接入密钥', exact: true }).click()
      let keyDialog = page.getByRole('dialog', { name: '创建接入密钥', exact: true })
      const keyName = width === 1440 ? 'Browser key' : 'Mobile key'
      await keyDialog.locator('#model-key-name').fill(keyName)
      await assertLayout(page, keyDialog)
      await page.screenshot({ path: path.join(artifacts, `key-create-${width}.png`) })
      await keyDialog.getByRole('button', { name: '创建接入密钥', exact: true }).click()
      keyDialog = page.getByRole('dialog', { name: '接入密钥已创建', exact: true })
      await keyDialog.locator('[data-model-secret]').waitFor()
      const secret = await keyDialog.locator('[data-model-secret]').textContent()
      await assertLayout(page, keyDialog)
      await page.screenshot({ path: path.join(artifacts, `key-created-${width}.png`), mask: [keyDialog.locator('[data-model-secret]')] })
      await keyDialog.getByRole('button', { name: '管理接入密钥', exact: true }).click()
      await page.locator('[data-model-key]').filter({ hasText: keyName }).waitFor()
      assert.equal((await page.locator('body').textContent()).includes(secret), false)
      await assertNoOverlays(page)
      await page.screenshot({ path: path.join(artifacts, `key-closed-${width}.png`) })
    }
    await page.setViewportSize({ width: 1440, height: 1000 })
    await serverRequest(origin, '/providers', { token: application.owner.session.access_token, tenantId: application.owner.session.personal_tenant_id, body: {
      ...saved.profile, id: 'account-source', display_name: 'Account source',
      models: [{ id: 'named-model', display_name: 'Readable model name', settings: { mode: 'inherit' } }, { id: 'id-only-model', settings: { mode: 'inherit' } }],
    } })
    await page.goto(`${origin}/models?source=account`)
    await page.locator('[data-account-model-default] [data-model-picker]').click()
    const accountMenu = page.getByRole('menuitem', { name: /^模型/ })
    await accountMenu.waitFor()
    assert.equal(await page.getByRole('button', { name: '平台授权', exact: true }).count(), 1, 'only the page source filter remains outside the unified model menu')
    await accountMenu.hover()
    await accountMenu.press('ArrowRight')
    const modelItems = page.locator('[data-model-provider="account-source"] [role="menuitem"]')
    await modelItems.first().waitFor()
    assert.deepEqual(await modelItems.locator('.font-mono').allTextContents(), ['named-model', 'id-only-model'], 'all model entries show an ID, even without a separate display name')
    await page.locator('[data-model-source="platform"] [role="menuitem"]').first().waitFor()
    await page.screenshot({ path: path.join(artifacts, 'model-sources.png') })
    await page.keyboard.press('Escape')
    await page.keyboard.press('Escape')
    await page.getByRole('menu').waitFor({ state: 'hidden' })
    await assertNoOverlays(page)
    await page.screenshot({ path: path.join(artifacts, 'model-sources-desktop-closed.png') })
    await page.setViewportSize({ width: 390, height: 844 })
    await page.locator('[data-account-model-default]').evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))))
    await page.locator('[data-account-model-default] [data-model-picker]').click()
    await page.getByRole('menuitem', { name: /^模型/ }).click()
    const platform = page.getByRole('menu').filter({ has: page.locator('[data-model-source="platform"]') })
    await assertLayout(page, platform)
    await page.screenshot({ path: path.join(artifacts, 'model-sources-mobile.png') })
    await platform.locator(`[data-model-grant="${directGrant.grant_id}"]`).getByRole('menuitem', { name: /Browser model/ }).click()
    await platform.waitFor({ state: 'hidden' })
    await page.locator('[data-account-model-default] [data-model-picker]').filter({ hasText: 'Browser model' }).waitFor()
    await assertNoOverlays(page)
    await page.screenshot({ path: path.join(artifacts, 'model-sources-mobile-closed.png') })
    await page.setViewportSize({ width: 1440, height: 1000 })
    await page.goto(`${origin}/admin/models`)
    for (const width of [1440, 390, 320]) {
      await page.setViewportSize({ width, height: width === 1440 ? 1000 : 844 })
      const routes = ['models', 'instance', 'workers', 'models', 'instance', 'workers']
      for (const [step, route] of routes.entries()) {
        if (width === 1440) await page.locator(`aside a[href="/admin/${route}"]`).click()
        else await selectChoice(page.getByRole('combobox', { name: '管理导航', exact: true }), `/admin/${route}`)
        await page.waitForURL(`${origin}/admin/${route}`)
        await page.locator('main > :first-child').waitFor()
        if (route === 'models') {
          await page.getByRole('tab', { name: '上游接入', exact: true }).click()
          await page.locator('[data-model-provider="browser-provider"]').getByRole('button', { name: '编辑', exact: true }).click()
          await editor.waitFor()
          if (step === 0) await editor.getByRole('button', { name: '取消', exact: true }).click()
          else await page.keyboard.press('Escape')
          await editor.waitFor({ state: 'hidden' })
        }
        await page.locator('main').hover()
        await page.mouse.wheel(0, 1000)
        await page.mouse.wheel(0, -1000)
        await page.screenshot({ path: path.join(artifacts, `admin-${width}-${step}-${route}.png`) })
        assert.equal(await page.locator('[role="dialog"], [role="tooltip"], [data-radix-popper-content-wrapper]').count(), 0, 'closed model dialogs must not leave overlays on other pages')
        await assertNoOverlays(page)
        await assertLayout(page, page.locator('main'))
      }
    }
    assert.deepEqual(errors, [])
    context.diagnostic('Verified real provider, publication, grants and desktop/mobile keys; 1440/390/320px switches and repeated administration navigation; no leftover dialogs or menus, console or HTTP errors')
  } catch (error) {
    if (page) await page.screenshot({ path: path.join(artifacts, 'failure.png') }).catch(() => {})
    throw error
  } finally {
    await writeFile(path.join(artifacts, 'server.log'), application?.diagnostics() ?? '')
    await writeFile(path.join(artifacts, 'browser-errors.json'), JSON.stringify(errors, null, 2))
    await browser?.close()
    await stopProcess(application)
    await rm(directory, { recursive: true, force: true })
  }
})

test('the standalone client distinguishes device models from connected Server grants with consistent model IDs', { timeout: 180_000 }, async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-client-model-menu-'))
  const artifacts = process.env.TERNILO_E2E_ARTIFACT_DIR ?? directory
  await mkdir(artifacts, { recursive: true })
  let application, local, browser, upstream
  const errors = []
  try {
    upstream = await modelFixture()
    application = await initializeServer({ directory: path.join(directory, 'server'), origin: `http://127.0.0.1:${await freePort()}` })
    await seedModels(application, upstream)
    const origin = `http://127.0.0.1:${await freePort()}`
    const binary = process.env.TERNILO_E2E_NODE_BINARY ?? path.join(repository, 'target/debug/ternilo')
    local = startProcess(binary, ['serve', '--listen', new URL(origin).host, '--data-dir', path.join(directory, 'local')], { XDG_STATE_HOME: path.join(directory, 'state') })
    await waitForHttp(origin, local)
    const request = await localApi(origin)
    await request('/providers', { body: { id: 'device-only', display_name: 'Device provider', base_url: upstream.baseUrl, protocol: 'openai-chat-completions', api_key_ref: null,
      defaults: { context_window: 32000, max_output_tokens: 2048 }, models: [{ id: 'device-named', display_name: 'Device model', settings: { mode: 'inherit' } }, { id: 'device-raw-id', settings: { mode: 'inherit' } }], timeout_ms: 0, max_attempts: 1, retry_base_delay_ms: 25 } })
    const folder = path.join(directory, 'workspace')
    await mkdir(folder)
    const workspace = await request('/workspaces', { body: { path: folder } })
    await request('/sessions', { body: { workspace_id: workspace.workspace_id } })
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
    const page = await browser.newPage({ viewport: { width: 1440, height: 1000 }, hasTouch: true, serviceWorkers: 'block' })
    const approval = await browser.newPage({ viewport: { width: 390, height: 844 }, hasTouch: true, serviceWorkers: 'block' })
    for (const target of [page, approval]) {
      target.on('pageerror', error => errors.push(error.message))
      target.on('console', message => { if (message.type() === 'error') errors.push(message.text()) })
      target.on('response', response => { if (response.status() >= 400) errors.push(`${response.status()} ${new URL(response.url()).pathname}`) })
    }
    await page.goto(origin)
    await approveConnection(page, approval, application, 'Browser Server')
    for (const width of [1440, 390]) {
      await page.setViewportSize({ width, height: 900 })
      await page.reload()
      await page.locator('[data-model-picker]').click()
      await page.getByRole('menuitem', { name: /^模型/ }).click()
      const items = page.locator('[data-model-provider="device-only"] [role="menuitem"]')
      await items.first().waitFor()
      assert.deepEqual(await items.locator('.font-mono').allTextContents(), ['device-named', 'device-raw-id'])
      await page.getByText('设备本地', { exact: true }).waitFor()
      await page.getByText('平台授权 · 已连接 Server', { exact: true }).waitFor()
      const connection = page.locator('[data-model-provider]').filter({ hasText: 'Budget Alpha' })
      assert.equal(await connection.getByRole('menuitem').locator('.font-mono').textContent(), 'account-model')
      await page.screenshot({ path: path.join(artifacts, `client-models-${width}.png`) })
      await connection.getByRole('menuitem').click()
      await page.getByRole('menu').waitFor({ state: 'hidden' })
      await assertNoOverlays(page)
      await page.screenshot({ path: path.join(artifacts, `client-models-${width}-closed.png`) })
    }
    assert.deepEqual(errors, [])
  } finally {
    await writeFile(path.join(artifacts, 'client-server.log'), application?.diagnostics() ?? '')
    await writeFile(path.join(artifacts, 'client-node.log'), local?.diagnostics() ?? '')
    await writeFile(path.join(artifacts, 'client-browser-errors.json'), JSON.stringify(errors, null, 2))
    await browser?.close()
    await stopProcess(local)
    await stopProcess(application)
    await upstream?.close()
    await rm(directory, { recursive: true, force: true })
  }
})
