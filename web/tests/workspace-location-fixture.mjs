import assert from 'node:assert/strict'
import { readFile, writeFile } from 'node:fs/promises'
import path from 'node:path'
import { DatabaseSync } from 'node:sqlite'

async function closedLocationHasNoPrivatePath(page, privatePath) {
  await page.locator('[data-workspace-location]').waitFor({ state: 'hidden' })
  const contents = await page.evaluate(() => ({
    html: document.documentElement.outerHTML,
    storage: [...Object.values(localStorage), ...Object.values(sessionStorage)],
  }))
  assert.equal(JSON.stringify(contents).includes(privatePath), false, 'closed location must not retain a private path in DOM or browser storage')
}

function verifyDatabasePrivacy(databaseUrl, privatePath) {
  assert.ok(databaseUrl.startsWith('sqlite:'), 'this fixture uses its isolated SQLite Server')
  const database = new DatabaseSync(decodeURIComponent(new URL(databaseUrl).pathname), { readOnly: true })
  try {
    const tables = database.prepare("SELECT name FROM sqlite_master WHERE type = 'table'").all()
    for (const { name } of tables) {
      const rows = database.prepare(`SELECT * FROM "${name.replaceAll('"', '""')}"`).all()
      const encoded = JSON.stringify(rows, (_, value) => value instanceof Uint8Array
        ? Buffer.from(value).toString('utf8') : typeof value === 'bigint' ? value.toString() : value)
      assert.equal(encoded.includes(privatePath), false, `${name} must not persist the transient Node directory`)
    }
    return tables.length
  } finally {
    database.close()
  }
}

