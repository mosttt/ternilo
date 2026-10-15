import assert from 'node:assert/strict'
import { verifyTeamUsage } from './team-usage-fixture.mjs'
import { modelFixture, verifyToolDiscovery, verifyToolBudgetAndMap, verifyContextComposition, verifyAutomaticReasoning, verifyReasoningDisplay } from './coordination-fixture.mjs'
import { mkdir, mkdtemp, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { chromium } from 'playwright'
import { localApi, until } from './model-device-fixture.mjs'
import { freePort, repository, startProcess, stopProcess, waitForHttp } from './platform-e2e-fixture.mjs'
import { selectChoice } from './browser-select-fixture.mjs'


const locales = {
  zh: { settings: '设置', plugins: '插件', expand: '展开: subagents', parallel: '并进', assisted: '协行', independent: '独行', investigator: '调查', save: '保存', model: '模型', dismiss: '关闭通知' },
  en: { settings: 'Settings', plugins: 'Plugins', expand: 'Expand: subagents', parallel: 'Parallel', assisted: 'Assisted', independent: 'Independent', investigator: 'Investigator', save: 'Save', model: 'Model', dismiss: 'Dismiss notification' },
  ko: { settings: '설정', plugins: '플러그인', expand: '펼치기: subagents', parallel: '병렬 협업', assisted: '보조 협업', independent: '독립 실행', investigator: '조사', save: '저장', model: '모델', dismiss: '알림 닫기' },
}

test('coordination settings persist in all locales and a real delegated call uses the role model', { timeout: 180_000 }, async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-coordination-'))
  let app, browser, upstream, activePage
  const errors = { page: [], console: [], http: [] }
  try {
    upstream = await modelFixture()
    const origin = `http://127.0.0.1:${await freePort()}`
    app = startProcess(process.env.TERNILO_E2E_NODE_BINARY ?? path.join(repository, 'target/debug/ternilo'),
      ['serve', '--listen', new URL(origin).host, '--data-dir', path.join(directory, 'data')],
      { ...Object.fromEntries(Object.keys(process.env).filter(key => key.startsWith('TERNILO_')).map(key => [key, undefined])), XDG_STATE_HOME: path.join(directory, 'state') })
    await waitForHttp(origin, app)
    assert.ok((await (await fetch(`${origin}/assets/app.js`)).text()).includes('data-notification-region'))
    const request = await localApi(origin)
    await request('/providers', { body: { id: 'coordination-fixture', display_name: 'Coordination Fixture', base_url: upstream.baseUrl,
      protocol: 'openai-chat-completions', api_key_ref: null, defaults: { context_window: 128_000, max_output_tokens: 4096, reasoning: { default_effort: 'medium', efforts: { low: 'low', medium: 'medium', high: 'high' } } },
      models: ['lead-model', 'role-model'].map(id => ({ id, display_name: id, settings: { mode: 'inherit' } })), timeout_ms: 30_000, max_attempts: 1, retry_base_delay_ms: 10 } })
    const folder = path.join(directory, 'workspace'); await mkdir(folder)
    const workspace = await request('/workspaces', { body: { path: folder } })
    const session = await request('/sessions', { body: { workspace_id: workspace.workspace_id } })
    const resource = `/sessions/${session.identity.session_id}`
    await request(resource, { method: 'PATCH', body: { model: { provider: 'named_provider', provider_id: 'coordination-fixture', model: 'lead-model' } } })
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
    for (const [locale, labels] of Object.entries(locales)) {
      const context = await browser.newContext({ viewport: locale === 'ko' ? { width: 320, height: 560 } : { width: 1440, height: 900 }, hasTouch: true, serviceWorkers: 'block' })
      await context.addInitScript(({ locale, sessionId, workspaceId }) => {
        localStorage.setItem('ternilo.locale', locale)
        localStorage.setItem('ternilo.current-session', sessionId)
        localStorage.setItem('ternilo.current-workspace', workspaceId)
      }, { locale, sessionId: session.identity.session_id, workspaceId: workspace.workspace_id })
      const page = await context.newPage(); activePage = page
      page.on('pageerror', error => errors.page.push(error.message))
      page.on('console', message => { if (message.type() === 'error') errors.console.push(message.text()) })
      page.on('response', response => { if (response.status() >= 400) errors.http.push(`${response.status()} ${response.url()}`) })
      await page.goto(origin)
      await page.locator('[data-composer-input]').waitFor()
      if (locale === 'ko') await page.getByRole('button', { name: '사이드바 열기', exact: true }).click()
      await page.getByRole('button', { name: labels.settings, exact: true }).click()
      const dialog = page.getByRole('dialog')
      await dialog.getByRole('button', { name: labels.plugins, exact: true }).click()
      await dialog.getByRole('tab', { name: { zh: '插件配置', en: 'Plugin configuration', ko: '플러그인 구성' }[locale], exact: true }).click()
      const card = dialog.locator('[data-plugin-id="subagents"]')
      await card.locator('button[aria-expanded][aria-label]').click()
      const mode = card.locator('#plugin-subagents-coordination-mode')
      if (locale === 'ko') {
        await mode.scrollIntoViewIfNeeded()
        const content = page.locator('[data-settings-content]')
        const before = await content.evaluate(element => element.scrollTop)
        const bounds = await content.boundingBox()
        assert.ok(bounds)
        const touch = await context.newCDPSession(page)
        const x = bounds.x + bounds.width - 18, y = bounds.y + bounds.height - 28
        const distance = Math.min(180, bounds.height - 64)
        try {
          await touch.send('Input.dispatchTouchEvent', { type: 'touchStart', touchPoints: [{ x, y }] })
          for (let step = 1; step <= 8; step++) await touch.send('Input.dispatchTouchEvent', { type: 'touchMove', touchPoints: [{ x, y: y - distance * step / 8 }] })
          await touch.send('Input.dispatchTouchEvent', { type: 'touchEnd', touchPoints: [] })
          await until(() => content.evaluate(element => element.scrollTop), value => value > before + 20, 'native touch scrolling in role settings')
        } finally { await touch.detach() }
      }
      if (locale === 'zh') assert.equal(await card.locator('#plugin-subagents-coordination-parallel').inputValue(), '4')
      await selectChoice(mode, 'parallel')
      if (locale === 'zh') assert.equal(await card.locator('#plugin-subagents-coordination-parallel').inputValue(), '8')
      assert.ok((await mode.textContent()).includes(labels.parallel))
      await card.locator('#plugin-subagents-coordination-parallel').fill('3')
      const investigator = card.getByRole('group', { name: labels.investigator, exact: true })
      await investigator.getByRole('button', { expanded: false }).first().click()
      await page.getByRole('menuitem').filter({ hasText: labels.model }).first().click()
      await page.getByRole('menuitem').filter({ hasText: 'role-model' }).first().click()
      await card.locator('#plugin-subagents-coordination-investigator-instructions').fill(`Evidence in ${locale}`)
      assert.equal(await card.locator('#plugin-subagents-coordination-investigator-readonly').getAttribute('aria-checked'), 'true')
      const save = card.getByRole('button', { name: labels.save, exact: true })
      await save.scrollIntoViewIfNeeded()
      const bounds = await save.boundingBox(), viewport = page.viewportSize()
      assert.ok(bounds && bounds.x >= 0 && bounds.x + bounds.width <= viewport.width && bounds.y >= 0 && bounds.y + bounds.height <= viewport.height)
      await save.click()
      const state = await until(() => request('/state'), value => value.sessions.find(item => item.identity.session_id === session.identity.session_id)?.profile_plugins.some(entry => entry.id === 'subagents' && entry.config.coordination?.investigator?.instructions === `Evidence in ${locale}`), 'saved coordination')
      const saved = state.sessions.find(item => item.identity.session_id === session.identity.session_id)
      assert.equal(saved.model.model, 'lead-model', 'editing role model does not switch the lead model')
      const config = saved.profile_plugins.find(entry => entry.id === 'subagents').config.coordination
      assert.equal(config.mode, 'parallel'); assert.equal(config.max_parallel, 3); assert.equal(config.investigator.model.model, 'role-model')
      await until(() => card.locator('button[aria-expanded][aria-label]').getAttribute('aria-expanded'), value => value === 'false', 'collapsed saved parallel card')
      await card.locator('button[aria-expanded][aria-label]').click()
      await selectChoice(mode, 'independent')
      assert.equal(await card.locator('fieldset').count(), 0)
      await selectChoice(mode, 'assisted')
      await save.scrollIntoViewIfNeeded(); await save.click()
      await until(() => request('/state'), value => value.sessions.find(item => item.identity.session_id === session.identity.session_id)?.profile_plugins.some(entry => entry.id === 'subagents' && entry.config.coordination?.mode === 'assisted'), 'saved assisted mode')
      await until(() => card.locator('button[aria-expanded][aria-label]').getAttribute('aria-expanded'), value => value === 'false', 'collapsed saved card')
      if (process.env.TERNILO_E2E_ARTIFACT_DIR) {
        await mkdir(process.env.TERNILO_E2E_ARTIFACT_DIR, { recursive: true })
        await card.locator('button[aria-expanded][aria-label]').click()
        const dismiss = page.getByRole('button', { name: labels.dismiss, exact: true })
        while (await dismiss.count()) await dismiss.first().click()
        await mode.waitFor()
        await mode.scrollIntoViewIfNeeded()
        await page.screenshot({ path: path.join(process.env.TERNILO_E2E_ARTIFACT_DIR, `coordination-${locale}.png`) })
      }
      const registry = dialog.locator('[data-plugin-id="tool-registry"]')
      await registry.locator('button[aria-expanded][aria-label]').click()
      const enabled = registry.locator('#plugin-tool-registry-schemas-enabled')
      if (await enabled.getAttribute('aria-checked') === 'false') await enabled.click()
      await registry.locator('#plugin-tool-registry-schemas-loaded').fill(String(3 + Object.keys(locales).indexOf(locale)))
      await registry.getByRole('button', { name: labels.save, exact: true }).click()
      await until(() => registry.locator('button[aria-expanded][aria-label]').getAttribute('aria-expanded'), value => value === 'false', 'saved schema loading')
      for (const [id, field, value] of [['session', 'max_tool_history_chars', '48000'], ['local-files', 'read_cache_bytes', '8388608']]) {
        const configured = dialog.locator(`[data-plugin-id="${id}"]`)
        await configured.locator('button[aria-expanded][aria-label]').click()
        const input = configured.locator(`#plugin-${id}-${field}`)
        const configuredValue = String(Number(value) + Object.keys(locales).indexOf(locale) + 1)
        await input.fill(configuredValue)
        assert.ok(await input.getAttribute('id'))
        await configured.getByRole('button', { name: labels.save, exact: true }).click()
        await until(() => configured.locator('button[aria-expanded][aria-label]').getAttribute('aria-expanded'), value => value === 'false', 'saved tool output settings')
        const saved = (await request('/state')).sessions.find(item => item.identity.session_id === session.identity.session_id)
        assert.equal(saved.profile_plugins.find(entry => entry.id === id).config[field], Number(configuredValue))
      }
      const optimization = dialog.locator('[data-plugin-id="agent-loop"]')
      const sessionConfig = dialog.locator('[data-plugin-id="session"]')
      await sessionConfig.locator('button[aria-expanded][aria-label]').click()
      const perTool = { read_file: 2048 + Object.keys(locales).indexOf(locale) }
      const limitsInput = sessionConfig.locator('#plugin-session-tool_result_limits')
      await limitsInput.fill('{"read_file":12}')
      assert.equal(await sessionConfig.getByRole('button', { name: labels.save, exact: true }).isEnabled(), false)
      await limitsInput.fill(JSON.stringify(perTool))
      if (process.env.TERNILO_E2E_ARTIFACT_DIR) {
        const dismiss = page.getByRole('button', { name: labels.dismiss, exact: true })
        while (await dismiss.count()) await dismiss.first().click()
        await limitsInput.evaluate(element => element.scrollIntoView({ block: 'center' }))
        await page.screenshot({ path: path.join(process.env.TERNILO_E2E_ARTIFACT_DIR, `per-tool-budget-${locale}.png`) })
      }
      await sessionConfig.getByRole('button', { name: labels.save, exact: true }).click()
      await until(() => sessionConfig.locator('button[aria-expanded][aria-label]').getAttribute('aria-expanded'), value => value === 'false', 'saved per-tool limits')
      const savedLimits = (await request('/state')).sessions.find(item => item.identity.session_id === session.identity.session_id)
      assert.deepEqual(savedLimits.profile_plugins.find(entry => entry.id === 'session').config.tool_result_limits, perTool)
      await optimization.locator('button[aria-expanded][aria-label]').click()
      for (const field of ['prompt_cache', 'observe_reasoning']) {
        const toggle = optimization.locator(`#plugin-agent-loop-${field}`)
        if (await toggle.getAttribute('aria-checked') === 'false') await toggle.click()
        assert.equal(await toggle.getAttribute('aria-checked'), 'true')
      }
      const automatic = optimization.locator('#plugin-agent-loop-automatic_reasoning')
      assert.equal(await automatic.getAttribute('aria-checked'), 'false')
      await automatic.click()
      assert.equal(await automatic.getAttribute('aria-checked'), 'true')
      await optimization.getByRole('button', { name: labels.save, exact: true }).click()
      await until(() => optimization.locator('button[aria-expanded][aria-label]').getAttribute('aria-expanded'), value => value === 'false', 'saved automatic reasoning')
      const savedAutomatic = (await request('/state')).sessions.find(item => item.identity.session_id === session.identity.session_id)
      assert.equal(savedAutomatic.profile_plugins.find(entry => entry.id === 'agent-loop').config.automatic_reasoning, true)
      await optimization.locator('button[aria-expanded][aria-label]').click()
      await automatic.click()
      const optimizationSave = optimization.getByRole('button', { name: labels.save, exact: true })
      if (await optimizationSave.isEnabled()) {
        await optimizationSave.click()
        await until(() => optimization.locator('button[aria-expanded][aria-label]').getAttribute('aria-expanded'), value => value === 'false', 'saved observation mode')
      }
      await context.close()
    }
    await request(`${resource}/queue`, { body: { content: { kind: 'prompt', input: 'Investigate using the configured role' } } })
    const pending = await until(() => request(`/questions?session_id=${session.identity.session_id}`), value => value.length, 'agent approval')
    await request(`/questions/${pending[0].question.id}/answer`, { body: { selected: ['Allow once'], custom: null } })
    await until(() => request(`${resource}/queue`), value => !value.active_run_id, 'finished role task')
    const state = await request('/state')
    const child = state.sessions.find(item => item.parent_session_id === session.identity.session_id && item.subagent?.role === 'investigator')
    assert.ok(child); assert.equal(child.model.model, 'role-model'); assert.equal(child.permissions, 'read_only')
    assert.ok(upstream.requests.some(body => body.model === 'role-model' && body.messages.some(message => String(message.content).includes('Evidence in ko'))))
    await verifyToolDiscovery(request, session.identity.session_id, upstream)
    for (const locale of Object.keys(locales)) {
      const context = await browser.newContext({ viewport: locale === 'ko' ? { width: 320, height: 560 } : { width: 1280, height: 900 }, hasTouch: true, serviceWorkers: 'block' })
      await context.addInitScript(({ locale, sessionId, workspaceId }) => {
        localStorage.setItem('ternilo.locale', locale); localStorage.setItem('ternilo.current-session', sessionId); localStorage.setItem('ternilo.current-workspace', workspaceId)
      }, { locale, sessionId: session.identity.session_id, workspaceId: workspace.workspace_id })
      const page = await context.newPage(); activePage = page
      page.on('pageerror', error => errors.page.push(error.message))
      page.on('console', message => { if (message.type() === 'error') errors.console.push(message.text()) })
      page.on('response', response => { if (response.status() >= 400) errors.http.push(`${response.status()} ${response.url()}`) })
      await page.goto(origin)
      const observed = page.locator('[data-chat-event="reasoning_effort_observed"]').last()
      await observed.waitFor({ state: 'attached' })
      const process = observed.locator('xpath=ancestor::*[@data-chat-turn][1]').locator('[data-turn-process]')
      if (await process.getAttribute('aria-expanded') === 'false') await process.click()
      await observed.getByText({ zh: '推理强度观察', en: 'Reasoning effort observation', ko: '추론 강도 관찰' }[locale], { exact: true }).waitFor()
      assert.ok((await observed.innerText()).includes({ zh: '实际模型与推理设置保持不变', en: 'actual model and reasoning settings are unchanged', ko: '실제 모델 및 추론 설정은 변경되지 않습니다' }[locale]))
      await verifyTeamUsage(page, request, session.identity.session_id, locale)
      await verifyContextComposition(page, request, session.identity.session_id, locale)
      await context.close()
    }
    await verifyToolBudgetAndMap(request, session.identity.session_id, upstream)
    await verifyAutomaticReasoning(request, session.identity.session_id, upstream)
    for (const locale of Object.keys(locales)) {
      const context = await browser.newContext({ viewport: locale === 'ko' ? { width: 320, height: 560 } : { width: 1280, height: 900 }, serviceWorkers: 'block' })
      await context.addInitScript(({ locale, id, workspaceId }) => { localStorage.setItem('ternilo.locale', locale); localStorage.setItem('ternilo.current-session', id); localStorage.setItem('ternilo.current-workspace', workspaceId) }, { locale, id: session.identity.session_id, workspaceId: workspace.workspace_id })
      const page = await context.newPage(); activePage = page
      page.on('pageerror', error => errors.page.push(error.message))
      page.on('console', message => { if (message.type() === 'error') errors.console.push(message.text()) })
      page.on('response', response => { if (response.status() >= 400) errors.http.push(`${response.status()} ${response.url()}`) })
      await page.goto(origin)
      await verifyReasoningDisplay(page, locale)
      await context.close()
    }
    assert.deepEqual(errors, { page: [], console: [], http: [] })
    console.log(JSON.stringify({ locales: Object.keys(locales), roleModel: child.model.model, childReadOnly: true, errors }))
  } catch (error) {
    console.error(app?.diagnostics().slice(-6000))
    if (activePage && !activePage.isClosed()) console.error((await activePage.locator('body').innerText()).slice(-10000))
    throw error
  } finally {
    await browser?.close(); await upstream?.close(); await stopProcess(app)
    await rm(directory, { recursive: true, force: true })
  }
})
