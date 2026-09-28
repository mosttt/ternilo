import assert from 'node:assert/strict'
import { mkdir, mkdtemp, rm } from 'node:fs/promises'
import { createServer } from 'node:http'
import { tmpdir } from 'node:os'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import { spawn } from 'node:child_process'
import test from 'node:test'
import { chromium } from 'playwright'

const webRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..')
const repository = path.resolve(webRoot, '..')
const binary = path.join(repository, 'target', 'debug', 'ternilo')

function startTernilo(dataDirectory) {
  const child = spawn(binary, ['serve', '--listen', '127.0.0.1:0', '--data-dir', dataDirectory], {
    cwd: repository,
    stdio: ['ignore', 'pipe', 'pipe'],
  })
  let diagnostics = ''
  child.stderr.setEncoding('utf8')
  child.stderr.on('data', chunk => { diagnostics += chunk })
  const origin = new Promise((resolve, reject) => {
    let output = ''
    child.stdout.setEncoding('utf8')
    child.stdout.on('data', chunk => {
      output += chunk
      const match = output.match(/Ternilo local web: (http:\/\/[^\s]+)/)
      if (match) resolve(match[1])
    })
    child.once('exit', code => reject(new Error(`Ternilo web exited with ${code}: ${diagnostics}`)))
    child.once('error', reject)
  })
  return { child, origin }
}

async function stopProcess(child) {
  if (child.exitCode !== null) return
  child.kill('SIGINT')
  await Promise.race([
    new Promise(resolve => child.once('exit', resolve)),
    new Promise(resolve => setTimeout(resolve, 5_000)).then(() => child.kill('SIGKILL')),
  ])
}

function event(payload) {
  return `data: ${JSON.stringify(payload)}\n\n`
}

async function startModelFixture() {
  const server = createServer((request, response) => {
    if (request.method !== 'POST' || request.url !== '/v1/responses') {
      response.writeHead(404).end()
      return
    }
    request.resume()
    request.on('end', () => {
      response.writeHead(200, { 'content-type': 'text/event-stream', 'cache-control': 'no-cache' })
      response.write(event({
        type: 'response.output_text.delta',
        output_index: 0,
        content_index: 0,
        delta: 'Sidebar lifecycle ready.',
      }))
      response.end(event({
        type: 'response.completed',
        response: {
          status: 'completed',
          output: [],
          usage: {
            input_tokens: 12,
            output_tokens: 4,
            input_tokens_details: { cached_tokens: 0 },
            output_tokens_details: { reasoning_tokens: 0 },
          },
        },
      }))
    })
  })
  await new Promise((resolve, reject) => {
    server.once('error', reject)
    server.listen(0, '127.0.0.1', resolve)
  })
  return {
    baseUrl: `http://127.0.0.1:${server.address().port}/v1`,
    close: () => new Promise((resolve, reject) => server.close(error => error ? reject(error) : resolve())),
  }
}

async function registerWorkspace(page, workspace, opener) {
  await opener.click()
  const dialog = page.getByRole('dialog', { name: '选择工作文件夹' })
  await dialog.getByRole('button', { name: '编辑文件夹路径' }).click()
  const pathEditor = dialog.getByRole('textbox', { name: '编辑文件夹路径' })
  await pathEditor.fill(workspace)
  await pathEditor.press('Enter')
  await dialog.getByRole('list', { name: `目录 ${workspace}`, exact: true }).waitFor()
  await dialog.getByRole('button', { name: '打开所选文件夹' }).click()
  await dialog.waitFor({ state: 'detached' })
}

