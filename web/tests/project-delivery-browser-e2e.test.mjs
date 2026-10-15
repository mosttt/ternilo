import assert from 'node:assert/strict'
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { chromium } from 'playwright'
import { localApi, until } from './model-device-fixture.mjs'
import { execute, freePort, repository, startProcess, stopProcess, waitForHttp } from './platform-e2e-fixture.mjs'

import { documentModel, githubFixture, verifyReleaseQueries, verifyDocumentMath, rhai, untilOperation } from './project-delivery-fixture.mjs'

test('project memory, scoped Git commits, GitHub contracts and rendered document reviews complete the delivery workflow', { timeout: 240_000 }, async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-project-delivery-'))
  let app, browser, github, model, request, activePage
  let lastEvents = []
  const errors = []
  try {
    github = await githubFixture(); model = await documentModel()
    const origin = `http://127.0.0.1:${await freePort()}`
    const binary = process.env.TERNILO_E2E_NODE_BINARY ?? path.join(repository, 'target/debug/ternilo')
    const browserPath = process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || chromium.executablePath()
    const args = ['serve', '--listen', new URL(origin).host, '--data-dir', path.join(directory, 'data')]
    const env = { ...Object.fromEntries(Object.keys(process.env).filter(key => key.startsWith('TERNILO_')).map(key => [key, undefined])), XDG_STATE_HOME: path.join(directory, 'state') }
    app = startProcess(binary, args, env); await waitForHttp(origin, app); request = await localApi(origin)
    const folder = path.join(directory, 'workspace'); await mkdir(folder)
    const workspace = await request('/workspaces', { body: { path: folder } })
    const session = await request('/sessions', { body: { workspace_id: workspace.workspace_id } })
    const id = session.identity.session_id, resource = `/sessions/${id}`
    await request('/credentials', { body: { name: 'DELIVERY_GITHUB_TOKEN', value: 'synthetic-github-token' } })
    await request(resource, { method: 'PATCH', body: { profile_plugins: [
      { id: 'github-tools', kind: 'ternilo.tools.github', enabled: true, config: { base_url: github.origin, credential_ref: 'DELIVERY_GITHUB_TOKEN' } },
      { id: 'document-review', kind: 'ternilo.tools.document_review', enabled: true, config: { browser_command: browserPath } },
    ] } })
    const run = async (sessionId, input, approvals = true) => {
      const before = (await request(`/sessions/${sessionId}/events`)).at(-1)?.seq ?? -1
      await request(`/sessions/${sessionId}/queue`, { body: { content: { kind: 'prompt', input } } })
      const events = await untilOperation(async () => {
        const questions = await request(`/questions?session_id=${sessionId}`)
        if (!approvals) assert.equal(questions.length, 0, 'read-only write must not become an approval escalation')
        for (const pending of questions) await request(`/questions/${pending.question.id}/answer`, { body: { selected: ['Allow once'], custom: null } })
        lastEvents = await request(`/sessions/${sessionId}/events`)
        return lastEvents
      }, events => events.some(event => event.seq > before && ['turn_finished', 'turn_failed', 'turn_cancelled'].includes(event.type)), 'completed delivery operation')
      await until(() => request(`/sessions/${sessionId}/queue`), queue => !queue.active_run_id, 'idle delivery queue')
      return events.filter(event => event.seq > before)
    }
    const tool = async (name, args, expectedError = false, sessionId = id, approvals = true) => {
      const events = await run(sessionId, `/code call_tool(${JSON.stringify(name)}, ${rhai(args)})`, approvals)
      const event = events.findLast(event => event.type === 'code_dispatch_finished' && event.name === name)
      assert.ok(event, `missing ${name}: ${JSON.stringify(events).slice(-1500)}`)
      assert.equal(event.output.is_error, expectedError, event.output.content)
      return expectedError ? event.output.content : JSON.parse(event.output.content)
    }
    assert.equal((await tool('read_project_memory', {})).revision, 0)
    await tool('update_project_memory', { key: 'checks', expected_revision: 0, content: 'Run relevant checks / 执行验证 / 검사 실행' })
    await tool('update_project_memory', { key: 'stale', expected_revision: 0, content: 'Must not overwrite' }, true)
    const second = await request('/sessions', { body: { workspace_id: workspace.workspace_id } })
    assert.ok((await tool('read_project_memory', {}, false, second.identity.session_id)).notes.checks.content.includes('执行验证'))
    await request(resource, { method: 'PATCH', body: { permissions: 'read_only' } })
    await tool('update_project_memory', { key: 'denied', expected_revision: 1, content: 'Must not be written' }, true, id, false)
    await request(resource, { method: 'PATCH', body: { permissions: 'workspace_write' } })
    assert.equal((await tool('read_project_memory', {})).revision, 1)
    const git = args => execute('git', args, { cwd: folder, env: process.env })
    await git(['init', '-q']); await git(['config', 'user.name', 'Delivery Fixture']); await git(['config', 'user.email', 'fixture@example.invalid']); await git(['config', 'commit.gpgsign', 'false'])
    await writeFile(path.join(folder, 'selected.txt'), 'Selected change'); await writeFile(path.join(folder, 'unrelated.txt'), 'Keep staged')
    await git(['add', '--', 'unrelated.txt'])
    await tool('git_inspect', { action: 'status' })
    await tool('git_commit_paths', { paths: ['selected.txt'], message: 'Commit only selected work' })
    const staged = (await tool('git_inspect', { action: 'diff', staged: true })).operations[0].stdout
    assert.ok(staged.includes('unrelated.txt')); assert.ok(!staged.includes('selected.txt'))
    await request(resource, { method: 'PATCH', body: { permissions: 'read_only' } })
    await verifyReleaseQueries((name, args, expectedError) => tool(name, args, expectedError, id, false), github)
    await request(resource, { method: 'PATCH', body: { permissions: 'workspace_write' } })
    const reviewed = await tool('github_review_pr', { repository: 'fixture/repo', number: 1 })
    assert.equal(reviewed.pull.head_sha, 'a'.repeat(40)); assert.equal(reviewed.has_more_files, true)
    github.state.flip = true
    assert.ok((await tool('github_review_pr', { repository: 'fixture/repo', number: 1 }, true)).includes('changed'))
    github.state.flip = false; github.state.head = 'a'.repeat(40)
    const created = await tool('github_create_pr', { repository: 'fixture/repo', title: 'Reviewed change', body: 'Validation completed.\nLiteral $(text) stays text.', head: 'feature', base: 'main' })
    assert.equal(created.draft, true)
    github.state.denyMerge = true
    await tool('github_merge_pr', { repository: 'fixture/repo', number: 1, expected_head: 'a'.repeat(40) }, true)
    github.state.denyMerge = false
    await tool('github_merge_pr', { repository: 'fixture/repo', number: 1, expected_head: 'a'.repeat(40) })
    assert.ok(github.state.requests.filter(request => ['POST', 'PUT'].includes(request.method)).every(request => request.authorization === 'Bearer synthetic-github-token'))
    assert.equal(github.state.requests.find(request => request.method === 'PUT').body.sha, 'a'.repeat(40))
    await writeFile(path.join(folder, 'delivery.html'), '<html><body><div style="width:1200px">Overflow fixture / 页面溢出 / 넘침</div></body></html>')
    await request('/providers', { body: { id: 'delivery-model', display_name: 'Delivery Model', base_url: `${model.origin}/v1`, protocol: 'openai-chat-completions', api_key_ref: null, defaults: { context_window: 128_000, max_output_tokens: 1024 }, models: [{ id: 'delivery-model', settings: { mode: 'inherit' } }], timeout_ms: 60_000, max_attempts: 1, retry_base_delay_ms: 10 } })
    await request(resource, { method: 'PATCH', body: { model: { provider: 'named_provider', provider_id: 'delivery-model', model: 'delivery-model' } } })
    const events = await run(id, 'Verify document delivery using actual screenshots.')
    assert.ok(events.some(event => event.type === 'assistant_message' && event.response.content === 'Document delivery verified.'), JSON.stringify(events).slice(-2500))
    const renders = events.filter(event => event.type === 'document_rendered').map(event => event.render)
    assert.equal(renders.length, 2); assert.equal(renders[0].horizontal_overflow, true); assert.equal(renders[1].horizontal_overflow, false)
    assert.notEqual(renders[0].source_sha256, renders[1].source_sha256)
    assert.ok(events.some(event => event.type === 'tool_call_finished' && event.call_id === 'doc-reject' && event.output.is_error))
    assert.ok(events.some(event => event.type === 'tool_call_finished' && event.call_id === 'doc-stale' && event.output.is_error))
    const review = events.findLast(event => event.type === 'document_reviewed').review
    assert.equal(review.verdict, 'pass'); assert.equal(review.render_id, renders[1].render_id)
    assert.ok(model.requests.some(request => request.messages.some(message => Array.isArray(message.content) && message.content.some(part => part.type === 'image_url' && part.image_url.url.startsWith('data:image/png;base64,')))))
    await verifyDocumentMath(tool)
    await stopProcess(app); app = startProcess(binary, args, env); await waitForHttp(origin, app); request = await localApi(origin)
    assert.equal((await tool('read_project_memory', {})).notes.checks.content, 'Run relevant checks / 执行验证 / 검사 실행')
    assert.equal((await request(`${resource}/events`)).filter(event => event.type === 'document_rendered').length, 5)
    browser = await chromium.launch({ headless: true, executablePath: browserPath })
    for (const [locale, label, renderLabel] of [['zh', '该版本已检查', '渲染文档'], ['en', 'This version was reviewed', 'Render document'], ['ko', '이 버전 검토 완료', '문서 렌더링']]) {
      const context = await browser.newContext({ viewport: locale === 'ko' ? { width: 320, height: 560 } : { width: 1280, height: 900 }, serviceWorkers: 'block', hasTouch: true })
      await context.addInitScript(({ locale, id, workspaceId }) => { localStorage.setItem('ternilo.locale', locale); localStorage.setItem('ternilo.current-session', id); localStorage.setItem('ternilo.current-workspace', workspaceId) }, { locale, id, workspaceId: workspace.workspace_id })
      const page = await context.newPage(); activePage = page
      page.on('pageerror', error => errors.push(error.message)); page.on('console', message => { if (message.type() === 'error') errors.push(message.text()) }); page.on('response', response => { if (response.status() >= 400) errors.push(`${response.status()} ${response.url()}`) })
      await page.goto(origin)
      const row = page.locator('[data-chat-event="document_rendered"]').last(); await row.waitFor({ state: 'attached' })
      const processControl = row.locator('xpath=ancestor::*[@data-chat-turn][1]').locator('[data-turn-process]'); if (await processControl.count() && await processControl.getAttribute('aria-expanded') === 'false') await processControl.click()
      await row.getByText(label, { exact: true }).waitFor()
      await row.locator('xpath=ancestor::*[@data-chat-turn][1]').locator('[data-tool-call-tree]').getByText(renderLabel, { exact: true }).waitFor()

      await until(() => row.locator('img').evaluateAll(images => images.length && images.every(image => image.complete && image.naturalWidth > 0)), value => value, 'resolved durable document screenshots')
      assert.equal(await row.locator('img').first().evaluate(image => getComputedStyle(image).objectFit), 'contain')
      await row.scrollIntoViewIfNeeded()
      const bounds = await row.boundingBox(); assert.ok(bounds.x >= 0 && bounds.x + bounds.width <= page.viewportSize().width)
      if (process.env.TERNILO_E2E_ARTIFACT_DIR) { await mkdir(process.env.TERNILO_E2E_ARTIFACT_DIR, { recursive: true }); await page.screenshot({ path: path.join(process.env.TERNILO_E2E_ARTIFACT_DIR, `document-review-${locale}.png`) }) }
      await row.locator('[data-message-image-thumbnail]').first().click()
      const preview = page.locator('[data-message-image-lightbox]')
      await preview.waitFor()
      assert.equal(await preview.locator('img').evaluate(image => image.naturalWidth), 320)
      if (process.env.TERNILO_E2E_ARTIFACT_DIR) await preview.locator('img').screenshot({ path: path.join(process.env.TERNILO_E2E_ARTIFACT_DIR, `math-${locale}.png`) })
      await page.keyboard.press('Escape')
      await preview.waitFor({ state: 'detached' })
      await tool('github_release_status', { repository: 'fixture/repo', tag: 'v1' })
      const releaseTitle = page.locator('[data-tool-call-tree]').getByText({ zh: '查询发布状态', en: 'Check release status', ko: '릴리스 상태 조회' }[locale], { exact: true }).last()
      await releaseTitle.waitFor({ state: 'attached' })
      await releaseTitle.scrollIntoViewIfNeeded()
      if (process.env.TERNILO_E2E_ARTIFACT_DIR) await page.screenshot({ path: path.join(process.env.TERNILO_E2E_ARTIFACT_DIR, `release-status-${locale}.png`) })
      await context.close()
    }
    assert.deepEqual(errors, [])
    console.log(JSON.stringify({ memoryPersistent: true, staleWriteRejected: true, readOnlyEnforced: true, unrelatedIndexPreserved: true, githubDraftAndHeadContracts: true, screenshotContext: true, staleReviewRejected: true, correctedDocumentReviewed: true, locales: ['zh', 'en', 'ko'], errors }))
  } catch (error) { console.error(app?.diagnostics().slice(-5000)); console.error(JSON.stringify(lastEvents.slice(-12)).slice(-12000)); if (activePage && !activePage.isClosed()) console.error((await activePage.locator('body').innerText()).slice(-6000)); throw error }
  finally { await browser?.close(); await stopProcess(app); await github?.close(); await model?.close(); await rm(directory, { recursive: true, force: true }) }
})
