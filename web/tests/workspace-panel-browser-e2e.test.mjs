import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { chmod, mkdir, mkdtemp, readFile, rm, symlink, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { chromium } from 'playwright'
import { selectSpace, freePort, initializeServer, repository, serverRequest, startProcess, stopProcess, waitForHttp } from './platform-e2e-fixture.mjs'

async function until(read, ready, label) {
  const deadline = Date.now() + 30_000
  while (Date.now() < deadline) {
    const value = await read()
    if (ready(value)) return value
    await new Promise(resolve => setTimeout(resolve, 100))
  }
  throw new Error(`Timed out: ${label}`)
}

test('Server workspace files work on insecure HTTP origins with real Node storage, isolated previews and workspace-only sharing', { timeout: 240_000 }, async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-workspace-panel-'))
  const artifacts = process.env.TERNILO_E2E_ARTIFACT_DIR
  const processes = [], errors = []
  let browser, page
  try {
    const application = await initializeServer({ directory: path.join(directory, 'server'), origin: `http://127.0.0.1:${await freePort()}` })
    processes.push(application)
    const owner = (resource, options = {}) => serverRequest(application.origin, resource, { token: application.owner.session.access_token, ...options })
    const invitation = await owner('/admin/invitations', { body: { tenant_id: null, role: 'member', expires_in_seconds: 3600 } })
    const memberAccount = { username: 'workspace-viewer', password: 'workspace-viewer-password' }
    const member = await serverRequest(application.origin, '/auth/invitations/accept', { body: { token: invitation.token, email: 'workspace-viewer@example.test', ...memberAccount } })
    const team = (await owner('/tenants', { body: { slug: 'workspace-files', display_name: 'Workspace files' } })).tenant
    await owner(`/tenants/${team.tenant_id}/members/${member.user.user_id}`, { method: 'PUT', body: { role: 'member' } })
    const project = (await owner('/projects', { tenantId: team.tenant_id })).projects[0]
    const enrollment = (await owner(`/tenants/${team.tenant_id}/my-computer-enrollments`, { body: { executor_id: 'files-node', project_id: project.project_id, ttl_seconds: 600 } })).enrollment
    const credential = (await serverRequest(application.origin, '/enrollments/consume', { body: { token: enrollment.token } })).credential
    const folder = path.join(directory, 'workspace with spaces;literal')
    const binaries = path.join(directory, 'bin')
    const desktopData = path.join(directory, 'desktop-data')
    await mkdir(path.join(folder, 'src'), { recursive: true })
    await mkdir(binaries)
    await mkdir(path.join(desktopData, 'applications'), { recursive: true })
    await mkdir(path.join(desktopData, 'icons/hicolor/scalable/apps'), { recursive: true })
    await writeFile(path.join(desktopData, 'applications/code.desktop'), '[Desktop Entry]\nIcon=fixture-code\n')
    await writeFile(path.join(desktopData, 'icons/hicolor/scalable/apps/fixture-code.svg'), '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 18 18"><path fill="#23a9f2" d="M2 2h14v14H2z"/></svg>')
    await writeFile(path.join(folder, 'src/main.rs'), 'fn main() { println!("workspace preview"); }\n')
    await writeFile(path.join(folder, 'README.md'), '# Workspace preview\n\nMarkdown body.')
    await writeFile(path.join(folder, 'unsafe.html'), '<h1>Isolated HTML</h1><script>parent.__workspaceInjected = true</script><img src="https://workspace.invalid/tracker"><form action="https://workspace.invalid/post"></form>')
    await writeFile(path.join(folder, 'large.txt'), '好'.repeat(800_000))
    await writeFile(path.join(folder, 'image.png'), Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jR1sAAAAASUVORK5CYII=', 'base64'))
    await symlink(directory, path.join(folder, 'outside'))
    const launched = path.join(directory, 'launched.txt')
    await writeFile(path.join(binaries, 'code'), `#!/bin/sh\nprintf '%s\\n' "$#" "$1" "\${TERNILO_LAUNCH_SECRET:-scrubbed}" >> '${launched}'\n`)
    await chmod(path.join(binaries, 'code'), 0o700)
    const localOrigin = `http://127.0.0.1:${await freePort()}`
    const node = startProcess(process.env.TERNILO_E2E_NODE_BINARY ?? path.join(repository, 'target/debug/ternilo'), [
      'serve', '--listen', new URL(localOrigin).host, '--data-dir', path.join(directory, 'node'), '--node-id', 'files-node',
      '--gateway-url', `${application.origin.replace('http:', 'ws:')}/api/v1/executors/connect`, '--allow-insecure-gateway',
    ], { TERNILO_LOCAL_TOKEN: credential.token, XDG_STATE_HOME: path.join(directory, 'state'), XDG_DATA_HOME: desktopData, PATH: binaries, DISPLAY: ':fixture', TERNILO_LAUNCH_SECRET: 'must-not-reach-application' })
    processes.push(node)
    await waitForHttp(localOrigin, node)
    const html = await (await fetch(localOrigin)).text()
    const localToken = JSON.parse(html.match(/window\.__TERNILO_BOOT__\s*=\s*(\{[^<]+\});/)[1]).apiToken
    const local = (resource, options = {}) => serverRequest(localOrigin, resource, { token: localToken, ...options })
    const workspace = await local('/workspaces', { body: { path: folder } })
    const session = await local('/sessions', { body: { workspace_id: workspace.workspace_id } })
    const localResource = `/sessions/${session.identity.session_id}`
    const state = await until(() => owner('/state', { tenantId: team.tenant_id }), value => value.sessions.length === 1, 'Node discovery')
    const remoteSession = state.sessions[0].identity.session_id
    const remoteWorkspace = state.sessions[0].workspace_id
    const remoteResource = `/sessions/${remoteSession}/workspace`
    const shared = (resource, options = {}) => owner(resource, { tenantId: team.tenant_id, ...options })
    const peer = (resource, options = {}) => serverRequest(application.origin, resource, { token: member.access_token ?? member.session?.access_token, tenantId: team.tenant_id, ...options })
    const viewOnly = { view: true, submit: false, stop: false, configure: false }
    await shared(`/sessions/${remoteSession}/sharing/user/${member.user.user_id}`, { method: 'PUT', body: viewOnly })
    assert.equal((await peer(remoteResource)).can_browse, false)
    await assert.rejects(peer(remoteResource, { body: { kind: 'list', path: '' } }), /403/)
    await shared(`/workspaces/${remoteWorkspace}/sharing/user/${member.user.user_id}`, { method: 'PUT', body: viewOnly })
    assert.equal((await peer(remoteResource)).can_browse, true)
    assert.deepEqual((await peer(remoteResource)).applications, [])
    assert.equal((await peer(remoteResource, { body: { kind: 'read', path: 'src/main.rs' } })).encoding, 'utf8')
    await assert.rejects(peer(remoteResource, { body: { kind: 'open', app_id: 'vscode' } }), /403/)
    for (const request of [local, shared]) {
      const resource = request === local ? `${localResource}/workspace` : remoteResource
      assert.deepEqual((await request(resource)).applications.map(app => app.id), request === local ? ['vscode'] : [])
      for (const file of ['../server/server.json', 'outside/server/server.json']) await assert.rejects(request(resource, { body: { kind: 'read', path: file } }))
      await assert.rejects(request(resource, { body: { kind: 'open', app_id: 'arbitrary-command' } }))
      if (request === shared) await assert.rejects(request(resource, { body: { kind: 'open', app_id: 'vscode' } }), /403/)
    }
    browser = await chromium.launch({ headless: true, executablePath: process.env.TERNILO_BROWSER_EXECUTABLE,
      args: ['--host-resolver-rules=MAP workspace-http.ternilo.test 127.0.0.1', '--no-proxy-server'] })
    page = await browser.newPage({ viewport: { width: 1500, height: 950 } })
    page.on('pageerror', error => errors.push(error.message))
    page.on('console', message => { if (message.type() === 'error' && !message.text().includes('Content Security Policy') && !message.text().includes('violates the following')) errors.push(message.text()) })
    page.on('response', response => { if (response.status() >= 400) errors.push(`${response.status()} ${new URL(response.url()).pathname}`) })
    const trackers = []
    page.on('request', request => { if (request.url().includes('workspace.invalid')) trackers.push(request.url()) })
    for (const origin of [localOrigin, application.origin]) for (const asset of ['app.js', 'app.css']) {
      assert.equal(createHash('sha256').update(Buffer.from(await (await fetch(`${origin}/assets/${asset}`)).arrayBuffer())).digest('hex'), createHash('sha256').update(await readFile(path.join(repository, 'web/dist/assets', asset))).digest('hex'))
    }
    await page.addInitScript(() => { if (window === window.top) localStorage.setItem('ternilo.theme', 'dark') })
    await page.goto(localOrigin)
    await page.locator(`[data-sidebar-session-row][data-session-id="${session.identity.session_id}"] [data-sidebar-session-button]`).click()
    const toggle = page.locator('[data-workspace-toggle]')
    await until(() => toggle.isEnabled(), Boolean, 'workspace permission')
    await page.getByRole('button', { name: '选择打开应用', exact: true }).click()
    const appsMenu = page.locator('[data-workspace-app-menu]')
    assert.equal(await appsMenu.getByRole('menuitem').count(), 1)
    assert.equal(await appsMenu.getByRole('menuitem').innerText(), 'VS Code')
    assert.equal(await appsMenu.evaluate(element => getComputedStyle(element).backgroundColor), 'rgb(53, 54, 56)')
    assert.equal(await appsMenu.locator('[data-current]').evaluate(element => getComputedStyle(element).backgroundColor), 'rgba(255, 255, 255, 0.08)')
    assert.equal(await appsMenu.locator('.lucide-check').count(), 0)
    await until(() => appsMenu.locator('img').evaluate(image => image.complete && image.naturalWidth > 0), Boolean, 'native application icon')
    assert.equal(await appsMenu.locator('img').evaluate(image => getComputedStyle(image).width), '18px')
    if (artifacts) { await mkdir(artifacts, { recursive: true }); await page.screenshot({ path: path.join(artifacts, 'workspace-app-menu.png') }) }
    await page.keyboard.press('Escape')
    await page.getByRole('button', { name: '在 VS Code 中打开工作区', exact: true }).click()
    const launchedText = await until(() => readFile(launched, 'utf8').catch(() => ''), Boolean, 'native launch')
    assert.equal(launchedText, `1\n${folder}\nscrubbed\n`)
    await toggle.click()
    const panel = page.locator('[data-workspace-panel]')
    await panel.locator('[data-workspace-entry="src"]').waitFor()
    await until(() => panel.boundingBox(), bounds => bounds.width >= 600 && bounds.width <= 620, 'half-width files sidebar')
    assert.equal(await panel.locator('[data-workspace-entry="outside"]').isDisabled(), true)
    assert.equal(await panel.locator('[data-workspace-entry]').first().getAttribute('data-kind'), 'directory')
    await panel.locator('[data-workspace-entry="src"]').click()
    await panel.locator('[data-workspace-entry="src/main.rs"]').click()
    await panel.locator('[data-workspace-preview="src/main.rs"] pre').filter({ hasText: 'workspace preview' }).waitFor()
    await panel.getByRole('tab', { name: '文件', exact: true }).click()
    await panel.locator('[data-workspace-entry="src/main.rs"]').click()
    assert.equal(await panel.getByRole('tab', { name: 'main.rs', exact: true }).count(), 1)
    await panel.getByRole('button', { name: '切换双栏', exact: true }).click()
    assert.equal(await panel.locator('[data-workspace-pane]').count(), 2)
    await panel.locator('[data-workspace-pane="0"]').getByRole('tab', { name: '文件', exact: true }).click()
    await panel.locator('[data-workspace-entry="README.md"]').click()
    await panel.frameLocator('iframe[title="预览 README.md"]').getByRole('heading', { name: 'Workspace preview' }).waitFor()
    await panel.locator('[data-workspace-entry="unsafe.html"]').click()
    const unsafe = panel.locator('[data-workspace-preview="unsafe.html"]')
    await unsafe.getByRole('button', { name: '预览', exact: true }).click()
    await unsafe.frameLocator('iframe').getByRole('heading', { name: 'Isolated HTML' }).waitFor()
    assert.equal(await unsafe.locator('iframe').getAttribute('sandbox'), '')
    assert.equal(await page.evaluate(() => window.__workspaceInjected), undefined)
    assert.deepEqual(trackers, [])
    await panel.locator('[data-workspace-entry="image.png"]').click()
    await until(() => panel.locator('[data-workspace-preview="image.png"] img').evaluate(image => image.complete && image.naturalWidth > 0), Boolean, 'image decode')
    await panel.locator('[data-workspace-entry="large.txt"]').click()
    const large = panel.locator('[data-workspace-preview="large.txt"]')
    await large.getByText(/文件超过 2 MiB/).waitFor()
    assert.equal(await large.getByRole('link', { name: '下载', exact: true }).count(), 0)
    await writeFile(path.join(folder, 'new.txt'), 'Live refresh')
    await panel.locator('[data-workspace-pane="0"]').getByRole('button', { name: '刷新', exact: true }).click()
    await panel.locator('[data-workspace-entry="new.txt"]').waitFor()
    await panel.getByRole('button', { name: '全屏文件面板', exact: true }).click()
    assert.equal(await panel.getAttribute('role'), 'dialog')
    assert.equal(await page.locator('[data-app-center-column]').getAttribute('inert'), '')
    assert.ok((await panel.boundingBox()).width >= 1498)
    await panel.getByRole('button', { name: '恢复侧栏', exact: true }).click()
    if (artifacts) { await mkdir(artifacts, { recursive: true }); await page.screenshot({ path: path.join(artifacts, 'workspace-desktop.png'), animations: 'disabled' }) }
    await page.reload()
    await panel.locator('[data-workspace-entry="src/main.rs"]').waitFor()
    assert.equal(await panel.locator('[data-workspace-pane]').count(), 2)
    await page.setViewportSize({ width: 390, height: 844 })
    await until(() => panel.getAttribute('role'), role => role === 'dialog', 'mobile overlay')
    assert.ok((await panel.boundingBox()).width <= 390)
    assert.ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth))
    if (artifacts) await page.screenshot({ path: path.join(artifacts, 'workspace-mobile.png'), animations: 'disabled' })
    await panel.getByRole('button', { name: '关闭文件侧栏', exact: true }).click()
    assert.equal(await panel.count(), 0)
    await until(() => toggle.evaluate(element => element === document.activeElement), Boolean, 'focus restored')
    await page.setViewportSize({ width: 1500, height: 950 })
    const second = await local('/sessions', { body: { workspace_id: workspace.workspace_id } })
    await page.locator(`[data-sidebar-session-row][data-session-id="${second.identity.session_id}"] [data-sidebar-session-button]`).click()
    await toggle.click()
    assert.equal(await panel.getByRole('tab').count(), 1)
    await panel.getByRole('button', { name: '关闭文件侧栏', exact: true }).click()
    await page.goto(application.origin)
    await page.getByLabel('用户名', { exact: true }).fill(application.owner.username)
    await page.getByLabel('密码', { exact: true }).fill(application.owner.password)
    await page.getByRole('button', { name: '登录', exact: true }).click()
    await selectSpace(page, team.tenant_id)
    await page.locator(`[data-sidebar-session-row][data-session-id="${remoteSession}"] [data-sidebar-session-button]`).click()
    await until(() => toggle.isEnabled(), Boolean, 'owner workspace browser')
    assert.equal(await page.locator('[data-workspace-open-app]').count(), 0)
    await toggle.click()
    await panel.locator('[data-workspace-entry="src"]').click()
    await panel.locator('[data-workspace-entry="src/main.rs"]').click()
    await panel.locator('[data-workspace-preview="src/main.rs"] pre').filter({ hasText: 'workspace preview' }).waitFor()
    const downloadPromise = page.waitForEvent('download')
    await panel.locator('[data-workspace-preview="src/main.rs"]').getByRole('link', { name: '下载', exact: true }).click()
    const download = await downloadPromise
    assert.equal(download.suggestedFilename(), 'main.rs')
    assert.equal(await readFile(await download.path(), 'utf8'), await readFile(path.join(folder, 'src/main.rs'), 'utf8'))
    await panel.getByRole('tab', { name: '文件', exact: true }).click()
    await panel.locator('[data-workspace-entry="image.png"]').click()
    await until(() => panel.locator('[data-workspace-preview="image.png"] img').evaluate(image => image.complete && image.naturalWidth > 0), Boolean, 'Server image decode under CSP')
    if (artifacts) await page.screenshot({ path: path.join(artifacts, 'workspace-server-owner.png'), animations: 'disabled' })
    await page.setViewportSize({ width: 390, height: 844 })
    await until(() => panel.getAttribute('role'), role => role === 'dialog', 'owner mobile workspace browser')
    await panel.getByRole('button', { name: '关闭文件侧栏', exact: true }).click()
    assert.equal(await page.locator('[data-workspace-open-app]').count(), 0)
    assert.equal(await readFile(launched, 'utf8'), launchedText)
    await page.setViewportSize({ width: 1500, height: 950 })
    await page.goto(application.origin.replace('127.0.0.1', 'workspace-http.ternilo.test'))
    assert.equal(await page.evaluate(() => isSecureContext), false)
    assert.equal(await page.evaluate(() => typeof crypto.randomUUID), 'undefined')
    await page.getByLabel('用户名', { exact: true }).fill(memberAccount.username)
    await page.getByLabel('密码', { exact: true }).fill(memberAccount.password)
    await page.getByRole('button', { name: '登录', exact: true }).click()
    await selectSpace(page, team.tenant_id)
    await page.locator(`[data-sidebar-session-row][data-session-id="${remoteSession}"] [data-sidebar-session-button]`).click()
    await until(() => toggle.isEnabled(), Boolean, 'shared workspace browser')
    assert.equal(await page.locator('[data-workspace-open-app]').count(), 0)
    await toggle.click()
    await panel.locator('[data-workspace-entry="README.md"]').click()
    await panel.frameLocator('iframe').getByRole('heading', { name: 'Workspace preview' }).waitFor()
    await panel.getByRole('button', { name: '新建文件标签页', exact: true }).click()
    assert.equal(await panel.getByRole('tab').count(), 3)
    await panel.getByRole('tab', { name: 'README.md', exact: true }).click()
    await shared(`/workspaces/${remoteWorkspace}/sharing/user/${member.user.user_id}`, { method: 'DELETE' })
    await until(() => toggle.isDisabled(), Boolean, 'workspace share revoked')
    await panel.getByText('需要工作区查看权限；仅共享会话不会开放目录。', { exact: true }).waitFor()
    await assert.rejects(peer(remoteResource, { body: { kind: 'read', path: 'README.md' } }), /403/)
    assert.deepEqual(errors, [])
  } catch (error) {
    if (page && artifacts) { await mkdir(artifacts, { recursive: true }); await page.screenshot({ path: path.join(artifacts, 'workspace-failure.png') }).catch(() => {}) }
    throw new Error(`${error.stack}\n${processes.map(process => process.diagnostics()).join('\n')}`)
  } finally {
    await browser?.close()
    for (const process of processes.reverse()) await stopProcess(process)
    await rm(directory, { recursive: true, force: true })
  }
})
