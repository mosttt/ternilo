import assert from 'node:assert/strict'
import { constants } from 'node:fs'
import { createHash } from 'node:crypto'
import { cp, mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import path from 'node:path'
import { tmpdir } from 'node:os'
import test from 'node:test'
import { chromium } from 'playwright'
import { documentModel, githubFixture, deliveryOperations } from './project-delivery-fixture.mjs'
import { localApi, until } from './model-device-fixture.mjs'
import { execute, freePort, initializeServer, repository, serverRequest, startProcess, stopProcess, waitForHttp } from './platform-e2e-fixture.mjs'

test('Server Node and Worker persist project memory and document evidence through the delivery workflow', { timeout: 240_000 }, async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-delivery-platform-'))
  const processes = [], errors = []
  let browser, model, github, page
  try {
    model = await documentModel(); github = await githubFixture()
    const browserPath = process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || chromium.executablePath()
    const policy = JSON.parse(await readFile(path.join(repository, 'examples/worker-policy.json'), 'utf8'))
    policy.minimum_workspace_free_bytes = 0
    policy.allowed_plugin_kinds = [...new Set([...policy.allowed_plugin_kinds, 'ternilo.model.host_gateway', 'ternilo.sandbox.cloud_outer', 'ternilo.files.local', 'ternilo.shell.local', 'ternilo.prompt.workspace_instructions', 'ternilo.tools.files', 'ternilo.tool.shell', 'ternilo.tool.ask_user', 'ternilo.tool.plan', 'ternilo.skills.registry', 'ternilo.skills.filesystem', 'ternilo.tools.skills', 'ternilo.subagents.in_process', 'ternilo.tools.agent_team'])]
    const policyPath = path.join(directory, 'policy.json'); await writeFile(policyPath, JSON.stringify(policy))
    const server = await initializeServer({ directory: path.join(directory, 'server'), origin: `http://127.0.0.1:${await freePort()}`, workerPolicy: policyPath, managedExecutionEnabled: true, environment: { XDG_STATE_HOME: path.join(directory, 'state') } })
    processes.push(server)
    const token = server.owner.session.access_token, tenantId = server.owner.session.personal_tenant_id
    const request = (url, options = {}) => serverRequest(server.origin, url, { token, tenantId, ...options })
    await request('/providers', { body: { id: 'delivery-model', display_name: 'Delivery Model', base_url: `${model.origin}/v1`, protocol: 'openai-chat-completions', api_key_ref: null, defaults: { context_window: 128_000, max_output_tokens: 1024 }, models: [{ id: 'delivery-model', settings: { mode: 'inherit' } }], timeout_ms: 60_000, max_attempts: 1, retry_base_delay_ms: 10 } })
    const selection = { provider: 'account_provider', owner_user_id: server.owner.session.user.user_id, provider_id: 'delivery-model', model: 'delivery-model' }
    const enrolled = await request(`/tenants/${tenantId}/my-computer-enrollments`, { body: { name: 'delivery-node', project_id: null, ttl_seconds: 600 } })
    const credential = await request('/enrollments/consume', { body: { token: enrolled.enrollment.token } })
    const origin = `http://127.0.0.1:${await freePort()}`
    const node = startProcess(process.env.TERNILO_E2E_NODE_BINARY ?? path.join(repository, 'target/debug/ternilo'), ['serve', '--listen', new URL(origin).host, '--data-dir', path.join(directory, 'node'), '--node-id', enrolled.enrollment.executor_id, '--gateway-url', `${server.origin.replace('http:', 'ws:')}/api/v1/executors/connect`, '--allow-insecure-gateway'], { TERNILO_LOCAL_TOKEN: credential.credential.token, XDG_STATE_HOME: path.join(directory, 'state') })
    processes.push(node); await waitForHttp(origin, node)
    const local = await localApi(origin)
    const folder = path.join(directory, 'workspace'); await mkdir(folder)
    const workspace = await local('/workspaces', { body: { path: folder } })
    await local('/sessions', { body: { workspace_id: workspace.workspace_id } })
    const state = await until(() => request('/state'), value => value.sessions.length === 1, 'mapped delivery Node session')
    const nodeId = state.sessions[0].identity.session_id
    const project = (await request('/projects', { body: { name: 'Delivery' } })).project
    const managed = (await request('/workspaces', { body: { project_id: project.project_id, name: 'Managed delivery', placement: 'cloud' } })).workspace
    const cloud = await request('/sessions', { body: { workspace_id: managed.workspace_id, session_id: 'delivery-cloud', permissions: 'workspace_write' } })
    const registration = await request('/admin/workers', { body: { worker_id: 'delivery-worker' } })
    const workerBinary = process.env.TERNILO_CLOUD_E2E_WORKER_BINARY ?? path.join(repository, 'target/debug/ternilo-worker')
    const workerDirectory = path.join(directory, 'worker')
    await execute(workerBinary, ['init', '--config-dir', workerDirectory, '--server-url', server.origin, '--workspace-root', path.join(directory, 'managed-workspaces'), '--sandbox', 'bubblewrap', '--health-listen', '127.0.0.1:0', '--poll-interval-ms', '50'], { cwd: repository, env: { ...process.env, TERNILO_WORKER_TOKEN: registration.token } })
    const worker = startProcess(workerBinary, ['serve', '--config-dir', workerDirectory]); processes.push(worker)
    await until(() => worker.diagnostics(), output => /Ternilo cloud worker .* ready/.test(output), 'delivery Worker ready')
    browser = await chromium.launch({ headless: true, executablePath: browserPath })
    page = await browser.newPage({ viewport: { width: 1280, height: 900 }, locale: 'zh-CN', serviceWorkers: 'block' })
    page.on('pageerror', error => errors.push(error.message)); page.on('console', message => { if (message.type() === 'error') errors.push(message.text()) }); page.on('response', response => { if (response.status() >= 400) errors.push(`${response.status()} ${response.url()}`) })
    await page.goto(server.origin)
    await page.getByLabel('用户名', { exact: true }).fill(server.owner.username); await page.getByLabel('密码', { exact: true }).fill(server.owner.password); await page.getByRole('button', { name: '登录', exact: true }).click()
    for (const id of [nodeId, cloud.identity.session_id]) {
      await request(`/sessions/${id}`, { method: 'PATCH', body: { model: selection, ...(id === cloud.identity.session_id ? { model_token_limit: 262144 } : {}), profile_plugins: [
        { id: 'project-memory', kind: 'ternilo.tools.project_memory', enabled: true, config: {} },
        { id: 'repository-tools', kind: 'ternilo.tools.repository', enabled: true, config: {} },
        { id: 'github-tools', kind: 'ternilo.tools.github', enabled: true, config: { base_url: github.origin } },
        { id: 'document-review', kind: 'ternilo.tools.document_review', enabled: true, config: { browser_command: browserPath } },
      ] } })
      const { run, tool } = deliveryOperations(request, id)
      assert.equal((await tool('read_project_memory', {})).revision, 0)
      await tool('update_project_memory', { key: 'checks', expected_revision: 0, content: 'Verified across execution hosts / 跨执行端验证 / 실행 호스트 간 검증' })
      assert.equal((await tool('read_project_memory', {})).revision, 1)
      await tool('write_file', { path: 'selected.txt', content: 'Selected delivery file' })
      await tool('write_file', { path: 'unrelated.txt', content: 'Unrelated staged work' })
      await tool('shell', { command: 'git init -q && git config user.name DeliveryFixture && git config user.email fixture@example.invalid && git config commit.gpgsign false && git add -- unrelated.txt' })
      await tool('git_commit_paths', { paths: ['selected.txt'], message: 'Commit selected delivery file' })
      assert.ok((await tool('git_inspect', { action: 'diff', staged: true })).operations[0].stdout.includes('unrelated.txt'))
      if (id === nodeId) assert.equal((await tool('github_review_pr', { repository: 'fixture/repo', number: 1 })).pull.head_sha, 'a'.repeat(40))
      else {
        assert.match(await tool('github_review_pr', { repository: 'fixture/repo', number: 1 }, true), /Worker sandbox is offline/)
        // Make the test browser available inside the existing workspace mount; keep the Worker boundary intact.
        const digest = value => createHash('sha256').update(value).digest('hex')
        const managedPath = path.join(directory, 'managed-workspaces', digest(tenantId), digest(managed.workspace_id))
        await cp(path.dirname(browserPath), path.join(managedPath, '.browser-fixture'), { recursive: true, mode: constants.COPYFILE_FICLONE })
        const current = (await request('/state')).sessions.find(session => session.identity.session_id === id)
        await request(`/sessions/${id}`, { method: 'PATCH', body: { profile_plugins: current.profile_plugins.map(entry => entry.id === 'document-review' ? { ...entry, config: { browser_command: `/workspace/.browser-fixture/${path.basename(browserPath)}` } } : entry) } })
      }
      await tool('write_file', { path: 'delivery.html', content: '<html><body><div style="width:1200px">Overflow fixture</div></body></html>' })
      const events = await run('Verify document delivery using actual screenshots.')
      const renders = events.filter(event => event.type === 'document_rendered')
      assert.equal(renders.length, 2, JSON.stringify(events.filter(event => event.type === 'tool_call_finished' || event.type === 'turn_failed')).slice(-5000))
      assert.equal(events.findLast(event => event.type === 'document_reviewed')?.review.verdict, 'pass')
      if (id === cloud.identity.session_id) await stopProcess(worker)
      await page.reload()
      if (id === cloud.identity.session_id) {
        const workspaceRow = page.locator('[data-sidebar-workspace-button]').filter({ hasText: 'Managed delivery' })
        await workspaceRow.waitFor()
        if (await workspaceRow.getAttribute('aria-expanded') === 'false') await workspaceRow.click()
      }
      const sessionRow = page.locator(`[data-sidebar-session-row][data-session-id="${id}"]`)
      await sessionRow.waitFor({ state: 'attached' })
      await sessionRow.locator('[data-sidebar-session-button]').click()
      const row = page.locator('[data-chat-event="document_rendered"]').last(); await row.waitFor({ state: 'attached' })
      const processControl = row.locator('xpath=ancestor::*[@data-chat-turn][1]').locator('[data-turn-process]'); if (await processControl.getAttribute('aria-expanded') === 'false') await processControl.click()
      await row.getByText('该版本已检查', { exact: true }).waitFor()
      await until(() => row.locator('img').evaluateAll(images => images.length && images.every(image => image.complete && image.naturalWidth > 0)), value => value, 'relayed durable screenshots')
      assert.equal(await row.locator('img').first().evaluate(image => getComputedStyle(image).objectFit), 'contain')
      await row.scrollIntoViewIfNeeded()
      if (process.env.TERNILO_E2E_ARTIFACT_DIR) { await mkdir(process.env.TERNILO_E2E_ARTIFACT_DIR, { recursive: true }); await page.screenshot({ path: path.join(process.env.TERNILO_E2E_ARTIFACT_DIR, `delivery-${id === nodeId ? 'node' : 'worker'}.png`) }) }
      await row.locator('[data-message-image-thumbnail]').first().click()
      const preview = page.locator('[data-message-image-lightbox]')
      await preview.waitFor()
      assert.equal(await preview.locator('img').evaluate(image => image.naturalWidth), 320)
      await page.keyboard.press('Escape')
      await preview.waitFor({ state: 'detached' })
    }
    assert.deepEqual(errors, [])
    console.log(JSON.stringify({ node: 'passed', worker: 'passed', memory: true, scopedGit: true, nodeGithubRead: true, workerNetworkBoundary: true, renderedEvidence: true, errors }))
  } catch (error) { for (const process of processes) console.error(process.diagnostics().slice(-5000)); if (page && !page.isClosed()) console.error((await page.locator('body').innerText()).slice(-4000)); throw error }
  finally { await browser?.close(); for (const process of processes.reverse()) await stopProcess(process); await model?.close(); await github?.close(); await rm(directory, { recursive: true, force: true }) }
})