export async function verifyWorkspaceLocation({ owner, member, ownerRequest, tenantId, memberIdentity,
  workspace, localWorkspace, workspacePath, nodeOrigin, configPath, artifacts }) {
  const endpoint = `/api/v1/workspaces/${encodeURIComponent(workspace.workspace_id)}/location`
  const requests = { owner: 0, member: 0 }
  const trackOwner = request => { if (new URL(request.url()).pathname === endpoint) requests.owner += 1 }
  const trackMember = request => { if (new URL(request.url()).pathname === endpoint) requests.member += 1 }
  owner.on('request', trackOwner)
  member.on('request', trackMember)
  const group = page => page.locator('[data-sidebar-workspace-group]').filter({
    has: page.locator('[data-sidebar-workspace-title]', { hasText: workspace.title }),
  })
  const sharing = `/workspaces/${encodeURIComponent(workspace.workspace_id)}/sharing/user/${memberIdentity.user.user_id}`
  let shared = false
  let local
  const localErrors = []
  try {
    await owner.setViewportSize({ width: 1440, height: 960 })
    const row = group(owner).locator('[data-sidebar-workspace-row]')
    await row.waitFor()
    assert.equal(await row.locator('[data-sidebar-workspace-title]').textContent(), workspace.title)
    assert.equal(await row.locator('[data-sidebar-workspace-machine]').count(), 0, 'one execution location keeps the Local single-line row')
    assert.equal(requests.owner, 0, 'rendering the sidebar must not fetch private locations')
    const response = owner.waitForResponse(response => new URL(response.url()).pathname === endpoint)
    await row.hover()
    const location = owner.locator('[data-workspace-location]')
    const received = await response
    assert.equal(received.ok(), true)
    assert.match(received.headers()['cache-control'], /no-store/)
    const details = await received.json()
    assert.equal(details.status, 'available')
    assert.equal(details.path, workspacePath)
    assert.equal(details.created_at_ms, localWorkspace.created_at_ms)
    assert.ok(details.home && workspacePath.startsWith(details.home + path.sep))
    const shortPath = '~' + workspacePath.slice(details.home.length)
    await location.getByText(shortPath, { exact: true }).waitFor()
    await location.getByText(`电脑 ${workspace.node_id} · 在线`, { exact: true }).waitFor()
    assert.match(await location.textContent(), /创建于/)
    await location.getByRole('button', { name: `复制工作区完整路径：${workspacePath}`, exact: true }).click()
    assert.equal(await owner.evaluate(() => navigator.clipboard.readText()), workspacePath)
    await owner.screenshot({ path: path.join(artifacts, 'workspace-location-owner-desktop.png'), animations: 'disabled' })
    await owner.mouse.move(1000, 80)
    await closedLocationHasNoPrivatePath(owner, workspacePath)

    await owner.setViewportSize({ width: 390, height: 844 })
    await owner.getByRole('button', { name: '打开侧边栏', exact: true }).click()
    await group(owner).getByRole('button', { name: `工作区“${workspace.title}”的操作`, exact: true }).click()
    await owner.getByRole('menuitem', { name: '查看位置', exact: true }).click()
    const dialog = owner.getByRole('dialog', { name: `工作区位置：${workspace.title}`, exact: true })
    const copy = dialog.getByRole('button', { name: `复制工作区完整路径：${workspacePath}`, exact: true })
    await copy.waitFor()
    await dialog.evaluate(element => Promise.all(element.getAnimations().map(animation => animation.finished)))
    assert.ok((await copy.boundingBox()).height >= 40)
    const close = dialog.getByRole('button', { name: '关闭', exact: true })
    const closeBounds = await close.boundingBox()
    assert.ok(closeBounds.width >= 40 && closeBounds.height >= 40)
    const bounds = await dialog.boundingBox()
    assert.ok(bounds.x >= 0 && bounds.x + bounds.width <= 390)
    await copy.click()
    assert.equal(await owner.evaluate(() => navigator.clipboard.readText()), workspacePath)
    await owner.screenshot({ path: path.join(artifacts, 'workspace-location-owner-mobile.png'), animations: 'disabled' })
    await close.click()
    await closedLocationHasNoPrivatePath(owner, workspacePath)
    await owner.locator('[data-mobile-sidebar-close]').click()
    await owner.setViewportSize({ width: 1440, height: 960 })

    await ownerRequest(sharing, { tenantId, method: 'PUT', body: { view: true, submit: false, stop: false, configure: false } })
    shared = true
    await member.setViewportSize({ width: 1440, height: 960 })
    await member.reload()
    await group(member).locator('[data-sidebar-workspace-row]').hover()
    await member.locator('[data-workspace-location]').getByText('目录路径仅所有者可见', { exact: true }).waitFor()
    assert.equal(requests.member, 0, 'shared viewers must not request the owner-only location endpoint')
    assert.equal(await member.locator('[data-workspace-location] button').count(), 0, 'a shared viewer cannot copy a fabricated path')
    await member.screenshot({ path: path.join(artifacts, 'workspace-location-viewer-desktop.png'), animations: 'disabled' })
    await member.mouse.move(1000, 80)
    await closedLocationHasNoPrivatePath(member, workspacePath)

    local = await owner.context().newPage()
    local.on('pageerror', error => localErrors.push(error.message))
    local.on('console', message => { if (message.type() === 'error') localErrors.push(message.text()) })
    local.on('response', response => { if (response.status() >= 400) localErrors.push(`HTTP ${response.status()} ${new URL(response.url()).pathname}`) })
    let localLocationRequests = 0
    local.on('request', request => { if (new URL(request.url()).pathname.endsWith('/location')) localLocationRequests += 1 })
    await local.setViewportSize({ width: 1440, height: 960 })
    await local.goto(nodeOrigin)
    await group(local).locator('[data-sidebar-workspace-row]').hover()
    const localLocation = local.locator('[data-workspace-location]')
    await localLocation.getByText(shortPath, { exact: true }).waitFor()
    await localLocation.getByRole('button', { name: `复制工作区完整路径：${workspacePath}`, exact: true }).click()
    assert.equal(await local.evaluate(() => navigator.clipboard.readText()), workspacePath)
    assert.match(await localLocation.textContent(), /创建于/)
    assert.equal(await group(local).locator('[data-sidebar-workspace-machine]').count(), 0)
    await local.screenshot({ path: path.join(artifacts, 'workspace-location-local-desktop.png'), animations: 'disabled' })
    await local.mouse.move(1000, 80)
    await localLocation.waitFor({ state: 'hidden' })
    await local.setViewportSize({ width: 390, height: 844 })
    await local.getByRole('button', { name: '打开侧边栏', exact: true }).click()
    await group(local).getByRole('button', { name: `工作区“${workspace.title}”的操作`, exact: true }).click()
    await local.getByRole('menuitem', { name: '查看位置', exact: true }).click()
    const localDialog = local.getByRole('dialog', { name: `工作区位置：${workspace.title}`, exact: true })
    const localCopy = localDialog.getByRole('button', { name: `复制工作区完整路径：${workspacePath}`, exact: true })
    await localCopy.click()
    assert.equal(await local.evaluate(() => navigator.clipboard.readText()), workspacePath)
    await local.screenshot({ path: path.join(artifacts, 'workspace-location-local-mobile.png'), animations: 'disabled' })
    await localDialog.getByRole('button', { name: '关闭', exact: true }).click()
    await localDialog.waitFor({ state: 'hidden' })
    assert.equal(localLocationRequests, 0, 'the Local UI uses the existing local path without a remote location request')
    assert.deepEqual(localErrors, [])

    const snapshot = await ownerRequest('/state', { tenantId })
    assert.equal(JSON.stringify(snapshot).includes(workspacePath), false, 'the global workspace snapshot remains redacted after on-demand reads')
    const config = JSON.parse(await readFile(configPath, 'utf8'))
    const tables = verifyDatabasePrivacy(config.database_url, workspacePath)
    await writeFile(path.join(artifacts, 'workspace-location-observations.json'), JSON.stringify({
      requests, remoteCreatedAt: details.created_at_ms, bindingCreatedAt: workspace.created_at_ms,
      copiedActualPath: true, homeAbbreviated: true, closedDomAndStoragePrivate: true, serverTablesChecked: tables,
      localLocationRequests, localErrors,
    }, null, 2))
  } finally {
    owner.off('request', trackOwner)
    member.off('request', trackMember)
    await local?.close()
    if (shared) await ownerRequest(sharing, { tenantId, method: 'DELETE' })
  }
}
