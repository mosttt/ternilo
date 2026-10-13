import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { chromium } from 'playwright'
import { localApi, until } from './model-device-fixture.mjs'
import { freePort, initializeServer, repository, serverRequest, startProcess, stopProcess, waitForHttp } from './platform-e2e-fixture.mjs'
import { selectChoice } from './browser-select-fixture.mjs'

const labels = {
  zh: { settings: '设置', plugins: '插件', overview: '集中配置', configuration: '插件配置', jump: '快速定位插件', search: '搜索插件或参数', mode: '协作模式', children: '保留的子代理实例上限', team: '打开 Agent Team', collaboration: '协作设置', refresh: '刷新 Team', save: '保存', reset: '移除会话覆盖', discard: '放弃修改' },
  en: { settings: 'Settings', plugins: 'Plugins', overview: 'All settings', configuration: 'Plugin configuration', jump: 'Jump to plugin', search: 'Search plugins or parameters', mode: 'Coordination mode', children: 'Retained child agent limit', team: 'Open Agent Team', collaboration: 'Collaboration settings', refresh: 'Refresh Team', save: 'Save', reset: 'Remove session override', discard: 'Discard changes' },
  ko: { settings: '설정', plugins: '플러그인', overview: '통합 설정', configuration: '플러그인 구성', jump: '플러그인으로 이동', search: '플러그인 또는 매개변수 검색', mode: '협업 모드', children: '유지할 하위 에이전트 수 제한', team: '에이전트 팀 열기', collaboration: '협업 설정', refresh: '팀 새로 고침', save: '저장', reset: '세션 재정의 제거', discard: '변경 취소' },
}

