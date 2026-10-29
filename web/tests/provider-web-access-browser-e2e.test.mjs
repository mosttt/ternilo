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
  zh: { settings: '设置', models: '模型', edit: '编辑', search: '网页搜索', fetch: '网页读取', computer: '外部集成 · DeepSeek', key: '搜索 API Key', advanced: '网页服务高级设置', searchUrl: '搜索服务地址', fetchUrl: '读取服务地址', results: '最多搜索结果', save: '保存', send: '发送', stop: '停止运行' },
  en: { settings: 'Settings', models: 'Models', edit: 'Edit', search: 'Web search', fetch: 'Page reading', computer: 'External integration · DeepSeek', key: 'Search API key', advanced: 'Advanced web service settings', searchUrl: 'Search service address', fetchUrl: 'Page reading service address', results: 'Maximum search results', save: 'Save', send: 'Send', stop: 'Stop run' },
  ko: { settings: '설정', models: '모델', edit: '편집', search: '웹 검색', fetch: '웹페이지 읽기', computer: '외부 연동 · DeepSeek', key: '검색 API 키', advanced: '웹 서비스 고급 설정', searchUrl: '검색 서비스 주소', fetchUrl: '읽기 서비스 주소', results: '최대 검색 결과 수', save: '저장', send: '전송', stop: '실행 중지' },
}
async function choose(page, field, label) {
  await field.click(); await page.getByRole('option', { name: label, exact: true }).click()
}