async function configureProvider(page, baseUrl) {
  await page.getByRole('button', { name: '设置' }).click()
  const dialog = page.getByRole('dialog', { name: '设置' })
  await dialog.getByRole('button', { name: '模型', exact: true }).click()
  await dialog.getByRole('button', { name: '添加 Provider' }).first().click()
  const editor = dialog.locator('[data-provider-editor="new"]')
  await editor.getByLabel('Provider ID').fill('sidebar-fixture')
  await editor.getByLabel('显示名称', { exact: true }).fill('Sidebar Fixture')
  await editor.getByLabel('API 地址').fill(baseUrl)
  await editor.getByLabel('模型 ID 1').fill('sidebar-model')
  await editor.getByLabel('显示名称（可选） 1').fill('Sidebar Model')
  await editor.getByRole('button', { name: '模型详细设置 1' }).click()
  await editor.getByLabel('最大输出 token').fill('2048')
  await editor.getByRole('button', { name: '添加 Provider', exact: true }).click()
  await dialog.getByText('Sidebar Fixture', { exact: true }).waitFor()
  await dialog.getByRole('button', { name: '关闭设置' }).click()
  await dialog.waitFor({ state: 'detached' })
}

async function reverseByDrag(page, rows, titleHook) {
  assert.equal(await rows.count() >= 2, true)
  assert.equal(await rows.nth(0).getAttribute('draggable'), 'true')
  const before = await rows.locator(titleHook).allTextContents()
  const target = await rows.nth(1).boundingBox()
  assert.ok(target)
  const persisted = page.waitForResponse(response => (
    response.url().endsWith('/api/v1/sidebar-ordering')
    && response.request().method() === 'PUT'
  ))
  await rows.nth(0).dragTo(rows.nth(1), {
    targetPosition: { x: 8, y: Math.max(1, target.height - 2) },
  })
  assert.equal((await persisted).ok(), true)
  const expected = [before[1], before[0], ...before.slice(2)]
  let actual = await rows.locator(titleHook).allTextContents()
  for (let attempt = 0; attempt < 20 && JSON.stringify(actual) !== JSON.stringify(expected); attempt += 1) {
    await page.waitForTimeout(50)
    actual = await rows.locator(titleHook).allTextContents()
  }
  assert.deepEqual(actual, expected)
  return expected
}

async function settleOpenMenu(page) {
  await page.locator('[role="menu"]:visible').evaluate(async element => {
    await Promise.all(element.getAnimations({ subtree: true }).map(animation => animation.finished))
  })
}

async function chooseViewOption(page, label) {
  await page.getByRole('button', { name: '视图选项' }).click()
  const menu = page.locator('[role="menu"]:visible')
  await menu.waitFor()
  await settleOpenMenu(page)
  await menu.getByRole('menuitem', { name: label }).click()
  await menu.waitFor({ state: 'hidden' })
}

async function ensureMobileSidebarOpen(page) {
  const sidebar = page.getByRole('complementary', { name: '会话侧边栏' })
  await page.waitForTimeout(200)
  const initial = await sidebar.boundingBox()
  if (!initial || initial.x < -1) await page.getByRole('button', { name: '打开侧边栏' }).click()
  await page.waitForFunction(() => {
    const x = document.querySelector('.app-sidebar')?.getBoundingClientRect().x
    return x !== undefined && Math.abs(x) < 0.5
  })
  return sidebar
}