for (const placement of ['local', 'server-node']) {
  test(`plugin drafts, saved settings and inheritance sync across all entry points (${placement})`, { timeout: 180_000 }, async () => {
    const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-plugin-settings-'))
    const artifacts = process.env.TERNILO_E2E_ARTIFACT_DIR ?? directory
    await mkdir(artifacts, { recursive: true })
    const processes = [], errors = { page: [], console: [], http: [] }
    let browser, page
    try {
      const environment = { ...Object.fromEntries(Object.keys(process.env).filter(key => key.startsWith('TERNILO_')).map(key => [key, undefined])), XDG_STATE_HOME: path.join(directory, 'state') }
      const nodeOrigin = `http://127.0.0.1:${await freePort()}`
      let origin = nodeOrigin, server, request, project, executor
      const args = ['serve', '--listen', new URL(nodeOrigin).host, '--data-dir', path.join(directory, 'node')]
      if (placement === 'server-node') {
        origin = `http://127.0.0.1:${await freePort()}`
        server = await initializeServer({ directory: path.join(directory, 'server'), origin, environment })
        processes.push(server)
        const tenant = server.owner.session.personal_tenant_id
        request = (url, options = {}) => serverRequest(origin, url, { token: server.owner.session.access_token, tenantId: tenant, ...options })
        project = (await request('/projects')).projects[0]
        const enrollment = (await request(`/tenants/${tenant}/my-computer-enrollments`, { body: { name: 'Settings computer', project_id: project.project_id, ttl_seconds: 600 } })).enrollment
        const credential = (await request('/enrollments/consume', { body: { token: enrollment.token } })).credential
        executor = enrollment.executor_id
        args.push('--node-id', executor, '--gateway-url', `${origin.replace('http:', 'ws:')}/api/v1/executors/connect`, '--allow-insecure-gateway')
        environment.TERNILO_LOCAL_TOKEN = credential.token
      }
      const node = startProcess(process.env.TERNILO_E2E_NODE_BINARY ?? path.join(repository, 'target/debug/ternilo'), args, environment)
      processes.push(node); await waitForHttp(nodeOrigin, node)
      const nodeRequest = await localApi(nodeOrigin)
      if (server) await until(() => request('/execution-targets'), value => value.executors.some(item => item.executor_id === executor && item.connected), 'connected Node')
      else request = nodeRequest
      const folder = path.join(directory, 'workspace'); await mkdir(folder)
      const workspace = server
        ? (await request('/workspaces', { body: { project_id: project.project_id, name: 'Settings workspace', placement: 'local_node', executor_id: executor, path: folder } })).workspace
        : await request('/workspaces', { body: { path: folder } })
      const session = await request('/sessions', { body: { workspace_id: workspace.workspace_id } })
      const resource = `/sessions/${session.identity.session_id}`
      const stored = async () => (await request('/state')).sessions.find(item => item.identity.session_id === session.identity.session_id)
      browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
      const assetHashes = {}
      for (const asset of ['app.js', 'app.css']) {
        const served = Buffer.from(await (await fetch(`${origin}/assets/${asset}`)).arrayBuffer())
        const built = await readFile(path.join(repository, 'web/dist/assets', asset))
        assert.deepEqual(createHash('sha256').update(served).digest('hex'), createHash('sha256').update(built).digest('hex'), `embedded ${asset} must match the current build`)
        assetHashes[asset] = createHash('sha256').update(served).digest('hex')
      }
      const context = await browser.newContext({ locale: 'zh-CN', viewport: { width: 1440, height: 900 }, hasTouch: true, serviceWorkers: 'block' })
      page = await context.newPage()
      page.setDefaultTimeout(12_000)
      page.on('pageerror', error => errors.page.push(error.message))
      page.on('console', message => { if (message.type() === 'error') errors.console.push(message.text()) })
      page.on('response', response => { if (response.status() >= 400) errors.http.push(`${response.status()} ${response.url()}`) })
      await page.goto(origin)
      if (server) {
        await page.getByLabel('用户名', { exact: true }).fill(server.owner.username)
        await page.getByLabel('密码', { exact: true }).fill(server.owner.password)
        await page.getByRole('button', { name: '登录', exact: true }).click()
      }
      await page.locator('[data-composer-input]').waitFor()
      for (const [locale, text] of Object.entries(labels)) {
        await page.evaluate(locale => { localStorage.setItem('ternilo.locale', locale); localStorage.setItem('ternilo.theme', locale === 'en' ? 'light' : 'dark') }, locale)
        await page.reload()
        await page.locator('[data-composer-input]').waitFor()
        const settings = page.locator('[data-settings-surface]')
        const card = settings.locator('[data-plugin-id="subagents"]')
        const openSettings = async () => {
          // Open settings while the sidebar is visible, then validate narrow layouts.
          await page.setViewportSize({ width: 1440, height: 900 })
          await page.getByRole('button', { name: server ? { zh: '用户设置', en: 'User settings', ko: '사용자 설정' }[locale] : text.settings, exact: true }).click()
          await settings.getByRole('button', { name: text.plugins, exact: true }).click()
          await settings.getByRole('tab', { name: text.overview, exact: true }).click()
        }
        const closeSettings = async () => {
          await settings.locator('[data-settings-toolbar] > button').last().click()
          await page.locator('[data-composer-input]').waitFor()
        }
        const openTeam = async () => {
          await page.getByRole('button', { name: text.team, exact: true }).filter({ hasText: locale === 'ko' ? '에이전트 팀' : 'Agent Team' }).click()
          const team = page.locator('[data-agent-team-panel]')
          await team.waitFor()
          assert.equal(await team.getByRole('heading').evaluate(element => element === document.activeElement), true)
          const refresh = team.getByRole('button', { name: text.refresh, exact: true })
          const close = team.locator('[data-slot="dialog-close"]')
          await team.getByRole('tab', { name: text.collaboration, exact: true }).click()
          const [a, b] = await Promise.all([refresh.boundingBox(), close.boundingBox()])
          assert.ok(a && b && Math.abs(a.y - b.y) < 1 && Math.abs(a.width - b.width) < 1 && Math.abs(a.height - b.height) < 1 && a.x + a.width <= b.x, JSON.stringify({ refresh: a, close: b }))
          await team.locator('#plugin-subagents-coordination-mode').waitFor()
          return team
        }
        await openSettings()
        assert.equal(await settings.getByRole('tab', { name: text.overview, exact: true }).getAttribute('aria-selected'), 'true')
        await card.getByLabel(text.children, { exact: true }).waitFor()
        assert.equal(await card.locator('button[aria-expanded][aria-label]').count(), 0)
        assert.equal(await settings.locator('[data-plugin-id="session-query"]').isVisible(), false)
        await settings.getByRole('navigation', { name: text.jump, exact: true }).getByRole('button', { name: 'subagents', exact: true }).click()
        assert.equal(await page.evaluate(() => document.activeElement.id), 'plugin-section-subagents')
        const mode = card.locator('#plugin-subagents-coordination-mode')
        await selectChoice(mode, 'parallel')
        await card.locator('#plugin-subagents-coordination-parallel').fill('12')
        await card.getByLabel(text.children, { exact: true }).fill('6')
        assert.match(await card.locator('[data-effective-parallel]').textContent(), /6/)
        await card.locator('#plugin-subagents-coordination-investigator-instructions').fill(`Draft ${locale}`)
        const search = settings.getByRole('searchbox', { name: text.search, exact: true })
        await search.fill('no-matching-plugin')
        assert.equal(await card.isVisible(), false)
        await search.fill(text.children)
        await card.waitFor()
        assert.equal(await card.locator('#plugin-subagents-coordination-investigator-instructions').inputValue(), `Draft ${locale}`)
        await settings.getByRole('tab', { name: text.configuration, exact: true }).click()
        await card.locator('button[aria-expanded][aria-label]').click()
        assert.equal(await card.getByLabel(text.children, { exact: true }).inputValue(), '6')
        await settings.getByRole('tab', { name: text.overview, exact: true }).click()
        await search.fill('')
        // Saving another plugin must not discard this draft or its persisted override.
        const files = settings.locator('[data-plugin-id="local-files"]')
        await files.locator('#plugin-local-files-max_read_bytes').fill('3145728')
        if (locale === 'zh') {
          let release, started
          const held = new Promise(resolve => { release = resolve })
          const received = new Promise(resolve => { started = resolve })
          const routePattern = `**/api/v1${resource}`
          await page.route(routePattern, async route => {
            if (route.request().method() === 'PATCH') { started(); await held }
            await route.continue()
          })
          try {
            await files.getByRole('button', { name: text.save, exact: true }).click()
            await received
            await closeSettings()
            const busyTeam = await openTeam()
            assert.equal(await busyTeam.getByRole('button', { name: text.save, exact: true }).isEnabled(), false, 'saving in one entry point must lock writes in another')
            release()
            await until(stored, value => value.profile_plugins.some(entry => entry.id === 'local-files'), 'save after switching entry points')
            await busyTeam.locator('[data-slot="dialog-close"]').click()
            await openSettings()
          } finally { release(); await page.unroute(routePattern) }
        } else await files.getByRole('button', { name: text.save, exact: true }).click()
        await until(stored, value => value.profile_plugins.some(entry => entry.id === 'local-files' && entry.config.max_read_bytes === 3145728), 'saved independent plugin')
        await until(() => files.getByRole('button', { name: text.save, exact: true }).isEnabled(), value => !value, 'save complete')
        assert.equal(await card.getByLabel(text.children, { exact: true }).inputValue(), '6')
        await closeSettings()
        let team = await openTeam()
        assert.equal(await team.getByLabel(text.children, { exact: true }).inputValue(), '6')
        assert.equal(await team.locator('#plugin-subagents-coordination-investigator-instructions').inputValue(), `Draft ${locale}`)
        await team.getByLabel(text.children, { exact: true }).fill('7')
        await team.getByRole('button', { name: text.refresh, exact: true }).click()
        assert.equal(await team.getByLabel(text.children, { exact: true }).inputValue(), '7')
        await team.locator('[data-slot="dialog-close"]').click()
        team = await openTeam()
        assert.equal(await team.getByLabel(text.children, { exact: true }).inputValue(), '7')
        await page.setViewportSize({ width: locale === 'en' ? 390 : 320, height: 620 })
        assert.equal(await team.evaluate(element => element.scrollWidth <= element.clientWidth), true)
        while (await page.locator('[data-notification-action]').count()) await page.locator('[data-notification-action]').first().click()
        await team.getByLabel(text.mode, { exact: true }).scrollIntoViewIfNeeded()
        const scroll = team.locator('#agent-team-settings-panel').locator('..')
        const initialScroll = await scroll.evaluate(element => element.scrollTop)
        const area = await scroll.boundingBox()
        const touch = await context.newCDPSession(page)
        const x = area.x + area.width - 24, y = area.y + area.height - 30
        assert.equal(await scroll.evaluate((element, point) => element.contains(document.elementFromPoint(point.x, point.y)), { x, y }), true, 'touch starts in the scrollable form')
        try {
          await touch.send('Input.dispatchTouchEvent', { type: 'touchStart', touchPoints: [{ x, y }] })
          for (let step = 1; step <= 8; step++) await touch.send('Input.dispatchTouchEvent', { type: 'touchMove', touchPoints: [{ x, y: y - step * 16 }] })
          await touch.send('Input.dispatchTouchEvent', { type: 'touchEnd', touchPoints: [] })
          await until(() => scroll.evaluate(element => element.scrollTop), value => value > initialScroll + 20, 'native team settings touch scroll')
        } finally { await touch.detach() }
        while (await page.locator('[data-notification-action]').count()) await page.locator('[data-notification-action]').first().click()
        const save = team.getByRole('button', { name: text.save, exact: true })
        await save.scrollIntoViewIfNeeded()
        const bounds = await save.boundingBox()
        assert.ok(bounds.x >= 0 && bounds.y >= 0 && bounds.x + bounds.width <= page.viewportSize().width && bounds.y + bounds.height <= page.viewportSize().height)
        await save.click()
        await until(stored, value => value.profile_plugins.some(entry => entry.id === 'subagents' && entry.config.max_children === 7), 'saved team settings')
        await until(() => save.isEnabled(), value => !value, 'team save complete')
        while (await page.locator('[data-notification-action]').count()) await page.locator('[data-notification-action]').first().click()
        await team.getByLabel(text.children, { exact: true }).scrollIntoViewIfNeeded()
        await page.screenshot({ path: path.join(artifacts, `plugin-settings-${placement}-${locale}.png`) })
        await team.locator('[data-slot="dialog-close"]').click()
        await openSettings()
        assert.equal(await card.getByLabel(text.children, { exact: true }).inputValue(), '7')
        // Changes in both directions, including enabled state and inheritance.
        const webFetch = settings.locator('[data-plugin-id="web-fetch"]')
        const toggle = webFetch.locator('> div [role="switch"]').first()
        await toggle.click()
        await until(() => toggle.getAttribute('aria-checked'), value => value === 'false', 'disabled plugin')
        await settings.getByRole('tab', { name: text.configuration, exact: true }).click()
        assert.equal(await toggle.getAttribute('aria-checked'), 'false')
        await closeSettings()
        team = await openTeam()
        await team.getByRole('button', { name: text.reset, exact: true }).click()
        await until(stored, value => !value.profile_plugins.some(entry => entry.id === 'subagents'), 'restored inheritance')
        await until(() => team.getByLabel(text.children, { exact: true }).inputValue(), value => value === '8', 'inherited value')
        await team.locator('[data-slot="dialog-close"]').click()
        await openSettings()
        assert.equal(await card.getByLabel(text.children, { exact: true }).inputValue(), '8')
        assert.equal(await toggle.getAttribute('aria-checked'), 'false')
        await webFetch.getByRole('button', { name: text.reset, exact: true }).click()
        await until(() => toggle.getAttribute('aria-checked'), value => value === 'true', 'restored plugin enabled state')
        assert.equal(await card.getByRole('button', { name: text.save, exact: true }).isEnabled(), false)
        await files.getByRole('button', { name: text.reset, exact: true }).click()
        await until(stored, value => value.profile_plugins.length === 0, 'reset fixture overrides')
        await closeSettings()
        await page.reload()
        await page.locator('[data-composer-input]').waitFor()
        team = await openTeam()
        assert.equal(await team.getByLabel(text.children, { exact: true }).inputValue(), '8')
        await team.locator('[data-slot="dialog-close"]').click()
      }
      assert.deepEqual(errors, { page: [], console: [], http: [] })
      await writeFile(path.join(artifacts, `plugin-settings-${placement}.json`), JSON.stringify({ placement, assetHashes, locales: Object.keys(labels), errors }, null, 2))
    } catch (error) {
      console.error(JSON.stringify(errors))
      await page?.screenshot({ path: path.join(artifacts, `plugin-settings-${placement}-failure.png`) }).catch(() => {})
      throw error
    } finally {
      await browser?.close()
      for (const process of processes.reverse()) await stopProcess(process)
      await rm(directory, { recursive: true, force: true })
    }
  })
}
