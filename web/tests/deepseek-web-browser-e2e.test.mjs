import assert from 'node:assert/strict'
import { createServer } from 'node:http'
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { chromium } from 'playwright'
import { localApi, until } from './model-device-fixture.mjs'
import { freePort, initializeServer, repository, serverRequest, startProcess, stopProcess, waitForHttp } from './platform-e2e-fixture.mjs'

const labels = {
  zh: { settings: '设置', userSettings: '用户设置', plugins: '插件', overview: '集中配置', configuration: '插件配置', results: '最多搜索结果', credential: '凭据名称', save: '保存', send: '发送', stop: '停止运行', fetch: '读取网页' },
  en: { settings: 'Settings', userSettings: 'User settings', plugins: 'Plugins', overview: 'All settings', configuration: 'Plugin configuration', results: 'Maximum search results', credential: 'Credential name', save: 'Save', send: 'Send', stop: 'Stop run', fetch: 'Fetch web page' },
  ko: { settings: '설정', userSettings: '사용자 설정', plugins: '플러그인', overview: '통합 설정', configuration: '플러그인 구성', results: '최대 검색 결과 수', credential: '자격 증명 이름', save: '저장', send: '전송', stop: '실행 중지', fetch: '웹 페이지 가져오기' },
}

for (const placement of ['local', 'server-node']) {
  test(`DeepSeek search and fetch share plugin settings and execute on the selected computer (${placement})`, { timeout: 150_000 }, async () => {
    const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-deepseek-web-'))
    const artifacts = process.env.TERNILO_E2E_ARTIFACT_DIR ? path.join(process.env.TERNILO_E2E_ARTIFACT_DIR, `deepseek-web-${placement}`) : undefined
    if (artifacts) await mkdir(artifacts, { recursive: true })
    const processes = [], calls = [], errors = []
    let browser, page, mode = 'success', cancelled = false
    const key = 'fixture-deepseek-tools-secret'
    const upstream = createServer(async (request, response) => {
      const chunks = []; for await (const chunk of request) chunks.push(chunk)
      const body = JSON.parse(Buffer.concat(chunks))
      if (request.url === '/v1/chat/completions') {
        const lastUser = body.messages.findLastIndex(item => item.role === 'user')
        const done = body.messages.slice(lastUser + 1).filter(item => item.role === 'tool')
        const tool = done.length === 0 ? 'web_search' : done.length === 1 ? 'web_fetch' : null
        const args = tool === 'web_search' ? { query: 'DeepSeek 中文 한국어 site:example.com', limit: 1, language: 'zh,ko' } : { url: 'https://example.com/article' }
        const delta = tool ? { role: 'assistant', tool_calls: [{ index: 0, id: `call-${done.length}`, type: 'function', function: { name: tool, arguments: JSON.stringify(args) } }] } : { role: 'assistant', content: 'Search and Markdown fetch completed.' }
        const frame = value => `data: ${JSON.stringify({ id: 'fixture', model: 'fixture', object: 'chat.completion.chunk', ...value })}\n\n`
        response.writeHead(200, { 'content-type': 'text/event-stream' }).end(frame({ choices: [{ index: 0, delta, finish_reason: null }] }) + frame({ choices: [{ index: 0, delta: {}, finish_reason: tool ? 'tool_calls' : 'stop' }] }) + frame({ choices: [], usage: { prompt_tokens: 20, completion_tokens: 10, total_tokens: 30 } }) + 'data: [DONE]\n\n')
        return
      }
      calls.push({ url: request.url, body, key: request.headers.authorization })
      if (mode === 'slow') { response.on('close', () => { cancelled = true }); return }
      if (mode === 'balance') { response.writeHead(402).end(JSON.stringify({ error: { message: `${key} private-provider-detail` } })); return }
      const result = request.url === '/v1/tools/search'
        ? { results: [{ title: 'Search source', url: 'https://example.com/article', snippet: 'Short snippet', summary: 'DeepSeek detailed evidence', content: '', date: null, authority: 3 }] }
        : { title: 'Fetched page', content: '# Markdown evidence\n\nArticle body.' }
      response.writeHead(200, { 'content-type': 'application/json' }).end(JSON.stringify(result))
    })
    await new Promise(resolve => upstream.listen(0, '127.0.0.1', resolve))
    try {
      const nodeOrigin = `http://127.0.0.1:${await freePort()}`
      const environment = { ...Object.fromEntries(Object.keys(process.env).filter(key => key.startsWith('TERNILO_')).map(key => [key, undefined])), XDG_STATE_HOME: path.join(directory, 'state') }
      let origin = nodeOrigin, request, server, project, executor
      const args = ['serve', '--listen', new URL(nodeOrigin).host, '--data-dir', path.join(directory, 'node')]
      if (placement === 'server-node') {
        origin = `http://127.0.0.1:${await freePort()}`
        server = await initializeServer({ directory: path.join(directory, 'server'), origin, environment }); processes.push(server)
        const tenant = server.owner.session.personal_tenant_id
        request = (url, options = {}) => serverRequest(origin, url, { token: server.owner.session.access_token, tenantId: tenant, ...options })
        project = (await request('/projects')).projects[0]
        const { enrollment } = await request(`/tenants/${tenant}/my-computer-enrollments`, { body: { name: 'Search computer', project_id: project.project_id, ttl_seconds: 600 } })
        const { credential } = await request('/enrollments/consume', { body: { token: enrollment.token } })
        executor = enrollment.executor_id
        args.push('--node-id', executor, '--gateway-url', `${origin.replace('http:', 'ws:')}/api/v1/executors/connect`, '--allow-insecure-gateway')
        environment.TERNILO_LOCAL_TOKEN = credential.token
      }
      const node = startProcess(process.env.TERNILO_E2E_NODE_BINARY ?? path.join(repository, 'target/debug/ternilo'), args, environment)
      processes.push(node); await waitForHttp(nodeOrigin, node)
      const local = await localApi(nodeOrigin)
      if (server) await until(() => request('/execution-targets'), value => value.executors.some(item => item.executor_id === executor && item.connected), 'connected search computer')
      else request = local
      await local('/credentials', { body: { name: 'DEEPSEEK_KEY', value: key } })
      const base = `http://127.0.0.1:${upstream.address().port}`
      await local('/providers', { body: { id: 'search-model', display_name: 'Search fixture', base_url: `${base}/v1`, protocol: 'openai-chat-completions', api_key_ref: null, defaults: { context_window: 32000, max_output_tokens: 2048 }, models: [{ id: 'fixture', settings: { mode: 'inherit' } }], timeout_ms: 10000, max_attempts: 1, retry_base_delay_ms: 25 } })
      const folder = path.join(directory, 'workspace'); await mkdir(folder)
      const workspace = server ? (await request('/workspaces', { body: { project_id: project.project_id, name: 'Search workspace', placement: 'local_node', executor_id: executor, path: folder } })).workspace : await request('/workspaces', { body: { path: folder } })
      const session = await request('/sessions', { body: { workspace_id: workspace.workspace_id } })
      const id = session.identity.session_id, resource = `/sessions/${id}`
      await request(resource, { method: 'PATCH', body: { title: 'DeepSeek plugin tools', model: { provider: 'named_provider', provider_id: 'search-model', model: 'fixture' }, profile_plugins: [
        { id: 'web-fetch', kind: 'ternilo.tool.web_fetch', enabled: false, config: {} },
        { id: 'deepseek-search', kind: 'ternilo.web.search.deepseek', enabled: true, config: { base_url: base, api_key_env: 'DEEPSEEK_KEY', max_results: 10 } },
        { id: 'deepseek-fetch', kind: 'ternilo.web.fetch.deepseek', enabled: true, config: { base_url: base, api_key_env: 'DEEPSEEK_KEY' } },
      ] } })
      browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
      page = await browser.newPage({ locale: 'zh-CN', viewport: { width: 1440, height: 960 }, serviceWorkers: 'block' })
      page.on('pageerror', error => errors.push(error.message)); page.on('console', message => { if (message.type() === 'error') errors.push(message.text()) }); page.on('response', response => { if (response.status() >= 400) errors.push(`HTTP ${response.status()} ${new URL(response.url()).pathname}`) })
      await page.goto(origin)
      if (server) {
        await page.getByLabel('用户名', { exact: true }).fill(server.owner.username); await page.getByLabel('密码', { exact: true }).fill(server.owner.password); await page.getByRole('button', { name: '登录', exact: true }).click()
      }
      await page.locator('[data-composer-input]').waitFor()
      await page.evaluate(id => { localStorage.setItem('ternilo.current-session', id); localStorage.setItem('ternilo.transcript-view', 'normal') }, id)
      let expected = 2
      for (const [locale, text] of Object.entries(labels)) {
        mode = 'success'
        await page.evaluate(locale => { localStorage.setItem('ternilo.locale', locale); localStorage.setItem('ternilo.theme', locale === 'en' ? 'light' : 'dark') }, locale); await page.reload()
        await page.getByRole('button', { name: server ? text.userSettings : text.settings, exact: true }).click()
        const settings = page.locator('[data-settings-surface]'), card = settings.locator('[data-plugin-id="deepseek-search"]')
        await settings.getByRole('button', { name: text.plugins, exact: true }).click(); await settings.getByRole('tab', { name: text.overview, exact: true }).click()
        await card.getByLabel(text.results, { exact: true }).fill(String(expected))
        assert.equal(await card.getByLabel(text.credential, { exact: true }).inputValue(), 'DEEPSEEK_KEY')
        await settings.getByRole('tab', { name: text.configuration, exact: true }).click(); await card.locator('button[aria-expanded][aria-label]').click()
        assert.equal(await card.getByLabel(text.results, { exact: true }).inputValue(), String(expected))
        await card.getByRole('button', { name: text.save, exact: true }).click()
        // Individual configuration collapses only after the save callback completes.
        await until(() => card.locator('button[aria-expanded][aria-label]').getAttribute('aria-expanded'), value => value === 'false', 'saved configuration collapsed')
        const state = await request('/state'); const saved = state.sessions.find(item => item.identity.session_id === id)
        assert.equal(saved.profile_plugins.find(item => item.id === 'deepseek-search').config.max_results, expected++)
        await settings.getByRole('tab', { name: text.overview, exact: true }).click()
        await page.setViewportSize({ width: 390, height: 844 })
        assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth))
        await card.getByLabel(text.credential, { exact: true }).scrollIntoViewIfNeeded()
        if (artifacts) await page.screenshot({ path: path.join(artifacts, `settings-${locale}.png`) })
        await settings.locator('[data-settings-toolbar] > button').last().click(); await page.setViewportSize({ width: 1440, height: 960 })
        const previous = (await request(`${resource}/events`)).filter(event => event.type === 'turn_finished').length
        await page.locator('[data-composer-input]').fill('Search, read the page and report the evidence.'); await page.getByRole('button', { name: text.send, exact: true }).click()
        const events = await until(() => request(`${resource}/events`), events => events.filter(event => event.type === 'turn_finished').length > previous || events.some(event => event.type === 'turn_failed'), 'search and fetch loop')
        assert.equal(events.some(event => event.type === 'turn_failed'), false, JSON.stringify(events))
        assert.ok(events.some(event => event.type === 'tool_call_finished' && event.name === 'web_search' && event.output.content.includes('DeepSeek detailed evidence')))
        assert.ok(events.some(event => event.type === 'tool_call_finished' && event.name === 'web_fetch' && event.output.content.includes('# Markdown evidence')))
        assert.ok(!JSON.stringify(events).includes(key)); assert.ok(calls.every(call => call.key === `Bearer ${key}`))
        await page.reload(); await page.getByText('Search and Markdown fetch completed.', { exact: true }).last().waitFor()
        await page.getByText(text.fetch, { exact: true }).last().click()
        await page.locator('[data-tool-view="web-fetch"]').last().getByText('# Markdown evidence', { exact: false }).waitFor()
        if (artifacts) await page.screenshot({ path: path.join(artifacts, `results-${locale}.png`) })
      }
      mode = 'balance'
      await page.locator('[data-composer-input]').fill('Check unavailable search.'); await page.getByRole('button', { name: labels.ko.send, exact: true }).click()
      const failures = await until(() => request(`${resource}/events`), events => events.some(event => event.type === 'tool_call_finished' && event.output?.content?.includes('HTTP 402')), 'safe balance error')
      assert.ok(!JSON.stringify(failures).includes(key) && !JSON.stringify(failures).includes('private-provider-detail'))
      await until(() => request(`${resource}/events`), events => events.filter(event => event.type === 'turn_finished').length === 4, 'error turn settled')
      mode = 'slow'; const before = calls.length
      await page.locator('[data-composer-input]').fill('Check cancellation.'); await page.getByRole('button', { name: labels.ko.send, exact: true }).click()
      await until(async () => calls.length, n => n > before, 'search in progress'); await page.getByRole('button', { name: labels.ko.stop, exact: true }).click()
      await until(async () => cancelled, Boolean, 'search connection cancelled')
      assert.deepEqual(errors, [])
      assert.ok(calls.some(call => call.url === '/v1/tools/search' && call.body.count === 1 && call.body.need_content === false && call.body.query.endsWith('lang:zh,ko')))
      assert.ok(calls.some(call => call.url === '/v1/tools/fetch' && call.body.url === 'https://example.com/article'))
      if (artifacts) await writeFile(path.join(artifacts, 'result.json'), JSON.stringify({ status: 'passed', calls: calls.length, errors }))
    } catch (error) { if (artifacts && page) await page.screenshot({ path: path.join(artifacts, 'failure.png') }).catch(() => {}); throw error }
    finally { await browser?.close(); for (const process of processes.reverse()) await stopProcess(process); upstream.closeAllConnections(); await new Promise(resolve => upstream.close(resolve)); await rm(directory, { recursive: true, force: true }) }
  })
}