for (const placement of ['local', 'server-node']) {
  test(`Provider web settings share one configuration and execute on the task computer (${placement})`, { timeout: 150_000 }, async () => {
    const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-provider-web-'))
    const artifacts = process.env.TERNILO_E2E_ARTIFACT_DIR ? path.join(process.env.TERNILO_E2E_ARTIFACT_DIR, `provider-web-${placement}`) : undefined
    if (artifacts) await mkdir(artifacts, { recursive: true })
    const processes = [], calls = [], errors = []
    let browser, page, mode = 'success', cancelled = false, closing = false
    const key = 'fixture-deepseek-tools-secret'
    const upstream = createServer(async (request, response) => {
      const chunks = []; for await (const chunk of request) chunks.push(chunk)
      const body = JSON.parse(Buffer.concat(chunks))
      if (request.url === '/v1/chat/completions') {
        assert.equal(request.headers.authorization, 'Bearer fixture-model-only-key')
        const names = (body.tools ?? []).map(tool => tool.function?.name)
        if (names.length) { const expected = mode === 'disabled' ? 0 : 1; assert.equal(names.filter(name => name === 'web_search').length, expected); assert.equal(names.filter(name => name === 'web_fetch').length, expected) }
        const lastUser = body.messages.findLastIndex(item => item.role === 'user')
        const done = body.messages.slice(lastUser + 1).filter(item => item.role === 'tool')
        const tool = mode === 'disabled' || !names.length ? null : done.length === 0 ? 'web_search' : done.length === 1 ? 'web_fetch' : null
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
      await request('/credentials', { body: { name: 'MODEL_KEY', value: 'fixture-model-only-key' } })
      const base = `http://127.0.0.1:${upstream.address().port}`
      await request('/providers', { body: { id: 'search-model', display_name: 'Search fixture', base_url: `${base}/v1`, protocol: 'openai-chat-completions', api_key_ref: 'MODEL_KEY', defaults: { context_window: 32000, max_output_tokens: 2048 }, models: [{ id: 'fixture', settings: { mode: 'inherit' } }, { id: 'fixture-custom', settings: { mode: 'inherit' } }, { id: 'fixture-off', settings: { mode: 'inherit' }, web_access: { search: { mode: 'disabled' }, fetch: { mode: 'disabled' } } }], timeout_ms: 10000, max_attempts: 1, retry_base_delay_ms: 25 } })
      const folder = path.join(directory, 'workspace'); await mkdir(folder)
      const workspace = server ? (await request('/workspaces', { body: { project_id: project.project_id, name: 'Search workspace', placement: 'local_node', executor_id: executor, path: folder } })).workspace : await request('/workspaces', { body: { path: folder } })
      const selection = server ? { provider: 'account_provider', owner_user_id: server.owner.session.user.user_id, provider_id: 'search-model', model: 'fixture' } : { provider: 'named_provider', provider_id: 'search-model', model: 'fixture' }
      await request('/default-model', { method: 'PUT', body: selection })
      const session = await request('/sessions', { body: { workspace_id: workspace.workspace_id } })
      const id = session.identity.session_id, resource = `/sessions/${id}`
      await request(resource, { method: 'PATCH', body: { title: 'Provider web tools', model: selection, profile_plugins: [
        { id: 'web-fetch', kind: 'ternilo.tool.web_fetch', enabled: false, config: {} },
        { id: 'legacy-fetch', kind: 'ternilo.web.fetch.deepseek', enabled: true, config: { base_url: base, api_key_env: 'UNSET_FIXTURE_LEGACY_WEB_KEY' } },
      ] } })
      browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined })
      page = await browser.newPage({ locale: 'zh-CN', viewport: { width: 1440, height: 960 }, serviceWorkers: 'block' })
      browser.on('disconnected', () => { if (!closing) errors.push('Browser disconnected before cleanup') }); page.on('close', () => { if (!closing) errors.push('Page closed before cleanup') })
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
        if (server) await page.goto(`${origin}/models?source=account`)
        else {
          await page.getByRole('button', { name: text.settings, exact: true }).click()
          await page.locator('[data-settings-surface]').getByRole('button', { name: text.models, exact: true }).click()
        }
        await page.getByRole('button', { name: text.edit, exact: true }).click()
        const editor = page.locator('[data-provider-editor="search-model"]')
        await editor.locator('details > summary').first().click()
        if (locale === 'zh') {
          await choose(page, editor.getByLabel(text.search, { exact: true }), text.computer)
          await choose(page, editor.getByLabel(text.fetch, { exact: true }), text.computer)
          await editor.getByLabel(text.key, { exact: true }).fill(key)
        } else assert.equal(await editor.getByLabel(text.key, { exact: true }).inputValue(), '')
        await editor.getByText(text.advanced, { exact: true }).click()
        await editor.getByLabel(text.searchUrl, { exact: true }).fill(base)
        await editor.getByLabel(text.fetchUrl, { exact: true }).fill(base)
        await editor.getByLabel(text.results, { exact: true }).fill(String(expected))
        await page.setViewportSize({ width: 390, height: 844 })
        await editor.locator('[data-provider-web-access]').scrollIntoViewIfNeeded()
        assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth))
        if (artifacts) await page.screenshot({ path: path.join(artifacts, `settings-${locale}.png`) })
        const detailLabel = { zh: '模型详细设置 2', en: 'Model details 2', ko: '모델 세부 정보 2' }[locale]
        await editor.getByRole('button', { name: detailLabel, exact: true }).click()
        const modelWeb = editor.locator('[data-provider-web-access="fixture-custom"]')
        const inherit = { zh: '继承 Provider · ', en: 'Inherit Provider · ', ko: 'Provider 상속 · ' }[locale] + text.computer
        const off = { zh: '关闭', en: 'Off', ko: '끄기' }[locale]
        await choose(page, modelWeb.getByLabel(text.search, { exact: true }), off)
        await choose(page, modelWeb.getByLabel(text.search, { exact: true }), inherit)
        assert.equal(await modelWeb.getByLabel(text.key, { exact: true }).count(), 0, 'inherited credentials are not duplicated inputs')
        await choose(page, modelWeb.getByLabel(text.search, { exact: true }), text.computer)
        await modelWeb.getByText(text.advanced, { exact: true }).click()
        await modelWeb.getByLabel(text.results, { exact: true }).fill(String(expected + 5))
        assert.equal(await modelWeb.getByLabel(text.fetch, { exact: true }).textContent(), inherit)
        if (artifacts) { await modelWeb.scrollIntoViewIfNeeded(); await page.screenshot({ path: path.join(artifacts, `model-${locale}.png`) }) }
        await editor.getByRole('button', { name: text.save, exact: true }).click()
        await editor.waitFor({ state: 'hidden' })
        const saved = (await request('/providers')).find(item => item.id === 'search-model')
        assert.equal(saved.models[0].web_access, undefined)
        assert.equal(saved.models[1].web_access.search.max_results, expected + 5)
        assert.equal(saved.models[1].web_access.search.api_key_ref, saved.web_access.search.api_key_ref)
        assert.equal(saved.models[1].web_access.fetch, undefined)
        await request(resource, { method: 'PATCH', body: { model: { ...selection, model: 'fixture-custom' } } })
        assert.equal(saved.web_access.search.max_results, expected++)
        assert.equal(saved.web_access.search.api_key_ref, saved.web_access.fetch.api_key_ref)
        assert.notEqual(saved.api_key_ref, saved.web_access.search.api_key_ref)
        assert.ok(!JSON.stringify(saved).includes(key))
        if (server) {
          assert.ok(!(await local('/credentials')).references.some(item => item.reference === saved.web_access.search.api_key_ref))
          await page.goto(origin)
        } else await page.locator('[data-settings-toolbar] > button').last().click()
        await page.setViewportSize({ width: 1440, height: 960 })
        const previous = (await request(`${resource}/events`)).filter(event => event.type === 'turn_finished').length
        await page.locator('[data-composer-input]').fill('Search, read the page and report the evidence.'); await page.getByRole('button', { name: text.send, exact: true }).click()
        const events = await until(() => request(`${resource}/events`), events => events.filter(event => event.type === 'turn_finished').length > previous || events.some(event => event.type === 'turn_failed'), 'search and fetch loop')
        assert.equal(events.some(event => event.type === 'turn_failed'), false, JSON.stringify(events))
        assert.ok(events.some(event => event.type === 'tool_call_finished' && event.name === 'web_search' && event.output.content.includes('DeepSeek detailed evidence')))
        assert.ok(events.some(event => event.type === 'tool_call_finished' && event.name === 'web_fetch' && event.output.content.includes('# Markdown evidence')))
        assert.ok(!JSON.stringify(events).includes(key)); assert.ok(calls.every(call => call.key === `Bearer ${key}`))
        await page.reload(); await page.getByText('Search and Markdown fetch completed.', { exact: true }).last().waitFor()
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
      await until(() => request(`${resource}/queue`), inbox => !inbox.active_run_id, 'cancelled task settled')
      mode = 'success'
      if (server) {
        const tenant = server.owner.session.personal_tenant_id
        const { enrollment } = await request(`/tenants/${tenant}/my-computer-enrollments`, { body: { name: 'Model source only', ttl_seconds: 600 } })
        const { credential } = await request('/enrollments/consume', { body: { token: enrollment.token } })
        const sourceOrigin = `http://127.0.0.1:${await freePort()}`
        const sourceProcess = startProcess(process.env.TERNILO_E2E_NODE_BINARY ?? path.join(repository, 'target/debug/ternilo'), ['serve', '--data-dir', path.join(directory, 'source'), '--listen', new URL(sourceOrigin).host, '--node-id', enrollment.executor_id, '--gateway-url', `${origin.replace('http:', 'ws:')}/api/v1/executors/connect`, '--allow-insecure-gateway'], { ...environment, TERNILO_LOCAL_TOKEN: credential.token })
        processes.push(sourceProcess); await waitForHttp(sourceOrigin, sourceProcess)
        const source = await localApi(sourceOrigin)
        const provider = (await request('/providers')).find(item => item.id === 'search-model')
        delete provider.source // Read-only directory metadata is not part of a writable Provider.
        await source('/credentials', { body: { name: 'MODEL_KEY', value: 'fixture-model-only-key' } })
        await source('/credentials', { body: { name: provider.web_access.search.api_key_ref, value: key } })
        await source('/providers', { body: provider })
        await until(() => request('/execution-targets'), value => value.executors.some(item => item.executor_id === enrollment.executor_id && item.connected), 'model source connected')
        await request(resource, { method: 'PATCH', body: { model: { provider: 'computer_provider', executor_id: enrollment.executor_id, provider_id: provider.id, model: 'fixture-custom' } } })
        const before = calls.length
        const previous = (await request(`${resource}/events`)).filter(event => event.type === 'turn_finished').length
        await page.reload(); await page.locator('[data-composer-input]').fill('Use the selected source Provider and execute its tools here.')
        await page.getByRole('button', { name: labels.ko.send, exact: true }).click()
        await until(() => request(`${resource}/events`), events => events.filter(event => event.type === 'turn_finished').length > previous, 'cross-computer Provider web loop')
        assert.equal(calls.length - before, 2)
        assert.equal((await source('/state')).sessions.length, 0, 'model source must not execute the task or tool session')
        assert.ok(!(await local('/credentials')).references.some(item => item.reference === provider.web_access.search.api_key_ref), 'delegated key must not become a device credential')
        await source(`/credentials/${provider.web_access.search.api_key_ref}`, { method: 'DELETE' })
        const beforeMissing = calls.length
        const finished = (await request(`${resource}/events`)).filter(event => event.type === 'turn_finished').length
        await page.locator('[data-composer-input]').fill('Verify revoked web credentials are not reused.'); await page.getByRole('button', { name: labels.ko.send, exact: true }).click()
        const missing = await until(() => request(`${resource}/events`), events => events.filter(event => event.type === 'turn_finished').length > finished, 'revoked web key handled')
        assert.equal(calls.length, beforeMissing, 'missing web credentials must not fall back to the model key')
        assert.ok(missing.some(event => event.type === 'tool_call_finished' && event.output?.is_error && event.output.content.includes('web credential')))
      }
      const originalProvider = (await request('/providers')).find(item => item.id === 'search-model')
      delete originalProvider.source
      await request('/providers', { body: { ...originalProvider, id: 'without-web', display_name: 'No web tools', hosted_tools: null, web_access: { search: { mode: 'disabled' }, fetch: { mode: 'disabled' } } } })
      for (const enabled of [false, true]) {
        mode = enabled ? 'success' : 'disabled'
        await request(resource, { method: 'PATCH', body: { model: { ...selection, provider_id: enabled ? 'search-model' : 'without-web' } } })
        const before = calls.length, finished = (await request(`${resource}/events`)).filter(event => event.type === 'turn_finished').length
        await page.reload(); await page.locator('[data-composer-input]').fill('Use the newly selected Provider settings.')
        await page.getByRole('button', { name: labels.ko.send, exact: true }).click()
        await until(() => request(`${resource}/events`), events => events.filter(event => event.type === 'turn_finished').length > finished, 'Provider switch applied')
        assert.equal(calls.length - before, enabled ? 2 : 0)
      }
      mode = 'disabled'
      await request(resource, { method: 'PATCH', body: { model: { ...selection, model: 'fixture-off' } } })
      const beforeOff = calls.length, finishedOff = (await request(`${resource}/events`)).filter(event => event.type === 'turn_finished').length
      await page.reload(); await page.locator('[data-composer-input]').fill('Use the model override, not the Provider defaults.')
      await page.getByRole('button', { name: labels.ko.send, exact: true }).click()
      await until(() => request(`${resource}/events`), events => events.filter(event => event.type === 'turn_finished').length > finishedOff, 'disabled model finishes')
      assert.equal(calls.length, beforeOff)
      mode = 'success'
      await request(resource, { method: 'PATCH', body: { model: selection } }); await page.reload()
      if (!server) {
        await page.getByRole('button', { name: labels.ko.settings, exact: true }).click()
        const settings = page.locator('[data-settings-surface]')
        await settings.getByRole('button', { name: '플러그인', exact: true }).click()
        for (const tab of ['통합 설정', '플러그인 구성']) {
          await settings.getByRole('tab', { name: tab, exact: true }).click()
          const card = settings.locator('[data-provider-managed][data-plugin-id="provider-web-search"]')
          await card.getByText(String(originalProvider.web_access.search.max_results), { exact: true }).waitFor()
          assert.equal(await card.getByRole('button', { name: labels.ko.save, exact: true }).count(), 0)
          await card.scrollIntoViewIfNeeded()
        }
        if (artifacts) await page.screenshot({ path: path.join(artifacts, 'provider-managed-ko.png') })
        await settings.locator('[data-settings-toolbar] > button').last().click()
      }
      if (!server) {
        await request(`/credentials/${originalProvider.web_access.search.api_key_ref}`, { method: 'DELETE' })
        await page.reload(); await page.getByRole('button', { name: labels.ko.settings, exact: true }).click()
        const settings = page.locator('[data-settings-surface]')
        await settings.getByRole('button', { name: labels.ko.models, exact: true }).click()
        await settings.locator('[data-provider-id="search-model"]').getByRole('button', { name: labels.ko.edit, exact: true }).click()
        const editor = settings.locator('[data-provider-editor="search-model"]')
        await editor.locator('details > summary').first().click()
        assert.equal(await editor.getByLabel(labels.ko.key, { exact: true }).getAttribute('placeholder'), '웹 서비스 API 키를 입력하세요')
        if (artifacts) { await editor.locator('[data-provider-web-access]').scrollIntoViewIfNeeded(); await page.screenshot({ path: path.join(artifacts, 'missing-credential-ko.png') }) }
      }
      assert.deepEqual(errors, [])
      assert.ok(calls.some(call => call.url === '/v1/tools/search' && call.body.count === 1 && call.body.need_content === false && call.body.query.endsWith('lang:zh,ko')))
      assert.ok(calls.some(call => call.url === '/v1/tools/fetch' && call.body.url === 'https://example.com/article'))
      if (artifacts) await writeFile(path.join(artifacts, 'result.json'), JSON.stringify({ status: 'passed', calls: calls.length, errors }))
    } catch (error) { if (artifacts) { await writeFile(path.join(artifacts, 'failure.json'), JSON.stringify({ message: error.message, errors, calls: calls.length })); if (page) await page.screenshot({ path: path.join(artifacts, 'failure.png') }).catch(() => {}) } throw error }
    finally { closing = true; await browser?.close(); for (const process of processes.reverse()) await stopProcess(process); upstream.closeAllConnections(); await new Promise(resolve => upstream.close(resolve)); await rm(directory, { recursive: true, force: true }) }
  })
}