test('Sidebar drives real workspace/session lifecycle on desktop and mobile', { timeout: 120_000 }, async () => {
  const dataDirectory = await mkdtemp(path.join(tmpdir(), 'ternilo-sidebar-e2e-'))
  const firstPath = path.join(dataDirectory, 'alpha-workspace')
  const secondPath = path.join(dataDirectory, 'beta-workspace')
  await Promise.all([mkdir(firstPath), mkdir(secondPath)])
  const model = await startModelFixture()
  const ternilo = startTernilo(dataDirectory)
  let browser
  try {
    const origin = await ternilo.origin
    browser = await chromium.launch({ headless: true })
    const context = await browser.newContext({ viewport: { width: 1440, height: 900 } })
    await context.grantPermissions(['clipboard-read', 'clipboard-write'], { origin })
    const page = await context.newPage()
    const pageErrors = []
    page.on('pageerror', error => {
      pageErrors.push(error.message)
      process.stderr.write(`browser page error: ${error.message}\n`)
    })
    await page.goto(origin, { waitUntil: 'networkidle' })

    await registerWorkspace(page, firstPath, page.getByRole('button', { name: '选择工作文件夹' }).first())
    await page.locator('[data-sidebar-new-session]').click()
    const blank = page.locator('[data-sidebar-session-row]').first()
    await blank.getByText('新会话', { exact: true }).waitFor()
    assert.equal(await blank.locator('[data-sidebar-session-time]').count(), 0)
    assert.equal(await blank.getByRole('button', { name: /的操作$/ }).count(), 0)

    await configureProvider(page, model.baseUrl)
    const input = page.getByRole('textbox', { name: '输入任务' })
    await input.fill('建立侧边栏生命周期会话')
    await page.getByRole('button', { name: '发送' }).click()
    await page.getByText('Sidebar lifecycle ready.', { exact: true }).waitFor()

    const established = page.locator('[data-sidebar-session-row]').filter({ has: page.locator('[data-sidebar-session-time]') }).first()
    await established.hover()
    await established.getByRole('button', { name: /的操作$/ }).click()
    assert.deepEqual((await page.getByRole('menuitem').allTextContents()).map(value => value.trim()), ['重命名', '分叉会话', '归档会话'])
    await page.getByRole('menuitem', { name: '重命名' }).click()
    const rename = page.getByRole('dialog', { name: '重命名会话' })
    let renamePatchCount = 0
    const rejectRename = route => {
      if (route.request().method() !== 'PATCH') return route.continue()
      renamePatchCount += 1
      return route.fulfill({
        status: 503,
        contentType: 'application/json',
        body: JSON.stringify({ error: { code: 'rename_unavailable', message: '重命名服务暂不可用' } }),
      })
    }
    await page.route('**/api/v1/sessions/*', rejectRename)
    await rename.getByRole('textbox', { name: '会话名称' }).fill('不应落盘的名称')
    await rename.getByRole('button', { name: '重命名', exact: true }).evaluate(button => {
      button.click()
      button.click()
    })
    await rename.getByRole('alert').filter({ hasText: '重命名服务暂不可用' }).waitFor()
    assert.equal(renamePatchCount, 1)
    assert.equal(await rename.isVisible(), true)
    await page.unroute('**/api/v1/sessions/*', rejectRename)
    await rename.getByRole('textbox', { name: '会话名称' }).fill('侧边栏主会话')
    await rename.getByRole('textbox', { name: '会话名称' }).press('Enter')
    await page.getByText('侧边栏主会话', { exact: true }).first().waitFor()

    const mainBeforeOrdering = page.locator('[data-sidebar-session-row]').filter({ hasText: '侧边栏主会话' }).first()
    await mainBeforeOrdering.hover()
    await mainBeforeOrdering.getByRole('button', { name: /的操作$/ }).click()
    const mainMenu = page.locator('[role="menu"]:visible')
    await settleOpenMenu(page)
    await mainMenu.getByRole('menuitem', { name: '分叉会话' }).click()
    await mainMenu.waitFor({ state: 'hidden' })
    const secondSession = page.locator('[data-sidebar-session-row]').filter({ hasText: '侧边栏主会话 (1)' })
    await secondSession.waitFor()
    await secondSession.hover()
    await secondSession.getByRole('button', { name: /的操作$/ }).click()
    const secondMenu = page.locator('[role="menu"]:visible')
    await settleOpenMenu(page)
    await secondMenu.getByRole('menuitem', { name: '重命名' }).click()
    await secondMenu.waitFor({ state: 'hidden' })
    const secondRename = page.getByRole('dialog', { name: '重命名会话' })
    await secondRename.getByRole('textbox', { name: '会话名称' }).fill('侧边栏次会话')
    await secondRename.getByRole('textbox', { name: '会话名称' }).press('Enter')
    await secondRename.waitFor({ state: 'detached' })
    await mainBeforeOrdering.locator('[data-sidebar-session-button]').click()
    await registerWorkspace(page, secondPath, page.getByRole('button', { name: '添加工作区' }))
    await page.locator('[data-sidebar-workspace-title]').filter({ hasText: /^beta-workspace$/ }).waitFor()

    await page.getByRole('button', { name: '视图选项' }).click()
    await settleOpenMenu(page)
    const viewMenu = page.locator('[role="menu"]:visible')
    for (const label of ['按工作区', '单列表', '手动排序', '最近更新']) {
      assert.equal(await viewMenu.getByRole('menuitem', { name: label }).isVisible(), true)
    }
    await viewMenu.getByRole('menuitem', { name: '单列表' }).click()
    await viewMenu.waitFor({ state: 'hidden' })
    assert.equal(await page.locator('[data-sidebar-workspace-header]').getByText('会话', { exact: true }).isVisible(), true)
    await chooseViewOption(page, '按工作区')
    await chooseViewOption(page, '手动排序')

    const workspaceRows = page.locator('[data-sidebar-workspace-row]').filter({ has: page.locator('[data-sidebar-workspace-title]') })
    const workspaceOrder = await reverseByDrag(page, workspaceRows, '[data-sidebar-workspace-title]')

    let rejectedOrderingWrites = 0
    const rejectOrdering = route => {
      if (route.request().method() !== 'PUT') return route.continue()
      rejectedOrderingWrites += 1
      return route.fulfill({
        status: 503,
        contentType: 'application/json',
        body: JSON.stringify({ error: { code: 'ordering_unavailable', message: '顺序服务暂不可用' } }),
      })
    }
    await page.route('**/api/v1/sidebar-ordering', rejectOrdering)
    const rollbackWorkspace = workspaceRows.nth(1)
    await rollbackWorkspace.hover()
    await rollbackWorkspace.getByRole('button', { name: /工作区“.*”的操作/ }).click()
    await page.getByRole('menuitem', { name: '工作区上移' }).click()
    await page.getByText(/无法保存侧边栏顺序.*顺序服务暂不可用/).waitFor()
    assert.equal(rejectedOrderingWrites, 1)
    assert.deepEqual(await page.locator('[data-sidebar-workspace-title]').allTextContents(), workspaceOrder)
    await page.unroute('**/api/v1/sidebar-ordering', rejectOrdering)

    await page.locator('[data-sidebar-session-row]').filter({ hasText: '侧边栏主会话' }).locator('[data-sidebar-session-button]').click()
    const alphaGroup = page.locator('[data-sidebar-workspace-group]').filter({ hasText: 'alpha-workspace' })
    const alphaToggle = alphaGroup.locator('[data-sidebar-workspace-button]')
    assert.equal(await alphaToggle.getAttribute('aria-expanded'), 'true')
    await alphaToggle.click()
    assert.equal(await alphaToggle.getAttribute('aria-expanded'), 'false')
    assert.equal(await alphaGroup.locator('[data-sidebar-session-row]').count(), 0)
    await alphaToggle.click()

    const sessionRows = alphaGroup.locator('[data-sidebar-session-row]')
    const sessionOrder = await reverseByDrag(page, sessionRows, '[data-sidebar-session-title]')
    const storedView = await page.evaluate(key => JSON.parse(localStorage.getItem(key) ?? '{}'), 'ternilo.sidebar-view-v1')
    assert.equal(storedView.workspaceOrder, undefined)
    assert.equal(storedView.sessionOrderByAccount, undefined)
    await page.reload({ waitUntil: 'domcontentloaded' })
    await page.locator('[data-sidebar-workspace-title]').first().waitFor()
    assert.deepEqual(await page.locator('[data-sidebar-workspace-title]').allTextContents(), workspaceOrder)
    assert.deepEqual(
      await page.locator('[data-sidebar-workspace-group]').filter({ hasText: 'alpha-workspace' }).locator('[data-sidebar-session-title]').allTextContents(),
      sessionOrder,
    )

    await page.getByRole('button', { name: '搜索会话' }).click()
    await page.getByRole('textbox', { name: '搜索会话' }).fill('侧边栏主会话')
    await page.locator('[data-sidebar-session-title]', { hasText: '侧边栏主会话' }).waitFor()
    await page.getByRole('button', { name: '关闭搜索' }).click()

    const alphaRow = page.locator('[data-sidebar-workspace-row]').filter({ hasText: 'alpha-workspace' }).first()
    assert.equal(await alphaRow.locator('[aria-label="本机 · 在线"]').count(), 1)
    await alphaRow.hover()
    const pathCard = page.getByRole('button', { name: `复制工作区完整路径：${firstPath}` })
    await pathCard.waitFor()
    assert.match(await pathCard.textContent(), /创建于 \d{4}年\d{1,2}月\d{1,2}日 \d{2}:\d{2}/)
    await pathCard.click()
    assert.equal(await page.evaluate(() => navigator.clipboard.readText()), firstPath)

    await alphaRow.hover()
    await alphaRow.getByRole('button', { name: '工作区“alpha-workspace”的操作' }).click()
    assert.deepEqual((await page.getByRole('menuitem').allTextContents()).map(value => value.trim()), [
      '重命名', '移除工作区', '工作区上移', '工作区下移',
    ])
    await page.getByRole('menuitem', { name: '重命名' }).click()
    const workspaceRename = page.getByRole('dialog', { name: '重命名工作区' })
    await workspaceRename.getByRole('textbox', { name: '工作区名称' }).fill('侧边栏工作区')
    await workspaceRename.getByRole('textbox', { name: '工作区名称' }).press('Enter')
    await page.getByText('侧边栏工作区', { exact: true }).first().waitFor()

    const mainSession = page.locator('[data-sidebar-session-row]').filter({ hasText: '侧边栏主会话' }).first()
    await mainSession.hover()
    await mainSession.getByRole('button', { name: /的操作$/ }).click()
    await page.getByRole('menuitem', { name: '分叉会话' }).click()
    const fork = page.locator('[data-sidebar-session-row]').filter({ hasText: '侧边栏主会话 (1)' })
    await fork.waitFor()
    await fork.hover()
    await fork.getByRole('button', { name: /的操作$/ }).click()
    await page.getByRole('menuitem', { name: '归档会话' }).click()
    await fork.waitFor({ state: 'detached' })

    await page.setViewportSize({ width: 390, height: 844 })
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth), 390)
    const sidebar = await ensureMobileSidebarOpen(page)
    assert.equal(await sidebar.evaluate(element => Math.abs(element.getBoundingClientRect().x) < 1), true)
    for (const button of await sidebar.locator('button:visible').all()) {
      const box = await button.boundingBox()
      assert.equal(box.height >= 40, true)
    }

    const mobileWorkspaceRows = sidebar.locator('[data-sidebar-workspace-row]').filter({ has: page.locator('[data-sidebar-workspace-title]') })
    const mobileWorkspaceBefore = await mobileWorkspaceRows.locator('[data-sidebar-workspace-title]').allTextContents()
    assert.equal(mobileWorkspaceBefore.length >= 2, true)
    const mobileWorkspaceAction = mobileWorkspaceRows.nth(0).getByRole('button', { name: /工作区.*的操作/ })
    const mobileActionGeometry = await mobileWorkspaceAction.evaluate(element => ({
      button: element.getBoundingClientRect().toJSON(),
      sidebar: element.closest('.app-sidebar')?.getBoundingClientRect().toJSON(),
      viewport: { width: window.innerWidth, height: window.innerHeight },
    }))
    assert.equal(
      mobileActionGeometry.button.left >= 0 && mobileActionGeometry.button.right <= mobileActionGeometry.viewport.width,
      true,
      JSON.stringify(mobileActionGeometry),
    )
    await mobileWorkspaceAction.click()
    const workspaceUp = page.getByRole('menuitem', { name: '工作区上移' })
    const workspaceDown = page.getByRole('menuitem', { name: '工作区下移' })
    assert.equal(await workspaceUp.getAttribute('data-disabled') !== null, true)
    await settleOpenMenu(page)
    const workspaceDownBox = await workspaceDown.boundingBox()
    assert.ok(workspaceDownBox)
    assert.equal(workspaceDownBox.height >= 40, true)
    await workspaceDown.click()
    await ensureMobileSidebarOpen(page)
    const mobileWorkspaceOrder = [mobileWorkspaceBefore[1], mobileWorkspaceBefore[0], ...mobileWorkspaceBefore.slice(2)]
    assert.deepEqual(await mobileWorkspaceRows.locator('[data-sidebar-workspace-title]').allTextContents(), mobileWorkspaceOrder)

    let mobileAlphaGroup = sidebar.locator('[data-sidebar-workspace-group]').filter({ hasText: '侧边栏工作区' })
    const mobileAlphaToggle = mobileAlphaGroup.locator('[data-sidebar-workspace-button]')
    if (await mobileAlphaToggle.getAttribute('aria-expanded') !== 'true') {
      await mobileAlphaToggle.click()
    }
    const mobileSessionRows = mobileAlphaGroup.locator('[data-sidebar-session-row]')
    const mobileSessionBefore = await mobileSessionRows.locator('[data-sidebar-session-title]').allTextContents()
    assert.equal(mobileSessionBefore.length >= 2, true)
    await mobileSessionRows.nth(0).getByRole('button', { name: /会话.*的操作/ }).click()
    await page.getByText('手动排序（仅当前分组）', { exact: true }).waitFor()
    const sessionDown = page.getByRole('menuitem', { name: '在当前分组内下移' })
    await settleOpenMenu(page)
    const sessionDownBox = await sessionDown.boundingBox()
    assert.ok(sessionDownBox)
    assert.equal(sessionDownBox.height >= 40, true)
    await sessionDown.click()
    await ensureMobileSidebarOpen(page)
    const mobileSessionOrder = [mobileSessionBefore[1], mobileSessionBefore[0], ...mobileSessionBefore.slice(2)]
    assert.deepEqual(await mobileSessionRows.locator('[data-sidebar-session-title]').allTextContents(), mobileSessionOrder)

    await page.reload({ waitUntil: 'domcontentloaded' })
    await ensureMobileSidebarOpen(page)
    assert.deepEqual(
      await page.locator('[data-sidebar-workspace-title]').allTextContents(),
      mobileWorkspaceOrder,
    )
    mobileAlphaGroup = page.locator('[data-sidebar-workspace-group]').filter({ hasText: '侧边栏工作区' })
    assert.deepEqual(
      await mobileAlphaGroup.locator('[data-sidebar-session-title]').allTextContents(),
      mobileSessionOrder,
    )
    await sidebar.getByRole('button', { name: '关闭侧边栏' }).click()
    await page.waitForFunction(() => document.querySelector('.app-sidebar')?.getBoundingClientRect().x < -20)

    await page.setViewportSize({ width: 1440, height: 900 })
    const renamedWorkspace = page.locator('[data-sidebar-workspace-row]').filter({ hasText: '侧边栏工作区' }).first()
    await renamedWorkspace.hover()
    await renamedWorkspace.getByRole('button', { name: '工作区“侧边栏工作区”的操作' }).click()
    await page.getByRole('menuitem', { name: '移除工作区' }).click()
    const remove = page.getByRole('dialog', { name: '移除工作区' })
    assert.match(await remove.textContent(), /目录、其中的文件和会话日志都会保留/)
    await remove.getByRole('button', { name: '移除工作区' }).click()
    await page.getByText('未分组', { exact: true }).waitFor()

    const ungrouped = page.locator('[data-sidebar-workspace-group]').filter({ hasText: '未分组' })
    await ungrouped.locator('[data-sidebar-workspace-row]').first().hover()
    await ungrouped.getByRole('button', { name: '未分组的操作' }).click()
    await page.getByRole('menuitem', { name: /删除全部会话/ }).click()
    const deleteUngrouped = page.getByRole('dialog', { name: '删除未分组会话？' })
    assert.match(await deleteUngrouped.textContent(), /永久删除“未分组”中的 \d+ 个会话及其日志/)
    await deleteUngrouped.getByRole('button', { name: '全部删除' }).click()
    await ungrouped.waitFor({ state: 'detached' })
    assert.deepEqual(pageErrors, [])
  } finally {
    await browser?.close()
    await stopProcess(ternilo.child)
    await model.close()
    await rm(dataDirectory, { recursive: true, force: true })
  }
})
