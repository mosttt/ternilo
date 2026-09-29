import { selectChoice } from './browser-select-fixture.mjs'
import assert from 'node:assert/strict'
import { mkdir, mkdtemp, readFile, readdir, rm, stat, writeFile } from 'node:fs/promises'
import { createServer } from 'node:http'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { chromium } from 'playwright'
import { choose, closeSettings, computerModels, settings as modelSettings } from './account-node-provider-fixture.mjs'

import { selectSpace, selectProject,
  freePort,
  initializeServer,
  repository,
  serverRequest,
  startOidcServer,
  startPostgres,
  startProcess,
  stopProcess,
  waitForHttp,
} from './platform-e2e-fixture.mjs'

const controlBinary = process.env.TERNILO_E2E_SERVER_BINARY ?? path.join(repository, 'target', 'debug', 'ternilo-server')

async function captureArtifact(page, name) {
  const directory = process.env.TERNILO_E2E_ARTIFACT_DIR
  if (!directory) return
  await mkdir(directory, { recursive: true })
  await page.screenshot({ path: path.join(directory, name), fullPage: true })
}
const nodeBinary = process.env.TERNILO_E2E_NODE_BINARY ?? path.join(repository, 'target', 'debug', 'ternilo')

function sse(payload) {
  return `data: ${JSON.stringify(payload)}\n\n`
}

async function startNodeModelFixture() {
  const requests = []
  const server = createServer(async (incoming, response) => {
    if (incoming.method !== 'POST' || incoming.url !== '/v1/responses') {
      response.writeHead(404, { 'content-type': 'application/json' })
      response.end('{"error":"not found"}')
      return
    }
    const chunks = []
    for await (const chunk of incoming) chunks.push(chunk)
    const body = JSON.parse(Buffer.concat(chunks).toString('utf8'))
    requests.push({ body, authorization: incoming.headers.authorization ?? null })
    const titleRequest = typeof body.instructions === 'string'
      && body.instructions.includes('You name software-agent conversations')
    const text = titleRequest ? 'Node model session' : 'control-node-model-ready'
    response.writeHead(200, {
      'content-type': 'text/event-stream',
      'cache-control': 'no-cache',
    })
    response.end([
      sse({ type: 'response.output_text.delta', output_index: 0, content_index: 0, delta: text }),
      sse({
        type: 'response.completed',
        response: {
          status: 'completed',
          output: [],
          usage: {
            input_tokens: 12,
            output_tokens: 4,
            input_tokens_details: { cached_tokens: 2 },
            output_tokens_details: { reasoning_tokens: 1 },
          },
        },
      }),
    ].join(''))
  })
  await new Promise((resolve, reject) => {
    server.once('error', reject)
    server.listen(0, '127.0.0.1', resolve)
  })
  return {
    baseUrl: `http://127.0.0.1:${server.address().port}/v1`,
    async waitForRequest(predicate) {
      const deadline = Date.now() + 30_000
      while (Date.now() < deadline) {
        const request = requests.find(predicate)
        if (request) return request
        await new Promise(resolve => setTimeout(resolve, 100))
      }
      throw new Error(`Node model request was not received: ${JSON.stringify(requests)}`)
    },
    close: () => new Promise((resolve, reject) => server.close(error => error ? reject(error) : resolve())),
  }
}

async function configureNodeProviderThroughUi(page, baseUrl, workspaceName) {
  const tenantId = await page.evaluate(() => localStorage.getItem('ternilo.current-tenant'))
  await modelSettings(page)
  const settings = await computerModels(page, tenantId)
  assert.match(await page.locator('[data-model-source-target]').textContent(), new RegExp(workspaceName))
  await settings.getByRole('button', { name: '添加 Provider' }).first().click()
  const editor = settings.locator('[data-provider-editor="new"]')
  await editor.getByLabel('Provider ID').fill('node-browser')
  await editor.getByLabel('显示名称', { exact: true }).fill('Node Browser Provider')
  await editor.getByLabel('API Key').fill('node-browser-secret')
  await editor.getByLabel('API 地址').fill(baseUrl)
  await editor.locator('[id$="-provider-defaults-context"]').fill('128K')
  await editor.locator('[id$="-provider-defaults-output"]').fill('8K')
  await editor.getByLabel('启用 Provider 默认模型设置 的推理强度').click()
  await selectChoice(editor.locator('[id$="-provider-defaults-default-effort"]'), 'high')
  await editor.getByLabel('high实际推理值', { exact: true }).fill('ultra')
  await editor.getByLabel('模型 ID 1').fill('node-browser-model')
  await editor.getByLabel('显示名称（可选） 1').fill('Node Browser Model')
  await editor.getByRole('button', { name: '添加 Provider', exact: true }).click()
  await settings.getByText('Node Browser Provider', { exact: true }).waitFor()
  await closeSettings(page)
  await settings.waitFor({ state: 'detached' })
  await choose(page, 'node', 'node-browser', 'Node Browser Model')
  await page.getByRole('button', { name: /Node Browser Model · high/ }).waitFor()
}

async function waitForFile(file, expected) {
  const deadline = Date.now() + 30_000
  while (Date.now() < deadline) {
    try {
      if (await readFile(file, 'utf8') === expected) return
    } catch {}
    await new Promise(resolve => setTimeout(resolve, 100))
  }
  throw new Error(`file did not reach expected contents: ${file}`)
}

async function snapshotNodeSessionFiles(dataDirectory) {
  const directory = path.join(dataDirectory, 'sessions')
  const files = (await readdir(directory)).sort()
  return Object.fromEntries(await Promise.all(files.map(async file => [
    file,
    await readFile(path.join(directory, file), 'utf8'),
  ])))
}

async function downloadSessionFromMenu(page, moreLabel, exportLabel, expectedSessionId) {
  const downloadStarted = page.waitForEvent('download')
  const exportResponded = page.waitForResponse(value => (
    value.request().method() === 'GET'
      && /\/api\/v1\/sessions\/[^/]+\/export$/.test(new URL(value.url()).pathname)
  ))
  await page.locator('.session-header-actions').getByRole('button', { name: moreLabel }).click()
  const item = page.getByRole('menuitem', { name: exportLabel, exact: true })
  assert.equal(await item.evaluate(element => element.tagName), 'BUTTON')
  assert.equal(await item.isEnabled(), true)
  await item.click()
  const [download, response] = await Promise.all([downloadStarted, exportResponded])
  await item.waitFor({ state: 'detached' })
  const responseText = await response.text()
  assert.equal(
    new URL(response.url()).pathname,
    `/api/v1/sessions/${encodeURIComponent(expectedSessionId)}/export`,
  )
  assert.equal(response.status(), 200, `session export response: ${responseText}`)
  const downloadedPath = await download.path()
  assert.ok(downloadedPath)
  return {
    filename: download.suggestedFilename(),
    value: JSON.parse(await readFile(downloadedPath, 'utf8')),
    response: JSON.parse(responseText),
  }
}

async function revealWriteTool(
  page,
  expectedFile = 'control-node-proof.txt',
  expectedContent = 'control-node-chain-ready',
) {
  const branch = page.locator('[data-tool-call-id]').filter({ hasText: '写入文件' }).last()
  await branch.waitFor({ state: 'visible', timeout: 30_000 })
  const expand = branch.getByRole('button', { name: '展开 写入文件 结果' })
  if (await expand.isVisible()) await expand.click()
  const diff = branch.locator('[data-tool-view="diff"]')
  await diff.waitFor({ state: 'visible', timeout: 30_000 })
  const content = await diff.textContent() ?? ''
  assert.equal(content.includes(expectedFile), true)
  assert.equal(content.includes(expectedContent), true)
  return branch
}

async function createProjectThroughUi(page) {
  await page.getByRole('button', { name: '选择工作文件夹' }).last().click()
  const dialog = page.getByRole('dialog', { name: '打开工作区' })
  await dialog.waitFor()
  const ownComputer = dialog.getByRole('button', { name: /我的电脑或 VPS/ })
  if (await ownComputer.count()) await ownComputer.click()
  await selectProject(page, dialog, '新建项目')
  await dialog.getByLabel('项目名称', { exact: true }).fill('Home Project')
  const created = page.waitForResponse(response => response.request().method() === 'POST' && new URL(response.url()).pathname === '/api/v1/projects')
  await dialog.getByRole('button', { name: '仅创建项目' }).click()
  const response = await created
  const value = await response.json()
  assert.equal(response.status(), 201, `project creation failed: ${JSON.stringify(value)}`)
  try {
    await dialog.getByRole('combobox', { name: '项目', exact: true }).filter({ hasText: 'Home Project' }).waitFor()
  } catch (error) {
    throw new Error(`created project ${JSON.stringify(value)} missing from dialog: ${await dialog.innerText()}`, { cause: error })
  }
  const projectId = await dialog.getByRole('combobox', { name: '项目', exact: true }).getAttribute('data-choice-value')
  assert.ok(projectId)
  await dialog.getByRole('button', { name: '取消' }).click()
  await dialog.waitFor({ state: 'detached' })
  return projectId
}

async function openSpaceManagement(page, language = 'zh') {
  await page.getByRole('button', { name: language === 'en' ? 'Space management' : '空间管理', exact: true }).click()
  const management = page.locator('[data-space-management]')
  await management.locator('[data-platform-settings]').waitFor()
  return management
}

async function closeSpaceManagement(page, language = 'zh') {
  await page.getByRole('link', { name: language === 'en' ? 'Back to workbench' : '返回工作台', exact: true }).click()
  await page.locator('[data-space-management]').waitFor({ state: 'detached' })
}

async function createTeamThroughUi(page, name, slug) {
  await page.getByRole('button', { name: '创建团队', exact: true }).click()
  const dialog = page.getByRole('dialog', { name: '创建团队', exact: true })
  await dialog.locator('#team-create-name').fill(name)
  await dialog.locator('#team-create-slug').fill(slug)
  const created = page.waitForResponse(response => response.request().method() === 'POST'
    && new URL(response.url()).pathname === '/api/v1/tenants')
  await dialog.getByRole('button', { name: '创建团队', exact: true }).click()
  const response = await created
  assert.equal(response.status(), 201)
  const { tenant } = await response.json()
  assert.equal(tenant.kind, 'team')
  await dialog.waitFor({ state: 'detached' })
  await selectSpace(page, tenant.tenant_id)
  return tenant.tenant_id
}

async function createEnrollmentThroughUi(page, nodeId, projectId) {
  const settings = await openSpaceManagement(page)
  assert.equal(await settings.getByRole('button', { name: '打开配置目录' }).count(), 0)
  await settings.getByRole('tab', { name: '电脑', exact: true }).click()
  await settings.getByLabel('电脑 ID').fill(nodeId)
  await selectChoice(settings.getByLabel('绑定项目'), projectId)
  await settings.getByRole('button', { name: '生成启动命令' }).click()
  const launch = page.getByRole('dialog', { name: '启动 Ternilo Node' })
  await launch.waitFor()
  const command = await launch.locator('[data-node-launch-command]').textContent() ?? ''
  assert.match(command, /ternilo serve/)
  assert.match(command, /--gateway-url 'ws:\/\/127\.0\.0\.1:\d+\/api\/v1\/executors\/connect'/)
  assert.match(command, new RegExp(`--node-id '${nodeId}'`))
  const credential = /--token "([^"\s]+)"/.exec(command)?.[1]
  assert.ok(credential)
  await launch.getByRole('button', { name: '复制命令' }).click()
  await launch.getByRole('button', { name: '已复制' }).waitFor()
  await launch.getByRole('button', { name: '我已保存，关闭' }).click()
  await launch.waitFor({ state: 'detached' })
  assert.equal(await page.locator('[data-node-launch-command]').count(), 0)
  assert.equal((await settings.textContent() ?? '').includes(credential), false)
  await closeSpaceManagement(page)
  return credential
}

async function createOwnedEnrollmentThroughUi(page, nodeId, projectId) {
  assert.equal(await page.getByRole('button', { name: '空间管理', exact: true }).count(), 0)
  assert.equal(await page.getByRole('button', { name: '平台管理', exact: true }).count(), 0)
  await page.getByRole('button', { name: '用户设置', exact: true }).click()
  const settings = page.locator('[data-user-settings]')
  await settings.waitFor()
  await settings.getByRole('button', { name: '我的机器', exact: true }).click()
  assert.equal(await settings.getByRole('button', { name: '平台管理', exact: true }).count(), 0)
  await settings.getByLabel('电脑 ID').fill(nodeId)
  await selectChoice(settings.getByLabel('绑定项目'), projectId)
  await settings.getByRole('button', { name: '生成启动命令' }).click()
  const launch = page.getByRole('dialog', { name: '启动 Ternilo Node' })
  await launch.waitFor()
  const command = await launch.locator('[data-node-launch-command]').textContent() ?? ''
  assert.match(command, /ternilo serve/)
  assert.match(command, new RegExp(`--node-id '${nodeId}'`))
  const credential = /--token "([^"\s]+)"/.exec(command)?.[1]
  assert.ok(credential)
  await launch.getByRole('button', { name: '我已保存，关闭' }).click()
  await launch.waitFor({ state: 'detached' })
  assert.equal((await settings.textContent() ?? '').includes(credential), false)
  await settings.getByRole('button', { name: '返回工作台' }).click()
  await settings.waitFor({ state: 'detached' })
  return credential
}

async function registerMemberThroughOidc(browser, origin, oidc) {
  oidc.selectIdentity('member')
  const context = await browser.newContext({ viewport: { width: 1280, height: 800 } })
  const page = await context.newPage()
  const pageErrors = []
  page.on('pageerror', error => pageErrors.push(error.message))
  try {
    await page.goto(origin, { waitUntil: 'domcontentloaded' })
    await page.getByRole('button', { name: '使用组织账号登录' }).click()
    await page.getByRole('dialog', { name: '完善账号信息', exact: true }).waitFor()
    await page.getByLabel('用户名', { exact: true }).fill('control-node-member')
    await page.getByLabel('邮箱', { exact: true }).fill('control-node-member@example.test')
    await page.getByRole('button', { name: '完成注册并继续', exact: true }).click()
    const spaces = page.getByRole('combobox', { name: '切换空间' })
    await spaces.waitFor({ timeout: 30_000 })
    assert.equal(await page.getByRole('dialog', { name: '创建 Ternilo 空间' }).count(), 0)
    const token = await page.evaluate(() => sessionStorage.getItem('ternilo.oidc.access'))
    const response = await page.request.get(`${origin}/api/v1/auth/session`, {
      headers: { authorization: `Bearer ${token}` },
    })
    assert.equal(response.status(), 200)
    const identity = await response.json()
    assert.equal(identity.platform_role, 'user')
    assert.equal(await spaces.inputValue(), identity.personal_tenant_id)
    const userId = identity.user.user_id
    assert.match(userId, /^usr_/)
    assert.deepEqual(pageErrors, [])
    return userId
  } finally {
    await context.close()
    oidc.selectIdentity('owner')
  }
}

async function manageMemberThroughUi(page, userId) {
  const waitForMutation = method => page.waitForResponse(response => (
    response.request().method() === method
      && new URL(response.url()).pathname.endsWith(`/members/${encodeURIComponent(userId)}`)
  ))
  let settings = await openSpaceManagement(page)
  await settings.getByRole('tab', { name: '成员', exact: true }).click()
  await settings.getByLabel('用户 ID').fill(userId)
  await selectChoice(settings.getByLabel('角色', { exact: true }), 'member')
  const added = waitForMutation('PUT')
  await settings.getByRole('button', { name: '添加或更新成员' }).click()
  assert.equal((await added).status(), 204)
  await settings.getByText('成员角色已保存', { exact: true }).waitFor()

  let row = settings.locator(`[data-platform-member="${userId}"]`)
  await row.getByText('member@example.com', { exact: true }).waitFor()
  const role = row.getByRole('combobox', { name: 'Control Platform Member · 角色' })
  assert.equal(await role.inputValue(), 'member')
  await selectChoice(role, 'admin')
  const updated = waitForMutation('PUT')
  await row.getByRole('button', { name: '保存角色' }).click()
  assert.equal((await updated).status(), 204)
  await settings.getByText('成员角色已保存', { exact: true }).waitFor()
  await closeSpaceManagement(page)

  await page.reload({ waitUntil: 'domcontentloaded' })
  settings = await openSpaceManagement(page)
  row = settings.locator(`[data-platform-member="${userId}"]`)
  await row.waitFor({ timeout: 30_000 })
  assert.equal(await row.getByRole('combobox', { name: 'Control Platform Member · 角色' }).inputValue(), 'admin')

  await row.getByRole('button', { name: '删除成员 · Control Platform Member' }).click()
  const removeDialog = page.getByRole('dialog', { name: '删除该成员？' })
  const removed = waitForMutation('DELETE')
  await removeDialog.getByRole('button', { name: '删除成员', exact: true }).click()
  assert.equal((await removed).status(), 204)
  await removeDialog.waitFor({ state: 'detached' })
  await row.waitFor({ state: 'detached' })
  await settings.getByText('成员已删除', { exact: true }).waitFor()
  await closeSpaceManagement(page)

  await page.reload({ waitUntil: 'domcontentloaded' })
  settings = await openSpaceManagement(page)
  await settings.getByRole('tab', { name: '成员', exact: true }).click()
  await settings.locator('[data-platform-members][data-platform-list-state="ready"]').waitFor()
  assert.equal(await settings.locator(`[data-platform-member="${userId}"]`).count(), 0)

  await settings.getByLabel('用户 ID').fill(userId)
  await selectChoice(settings.getByLabel('角色', { exact: true }), 'member')
  const readded = waitForMutation('PUT')
  await settings.getByRole('button', { name: '添加或更新成员' }).click()
  assert.equal((await readded).status(), 204)
  await settings.locator(`[data-platform-member="${userId}"]`).waitFor()
  await closeSpaceManagement(page)
}

async function setMemberRoleThroughUi(page, userId, roleName) {
  const settings = await openSpaceManagement(page)
  await settings.getByRole('tab', { name: '成员', exact: true }).click()
  const row = settings.locator(`[data-platform-member="${userId}"]`)
  await row.waitFor()
  await selectChoice(row.getByRole('combobox'), roleName)
  const updated = page.waitForResponse(response => (
    response.request().method() === 'PUT'
      && new URL(response.url()).pathname.endsWith(`/members/${encodeURIComponent(userId)}`)
  ))
  await row.getByRole('button', { name: '保存角色' }).click()
  assert.equal((await updated).status(), 204)
  await settings.getByText('成员角色已保存', { exact: true }).waitFor()
  await closeSpaceManagement(page)
}

async function waitForExecutionTarget(page, nodeId, connected = true) {
  await page.waitForFunction(async ({ executorId, expected }) => {
    const accessToken = sessionStorage.getItem('ternilo.oidc.access') ?? ''
    const tenantId = localStorage.getItem('ternilo.current-tenant') ?? ''
    const response = await fetch('/api/v1/execution-targets', {
      headers: { authorization: `Bearer ${accessToken}`, 'x-ternilo-tenant': tenantId },
    })
    if (!response.ok) return false
    const payload = await response.json()
    return payload.executors?.some(executor => executor.executor_id === executorId && executor.connected === expected)
  }, { executorId: nodeId, expected: connected }, { polling: 250, timeout: 30_000 })
}

async function chooseNodeWorkspace(page, {
  nodeId,
  baseDirectory,
  selectedDirectory,
  connectNode,
  projectName = 'Home Project',
  workspaceName = 'Home Workspace',
}) {
  await page.getByRole('button', { name: '选择工作文件夹' }).last().click()
  const placementDialog = page.getByRole('dialog', { name: '打开工作区' })
  await placementDialog.waitFor()
  const localPlacement = placementDialog.getByRole('button', { name: /我的电脑或 VPS/ })
  if (await localPlacement.count() && await localPlacement.getAttribute('aria-pressed') !== 'true') await localPlacement.click()
  await selectProject(page, placementDialog, projectName)
  await selectChoice(placementDialog.getByLabel('运行电脑'), nodeId)
  await placementDialog.getByLabel('工作区名称').fill(workspaceName)
  const chooseDirectory = placementDialog.getByRole('button', { name: '选择文件夹' })
  assert.match(await placementDialog.getByLabel('运行电脑').locator('option:checked').textContent() ?? '', /离线/)
  assert.equal(await chooseDirectory.isDisabled(), true)
  await connectNode()
  const deadline = Date.now() + 10_000
  while (await chooseDirectory.isDisabled() && Date.now() < deadline) {
    await new Promise(resolve => setTimeout(resolve, 250))
  }
  if (await chooseDirectory.isDisabled()) {
    const diagnostic = await page.evaluate(async () => {
      const accessToken = sessionStorage.getItem('ternilo.oidc.access') ?? ''
      const tenantId = localStorage.getItem('ternilo.current-tenant') ?? ''
      const response = await fetch('/api/v1/execution-targets', {
        headers: { authorization: `Bearer ${accessToken}`, 'x-ternilo-tenant': tenantId },
      })
      return {
        response: await response.json().catch(() => ({})),
        selects: [...document.querySelectorAll('select')].map(select => ({
          label: select.labels?.[0]?.textContent,
          value: select.value,
          options: [...select.options].map(option => ({ value: option.value, text: option.text, selected: option.selected })),
        })),
      }
    })
    throw new Error(`Node directory action remained disabled: ${JSON.stringify(diagnostic)}`)
  }
  assert.match(await placementDialog.getByLabel('运行电脑').locator('option:checked').textContent() ?? '', /在线/)
  await chooseDirectory.click()

  const directoryDialog = page.getByRole('dialog', { name: `选择 ${nodeId} 上的工作文件夹` })
  await directoryDialog.waitFor()
  await directoryDialog.getByRole('button', { name: '编辑文件夹路径' }).click()
  await directoryDialog.getByRole('textbox', { name: '编辑文件夹路径' }).fill(baseDirectory)
  await directoryDialog.getByRole('textbox', { name: '编辑文件夹路径' }).press('Enter')
  await directoryDialog.getByRole('list', { name: `目录 ${baseDirectory}`, exact: true }).waitFor()
    .catch(async cause => {
      const directoryState = await directoryDialog.evaluate(element => ({
        lists: [...element.querySelectorAll('[role="list"]')].map(list => list.getAttribute('aria-label')),
        alerts: [...element.querySelectorAll('[role="alert"]')].map(alert => alert.textContent),
        inputs: [...element.querySelectorAll('input')].map(input => ({ label: input.getAttribute('aria-label'), value: input.value })),
      }))
      throw new Error(`Node directory navigation did not settle: ${JSON.stringify(directoryState)}`, { cause })
    })

  await directoryDialog.getByRole('button', { name: '新建文件夹' }).click()
  const createDialog = page.getByRole('dialog', { name: '新建文件夹' })
  await createDialog.getByLabel('文件夹名称').fill(path.basename(selectedDirectory))
  await createDialog.getByRole('button', { name: '创建并选择' }).click()
  await createDialog.waitFor({ state: 'detached' })
  await directoryDialog.getByRole('list', { name: `目录 ${selectedDirectory}`, exact: true }).waitFor()
  await directoryDialog.getByRole('button', { name: '选择此文件夹' }).click()
  await directoryDialog.waitFor({ state: 'detached' })
  await placementDialog.getByRole('button', { name: '打开并开始会话', exact: true }).click()
  await placementDialog.waitFor({ state: 'detached' })
  const workspaceTitle = page.locator('[data-sidebar-workspace-title]').filter({ hasText: nodeId })
  await workspaceTitle.waitFor()
  assert.equal(await workspaceTitle.evaluate(element => [...element.childNodes]
    .filter(node => node.nodeType === Node.TEXT_NODE)
    .map(node => node.textContent).join('')), workspaceName)
}

test('Control browser drives an enrolled local Node workspace without connecting to the Node directly', { timeout: 300_000 }, async () => {
  const temporary = await mkdtemp(path.join(tmpdir(), 'ternilo-control-node-browser-'))
  const nodeData = path.join(temporary, 'node-data')
  const workspaceRoot = path.join(temporary, 'workspace-root')
  const selectedWorkspace = path.join(workspaceRoot, 'selected-by-browser')
  const memberNodeData = path.join(temporary, 'member-node-data')
  const memberWorkspaceRoot = path.join(temporary, 'member-workspace-root')
  const memberSelectedWorkspace = path.join(memberWorkspaceRoot, 'selected-by-member')
  await mkdir(nodeData)
  await mkdir(workspaceRoot)
  await mkdir(memberNodeData)
  await mkdir(memberWorkspaceRoot)
  const model = await startNodeModelFixture()

  const postgres = await startPostgres({
    prefix: 'ternilo-control-node-browser',
    database: 'ternilo_control_node_browser_test',
  })
  const oidc = await startOidcServer({
    audience: 'ternilo-control-node-e2e',
    identities: {
      owner: {
        subject: 'control-node-browser-user',
        email: 'control-node@example.com',
        name: 'Control Node Browser',
      },
      member: {
        subject: 'control-platform-member',
        email: 'member@example.com',
        name: 'Control Platform Member',
      },
    },
    initialIdentity: 'owner',
  })
  const port = await freePort()
  const origin = `http://127.0.0.1:${port}`
  const nodeLocalPort = await freePort()
  const nodeLocalOrigin = `http://127.0.0.1:${nodeLocalPort}`
  const memberNodeLocalPort = await freePort()
  const memberNodeLocalOrigin = `http://127.0.0.1:${memberNodeLocalPort}`
  const policyPath = path.join(temporary, 'worker-policy.json')
  await writeFile(policyPath, JSON.stringify({
    catalog_revision: 'ternilo-cloud-v2',
    policy_revision: 'control-node-browser-e2e-v1',
    maximum_limits: { max_steps: 4, max_tool_calls: 8 },
    max_run_attempts: 2,
    max_tenant_workspace_bytes: 1073741824,
    max_tenant_workspace_entries: 100000,
    minimum_workspace_free_bytes: 0,
    max_extension_packages_per_run: 0,
    extension_host_policy: {
      allowed_capabilities: [],
      maximum_rhai_limits: {
        max_operations: 1000000,
        max_string_bytes: 1048576,
        max_collection_items: 100000,
        max_call_levels: 64,
        max_expr_depth: 64,
        max_variables: 4096,
        max_functions: 256,
        max_wall_ms: 60000,
        max_input_bytes: 1048576,
        max_output_bytes: 2097152,
        max_workspace_read_bytes: 2097152,
      },
      maximum_wasm_component_limits: {
        fuel: 20000000,
        max_memory_bytes: 67108864,
        max_input_bytes: 1048576,
        max_output_bytes: 2097152,
        max_workspace_read_bytes: 2097152,
      },
      max_payload_bytes: 16777216,
    },
    allowed_plugin_kinds: [
      'ternilo.session.log', 'ternilo.prompt.registry', 'ternilo.tools.registry',
      'ternilo.hooks.registry', 'ternilo.prompt.system', 'ternilo.prompt.identity',
      'ternilo.model.host_gateway', 'ternilo.session_title.llm', 'ternilo.context.compaction',
      'ternilo.sandbox.cloud_outer', 'ternilo.files.local', 'ternilo.shell.local',
      'ternilo.prompt.workspace_instructions', 'ternilo.tools.files',
      'ternilo.tool.shell', 'ternilo.tool.plan', 'ternilo.skills.registry',
      'ternilo.skills.filesystem', 'ternilo.tools.skills',
      'ternilo.code_runtime.rhai', 'ternilo.tools.code_mode', 'ternilo.agent.react',
    ],
    denied_tools: [],

  }))

  const environment = {
    TERNILO_DATABASE_URL: postgres.url,
    TERNILO_MIGRATION_DATABASE_URL: postgres.url,
    TERNILO_SECRET_MASTER_KEY: Buffer.alloc(32, 19).toString('base64'),
  }
  let control
  const nodeId = 'home-e2e'
  const memberNodeId = 'member-home-e2e'
  let node
  let memberNode
  let memberCredentialToken
  const nodeRuns = []
  let browser
  try {
    control = await initializeServer({
      directory: path.join(temporary, 'server'),
      origin,
      binary: controlBinary,
      environment,
      databaseUrl: postgres.url,
      migrationDatabaseUrl: postgres.url,
      owner: { username: 'owner', password: 'control-node-owner-password' },
      ownerOidcToken: oidc.accessToken('owner'),
      oidc: {
        issuer: oidc.issuer,
        audience: 'ternilo-control-node-e2e',
        client_id: 'ternilo-control-node-browser',
        allow_insecure: true,
      },
      workerPolicy: policyPath,
    })
    const registrationPolicy = await serverRequest(origin, '/admin/registration', { token: control.owner.session.access_token })
    await serverRequest(origin, '/admin/registration', { token: control.owner.session.access_token, method: 'PATCH', body: { mode: 'open', require_approval: false, revision: registrationPolicy.revision } })
    await waitForHttp(`${origin}/health`, control)
    browser = await chromium.launch({
      headless: true,
      executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined,
    })
    const context = await browser.newContext({ viewport: { width: 1440, height: 900 } })
    const page = await context.newPage()
    const pageErrors = []
    const consoleMessages = []
    const browserOrigins = new Set()
    const browserWebSockets = []
    const backgroundLiveRequests = []
    page.on('pageerror', error => pageErrors.push(error.message))
    page.on('console', message => consoleMessages.push(message.text()))
    page.on('request', request => {
      const url = new URL(request.url())
      browserOrigins.add(url.origin)
      if (url.pathname === '/api/v1/questions'
        || /\/api\/v1\/sessions\/[^/]+\/(?:event-delta|queue|stats|projection|plugins)$/.test(url.pathname)) {
        backgroundLiveRequests.push({ method: request.method(), url: request.url(), at: Date.now() })
      }
    })
    page.on('websocket', socket => {
      const record = { url: socket.url(), openedAt: Date.now(), closedAt: null, sent: [], received: [] }
      browserWebSockets.push(record)
      const capture = target => event => {
        if (typeof event.payload !== 'string') return
        try { target.push({ frame: JSON.parse(event.payload), at: Date.now() }) } catch {}
      }
      socket.on('framesent', capture(record.sent))
      socket.on('framereceived', capture(record.received))
      socket.on('close', () => { record.closedAt = Date.now() })
    })

    await page.goto(origin, { waitUntil: 'domcontentloaded' })
    await page.getByRole('button', { name: '使用组织账号登录' }).click()
    await selectSpace(page, control.owner.session.personal_tenant_id)
    const teamId = await createTeamThroughUi(page, 'Control Node Team', 'control-node-team')

    const memberUserId = await registerMemberThroughOidc(browser, origin, oidc)
    await manageMemberThroughUi(page, memberUserId)

    await context.grantPermissions(['clipboard-read', 'clipboard-write'], { origin })
    const projectId = await createProjectThroughUi(page)

    oidc.selectIdentity('member')
    const memberContext = await browser.newContext({ viewport: { width: 1280, height: 800 } })
    const memberPage = await memberContext.newPage()
    const memberPageErrors = []
    memberPage.on('pageerror', error => memberPageErrors.push(error.message))
    await memberPage.goto(origin, { waitUntil: 'domcontentloaded' })
    await memberPage.getByRole('button', { name: '使用组织账号登录' }).click()
    await selectSpace(memberPage, teamId)
    oidc.selectIdentity('owner')

    memberCredentialToken = await createOwnedEnrollmentThroughUi(memberPage, memberNodeId, projectId)
    await memberPage.getByRole('button', { name: '选择工作文件夹' }).last().click()
    const memberPlacementPreview = memberPage.getByRole('dialog', { name: '打开工作区' })
    await memberPlacementPreview.waitFor()
    await memberPlacementPreview.locator('option', { hasText: 'Home Project' }).waitFor({ state: 'attached', timeout: 30_000 })
    assert.equal(await memberPlacementPreview.locator('#new-cloud-project').count(), 0)
    assert.match(await memberPlacementPreview.textContent() ?? '', /Home Project/)
    await memberPlacementPreview.getByRole('button', { name: '取消' }).click()

    await chooseNodeWorkspace(memberPage, {
      nodeId: memberNodeId,
      baseDirectory: memberWorkspaceRoot,
      selectedDirectory: memberSelectedWorkspace,
      workspaceName: 'Member Workspace',
      connectNode: async () => {
        memberNode = startProcess(nodeBinary, ['serve',
          '--gateway-url', `${origin.replace('http://', 'ws://')}/api/v1/executors/connect`,
          '--allow-insecure-gateway',
          '--node-id', memberNodeId,
          '--data-dir', memberNodeData,
          '--listen', `127.0.0.1:${memberNodeLocalPort}`,
        ], { TERNILO_LOCAL_TOKEN: memberCredentialToken })
        nodeRuns.push(memberNode)
        await waitForHttp(memberNodeLocalOrigin, memberNode)
        await waitForExecutionTarget(memberPage, memberNodeId)
      },
    })
    await memberPage.locator('[data-sidebar-new-session]').click()
    const memberPrompt = memberPage.getByRole('textbox', { name: '输入任务' })
    await memberPrompt.fill('/')
    await memberPage.getByRole('option', { name: /\/write/ }).waitFor({ timeout: 30_000 })
    await memberPrompt.fill('/write member-proof.txt member-owned-node-ready')
    await memberPage.getByRole('button', { name: '发送' }).click()
    await revealWriteTool(memberPage, 'member-proof.txt', 'member-owned-node-ready')
    await waitForFile(path.join(memberSelectedWorkspace, 'member-proof.txt'), 'member-owned-node-ready')
    const memberSessionId = await memberPage.evaluate(() => localStorage.getItem('ternilo.current-session'))
    assert.ok(memberSessionId)

    await setMemberRoleThroughUi(page, memberUserId, 'viewer')
    await memberPage.reload({ waitUntil: 'domcontentloaded' })
    await selectSpace(memberPage, teamId)
    const viewerSession = memberPage.locator(`[data-sidebar-session-row][data-session-id="${memberSessionId}"]`)
    await viewerSession.waitFor({ timeout: 30_000 })
    await viewerSession.click()
    await memberPage.locator('[data-viewer-read-only]').waitFor()
    assert.equal(await memberPage.locator('[data-sidebar-new-session]').count(), 0)
    assert.equal(await memberPage.getByRole('textbox', { name: '输入任务' }).count(), 0)
    assert.equal(await memberPage.locator('[data-session-workspace]').evaluate(element => element.tagName), 'SPAN')
    await memberPage.keyboard.press('Control+k')
    assert.equal(await memberPage.getByRole('dialog', { name: '打开工作区' }).count(), 0)
    assert.equal(await memberPage.getByRole('button', { name: '空间管理', exact: true }).count(), 0)
    assert.equal(await memberPage.getByRole('button', { name: '平台管理', exact: true }).count(), 0)
    await memberPage.getByRole('button', { name: '用户设置', exact: true }).click()
    const viewerSettings = memberPage.locator('[data-user-settings]')
    assert.equal(await viewerSettings.getByRole('button', { name: '我的机器', exact: true }).count(), 0)
    assert.equal(await viewerSettings.getByRole('button', { name: '平台管理', exact: true }).count(), 0)
    await viewerSettings.getByRole('button', { name: '返回工作台' }).click()
    const viewerNodeStateBefore = await readFile(path.join(memberNodeData, 'state.json'), 'utf8')
    const viewerNodeSessionsBefore = await snapshotNodeSessionFiles(memberNodeData)
    const viewerMutationAudit = await memberPage.evaluate(async ({ sessionId, nodeId, directory }) => {
      const token = sessionStorage.getItem('ternilo.oidc.access') ?? ''
      const tenant = localStorage.getItem('ternilo.current-tenant') ?? ''
      const headers = {
        authorization: `Bearer ${token}`,
        'content-type': 'application/json',
        'x-ternilo-tenant': tenant,
      }
      const request = async (url, method = 'GET', body) => {
        const response = await fetch(url, {
          method,
          headers,
          body: body === undefined ? undefined : JSON.stringify(body),
        })
        return {
          status: response.status,
          body: await response.json().catch(() => null),
        }
      }
      const sessionPath = `/api/v1/sessions/${encodeURIComponent(sessionId)}`
      const beforeState = await request('/api/v1/state')
      const beforeQueue = await request(`${sessionPath}/queue`)
      const mutations = await Promise.all([
        request(`${sessionPath}/turns`, 'POST', {
          input: '/write viewer-turn-bypass.txt forbidden',
          run_id: 'viewer-turn-bypass',
          attachments: [],
        }),
        request(`${sessionPath}/queue`, 'POST', {
          delivery: 'queue',
          run_id: 'viewer-queue-bypass',
          content: { kind: 'prompt', input: '/write viewer-queue-bypass.txt forbidden' },
          references: [],
          attachments: [],
        }),
        request(sessionPath, 'PATCH', { title: 'viewer-renamed-session' }),
        request(sessionPath, 'DELETE'),
        request(`/api/v1/executors/${encodeURIComponent(nodeId)}/directories`, 'POST', {
          parent: directory,
          name: 'viewer-created-directory',
        }),
      ])
      const afterState = await request('/api/v1/state')
      const afterQueue = await request(`${sessionPath}/queue`)
      return { beforeState, beforeQueue, mutations, afterState, afterQueue }
    }, {
      sessionId: memberSessionId,
      nodeId: memberNodeId,
      directory: memberSelectedWorkspace,
    })
    assert.equal(viewerMutationAudit.beforeState.status, 200)
    assert.equal(viewerMutationAudit.beforeQueue.status, 200)
    assert.deepEqual(viewerMutationAudit.mutations.map(result => result.status), [403, 403, 403, 403, 403])
    for (const result of viewerMutationAudit.mutations) {
      assert.equal(result.body?.error?.code, 'policy_denied')
    }
    assert.equal(viewerMutationAudit.afterState.status, 200)
    assert.equal(viewerMutationAudit.afterQueue.status, 200)
    assert.deepEqual(viewerMutationAudit.afterState.body, viewerMutationAudit.beforeState.body)
    assert.deepEqual(viewerMutationAudit.afterQueue.body, viewerMutationAudit.beforeQueue.body)
    assert.equal(await readFile(path.join(memberNodeData, 'state.json'), 'utf8'), viewerNodeStateBefore)
    assert.deepEqual(await snapshotNodeSessionFiles(memberNodeData), viewerNodeSessionsBefore)
    await assert.rejects(stat(path.join(memberSelectedWorkspace, 'viewer-turn-bypass.txt')), { code: 'ENOENT' })
    await assert.rejects(stat(path.join(memberSelectedWorkspace, 'viewer-queue-bypass.txt')), { code: 'ENOENT' })
    await assert.rejects(stat(path.join(memberSelectedWorkspace, 'viewer-created-directory')), { code: 'ENOENT' })
    assert.equal(await readFile(path.join(memberSelectedWorkspace, 'member-proof.txt'), 'utf8'), 'member-owned-node-ready')
    assert.deepEqual(memberPageErrors, [])
    await stopProcess(memberNode)
    await memberContext.close()

    const credentialToken = await createEnrollmentThroughUi(page, nodeId, projectId)
    const startNode = gatewayUrl => {
      node = startProcess(nodeBinary, ['serve',
        '--gateway-url', gatewayUrl,
        '--allow-insecure-gateway',
        '--node-id', nodeId,
        '--data-dir', nodeData,
        '--listen', `127.0.0.1:${nodeLocalPort}`,
      ], { TERNILO_LOCAL_TOKEN: credentialToken })
      nodeRuns.push(node)
      return node
    }
    const bootstrap = {
      tenantId: await page.getByRole('combobox', { name: '切换空间', exact: true }).getAttribute('data-space-id'),
      projectId,
    }
    await chooseNodeWorkspace(page, {
      nodeId,
      baseDirectory: workspaceRoot,
      selectedDirectory: selectedWorkspace,
      connectNode: async () => {
        startNode(`${origin.replace('http://', 'ws://')}/api/v1/executors/connect`)
        await waitForHttp(nodeLocalOrigin, node)
        await waitForExecutionTarget(page, nodeId)
      },
    })
    assert.equal((await stat(selectedWorkspace)).isDirectory(), true)

    await page.locator('[data-sidebar-new-session]').click()
    const prompt = page.getByRole('textbox', { name: '输入任务' })
    await prompt.waitFor()
    await prompt.fill('/')
    await page.getByRole('option', { name: /\/write/ }).waitFor({ timeout: 30_000 })
    await prompt.fill('/write control-node-proof.txt control-node-chain-ready')
    await page.getByRole('button', { name: '发送' }).click()
    assert.equal(await prompt.inputValue(), '')
    await revealWriteTool(page)
    await page.getByRole('button', { name: '发送' }).waitFor({ timeout: 30_000 })
    await waitForFile(path.join(selectedWorkspace, 'control-node-proof.txt'), 'control-node-chain-ready')
    const sessionId = await page.evaluate(() => localStorage.getItem('ternilo.current-session'))
    assert.ok(sessionId)

    await configureNodeProviderThroughUi(page, model.baseUrl, 'Home Workspace')
    const providerScopes = await page.evaluate(async selectedSessionId => {
      const token = sessionStorage.getItem('ternilo.oidc.access') ?? ''
      const tenant = localStorage.getItem('ternilo.current-tenant') ?? ''
      const headers = { authorization: `Bearer ${token}`, 'x-ternilo-tenant': tenant }
      const [cloudResponse, nodeResponse] = await Promise.all([
        fetch('/api/v1/providers', { headers }),
        fetch(`/api/v1/providers?session_id=${encodeURIComponent(selectedSessionId)}`, { headers }),
      ])
      return {
        cloudStatus: cloudResponse.status,
        nodeStatus: nodeResponse.status,
        cloud: await cloudResponse.json(),
        node: await nodeResponse.json(),
      }
    }, sessionId)
    assert.equal(providerScopes.cloudStatus, 200)
    assert.equal(providerScopes.nodeStatus, 200)
    assert.equal(providerScopes.cloud.some(provider => provider.id === 'node-browser'), false)
    assert.equal(providerScopes.node.some(provider => provider.id === 'node-browser'), true)

    const modelTask = '只回复 control-node-model-ready，不要调用工具'
    await prompt.fill(modelTask)
    await page.getByRole('button', { name: '发送' }).click()
    const modelRequest = await model.waitForRequest(request => (
      request.body.model === 'node-browser-model'
      && JSON.stringify(request.body.input).includes(modelTask)
    ))
    await page.getByText('control-node-model-ready', { exact: true }).waitFor({ timeout: 30_000 })
    assert.equal(modelRequest.authorization, 'Bearer node-browser-secret')
    assert.equal(modelRequest.body.reasoning?.effort, 'ultra')
    assert.equal(modelRequest.body.stream, true)
    await captureArtifact(page, 'control-node-workbench-desktop.png')

    await page.reload({ waitUntil: 'domcontentloaded' })
    const restoredSession = page.locator(`[data-sidebar-session-row][data-session-id="${sessionId}"]`)
    await restoredSession.waitFor({ timeout: 30_000 })
    await restoredSession.click()
    await prompt.waitFor()
    assert.equal(await page.evaluate(() => localStorage.getItem('ternilo.current-session')), sessionId)

    const localStateText = await readFile(path.join(nodeData, 'state.json'), 'utf8')
    const localSessionFiles = (await readdir(path.join(nodeData, 'sessions')))
      .filter(file => file.endsWith('.jsonl'))
    assert.ok(localSessionFiles.length > 0)
    const localEventLog = (await Promise.all(localSessionFiles.map(file => (
      readFile(path.join(nodeData, 'sessions', file), 'utf8')
    )))).join('\n')
    assert.equal(localStateText.includes(selectedWorkspace), true)
    assert.equal(localStateText.includes('<local-workspace>'), false)
    assert.equal(localEventLog.includes('control-node-proof.txt'), true)
    assert.equal(localEventLog.includes('control-node-chain-ready'), true)
    assert.equal(localEventLog.includes('<local-workspace>'), false)

    const localContext = await browser.newContext({ viewport: { width: 1280, height: 800 } })
    const localPage = await localContext.newPage()
    await localPage.goto(nodeLocalOrigin, { waitUntil: 'domcontentloaded' })
    await localPage.getByRole('button', { name: selectedWorkspace, exact: true }).waitFor({ timeout: 30_000 })
    assert.equal((await localPage.locator('html').textContent() ?? '').includes('<local-workspace>'), false)

    const offlineSearchTitle = 'Offline edge search proof'
    const renameForOfflineSearch = await page.evaluate(async ({ edgeSessionId, title }) => {
      const token = sessionStorage.getItem('ternilo.oidc.access') ?? ''
      const tenant = localStorage.getItem('ternilo.current-tenant') ?? ''
      const response = await fetch(`/api/v1/sessions/${encodeURIComponent(edgeSessionId)}`, {
        method: 'PATCH',
        headers: {
          authorization: `Bearer ${token}`,
          'content-type': 'application/json',
          'x-ternilo-tenant': tenant,
        },
        body: JSON.stringify({ title }),
      })
      return { status: response.status, body: await response.json().catch(() => null) }
    }, { edgeSessionId: sessionId, title: offlineSearchTitle })
    assert.equal(renameForOfflineSearch.status, 200, JSON.stringify(renameForOfflineSearch.body))
    await page.reload({ waitUntil: 'domcontentloaded' })
    await page.locator(`[data-sidebar-session-row][data-session-id="${sessionId}"]`)
      .getByText(offlineSearchTitle, { exact: true }).waitFor({ timeout: 30_000 })

    await stopProcess(node)
    await waitForExecutionTarget(page, nodeId, false)
    const offlineSettings = await openSpaceManagement(page)
    await offlineSettings.getByRole('tab', { name: '电脑', exact: true }).click()
    await offlineSettings.getByRole('button', { name: '刷新', exact: true }).click()
    await offlineSettings.locator(`[data-platform-computer="${nodeId}"]`).getByText('离线', { exact: true }).waitFor()
    await closeSpaceManagement(page)

    const offlineSearch = await page.evaluate(async ({ query, edgeSessionId }) => {
      const token = sessionStorage.getItem('ternilo.oidc.access') ?? ''
      const tenant = localStorage.getItem('ternilo.current-tenant') ?? ''
      const startedAt = performance.now()
      const response = await fetch(`/api/v1/session-search?${new URLSearchParams({ query, limit: '100' })}`, {
        headers: { authorization: `Bearer ${token}`, 'x-ternilo-tenant': tenant },
      })
      const hits = await response.json().catch(() => null)
      return { status: response.status, elapsedMs: performance.now() - startedAt, hits, edgeSessionId }
    }, { query: 'offline edge search', edgeSessionId: sessionId })
    assert.equal(offlineSearch.status, 200, JSON.stringify(offlineSearch.hits))
    assert.ok(offlineSearch.elapsedMs < 2_000, `offline search took ${offlineSearch.elapsedMs} ms`)
    assert.equal(offlineSearch.hits.some(hit => (
      hit.session_id === offlineSearch.edgeSessionId && hit.title === offlineSearchTitle
    )), true)
    assert.equal(JSON.stringify(offlineSearch.hits).includes(selectedWorkspace), false)
    assert.equal(JSON.stringify(offlineSearch.hits).includes(workspaceRoot), false)
    await page.getByRole('button', { name: '搜索会话' }).click()
    const offlineSearchInput = page.getByRole('textbox', { name: '搜索会话' })
    await offlineSearchInput.fill('offline edge search')
    await page.locator(`[data-sidebar-session-row][data-session-id="${sessionId}"]`)
      .locator('[data-sidebar-session-button][data-search-result]')
      .getByText(offlineSearchTitle, { exact: true }).waitFor({ timeout: 10_000 })
    const offlineSearchBrowser = page.locator('[data-workspace-browser-state]')
    assert.equal(await offlineSearchBrowser.getAttribute('data-workspace-browser-state'), 'ready')
    assert.equal(await offlineSearchBrowser.getByRole('alert').count(), 0)
    await page.getByRole('button', { name: '关闭搜索' }).click()

    const offlineSocketBoundary = browserWebSockets.length
    const offlineRequestBoundary = backgroundLiveRequests.length
    await page.reload({ waitUntil: 'domcontentloaded' })
    await restoredSession.waitFor({ timeout: 30_000 })
    await restoredSession.click()
    await revealWriteTool(page)
    await page.locator('[data-sidebar-connection][data-live-state="ready"]').waitFor({ timeout: 30_000 })
    await new Promise(resolve => setTimeout(resolve, 1_500))
    assert.deepEqual(
      backgroundLiveRequests.slice(offlineRequestBoundary),
      [],
      'offline Edge history must not fall back to periodic REST metadata/history requests',
    )
    const offlineLiveSockets = browserWebSockets.slice(offlineSocketBoundary)
      .filter(record => new URL(record.url).pathname === '/api/v1/live')
    assert.ok(offlineLiveSockets.length >= 1, JSON.stringify(offlineLiveSockets))
    assert.ok(
      offlineLiveSockets.length <= 4,
      `offline Edge live reconnect must back off; observed ${offlineLiveSockets.length} sockets`,
    )
    for (let index = 1; index < offlineLiveSockets.length; index++) {
      assert.ok(
        offlineLiveSockets[index].openedAt - offlineLiveSockets[index - 1].openedAt >= 200,
        `offline Edge live reconnect spun without backoff: ${JSON.stringify(offlineLiveSockets)}`,
      )
    }
    const offlineLiveSocket = [...offlineLiveSockets].reverse().find(record => (
      record.sent.some(({ frame }) => frame.type === 'subscribe' && frame.session_id === sessionId)
    ))
    assert.ok(offlineLiveSocket, JSON.stringify(offlineLiveSockets))

    const disconnectedGatewayPort = await freePort()
    startNode(`ws://127.0.0.1:${disconnectedGatewayPort}/api/v1/executors/connect`)
    await waitForHttp(nodeLocalOrigin, node)
    await waitForExecutionTarget(page, nodeId, false)
    await localPage.reload({ waitUntil: 'domcontentloaded' })
    const localSession = localPage.locator('[data-sidebar-session-row]').first()
    await localSession.waitFor({ timeout: 30_000 })
    await localSession.click()
    const localPrompt = localPage.getByRole('textbox', { name: '输入任务' })
    await localPrompt.fill('/')
    await localPage.getByRole('option', { name: /\/write/ }).waitFor({ timeout: 30_000 })
    await localPrompt.fill('/write control-node-reconnect.txt replayed-after-reconnect')
    await localPage.getByRole('button', { name: '发送' }).click()
    await waitForFile(path.join(selectedWorkspace, 'control-node-reconnect.txt'), 'replayed-after-reconnect')
    assert.equal(
      await localPage.locator('article[data-role="user"]').filter({ hasText: '/write control-node-reconnect.txt replayed-after-reconnect' }).count(),
      1,
    )
    assert.equal(
      await page.locator('article[data-role="user"]').filter({ hasText: '/write control-node-reconnect.txt replayed-after-reconnect' }).count(),
      0,
    )

    await stopProcess(node)
    startNode(`${origin.replace('http://', 'ws://')}/api/v1/executors/connect`)
    await waitForHttp(nodeLocalOrigin, node)
    await waitForExecutionTarget(page, nodeId)
    const replayedUser = page.locator('article[data-role="user"]').filter({ hasText: '/write control-node-reconnect.txt replayed-after-reconnect' })
    await replayedUser.waitFor({ timeout: 30_000 })
    assert.equal(await replayedUser.count(), 1)
    const replayFrames = offlineLiveSocket.received
      .flatMap(({ frame }) => frame.type === 'event_batch' ? frame.events : [])
      .filter(event => event.run_id && JSON.stringify(event).includes('control-node-reconnect.txt'))
    assert.deepEqual(
      [...new Set(replayFrames.map(event => event.seq))],
      replayFrames.map(event => event.seq),
      'Node reconnect cursor repair must not deliver duplicate durable events',
    )
    assert.ok(replayFrames.length > 0, JSON.stringify(offlineLiveSocket.received))
    await revealWriteTool(page, 'control-node-reconnect.txt', 'replayed-after-reconnect')
    await page.waitForFunction(() => ![...document.querySelectorAll('[role="alert"]')]
      .some(element => /selected Ternilo node is offline|Ternilo node.*offline|节点.*离线/i.test(element.textContent ?? '')))
      .catch(async cause => {
        const alerts = await page.locator('[role="alert"]').evaluateAll(elements => elements.map(element => ({
          text: element.textContent,
          className: element.className,
          parentClassName: element.parentElement?.className,
        })))
        throw new Error(`Node reconnected but an offline alert remained: ${JSON.stringify(alerts)}`, { cause })
      })

    await page.reload({ waitUntil: 'domcontentloaded' })
    await replayedUser.waitFor({ timeout: 30_000 })
    assert.equal(await replayedUser.count(), 1, 'replayed Node events must remain deduplicated after reload')
    await localContext.close()

    await page.setViewportSize({ width: 390, height: 844 })
    await page.waitForFunction(() => (document.querySelector('.app-sidebar')?.getBoundingClientRect().right ?? 0) <= 0)
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth), 390)
    assert.equal(await prompt.isVisible(), true)
    const composer = await page.locator('.composer-shell').boundingBox()
    assert.ok(composer)
    assert.equal(composer.x >= 8 && composer.x + composer.width <= 382, true)

    await page.getByRole('button', { name: '打开侧边栏' }).click()
    let mobileSettings = await openSpaceManagement(page)
    assert.equal(await mobileSettings.evaluate(element => element.scrollWidth <= element.clientWidth), true)
    await mobileSettings.getByText('control-node@example.com', { exact: true }).waitFor()
    await mobileSettings.getByRole('tab', { name: '电脑', exact: true }).click()
    const computerRow = mobileSettings.locator(`[data-platform-computer="${nodeId}"]`)
    await computerRow.getByText('在线', { exact: true }).waitFor()
    assert.equal(await mobileSettings.locator('[data-platform-settings]').evaluate(element => element.scrollWidth <= element.clientWidth), true)

    await closeSpaceManagement(page)
    if (!await page.getByRole('button', { name: '用户设置', exact: true }).isVisible()) {
      await page.getByRole('button', { name: '打开侧边栏' }).click()
    }
    await page.getByRole('button', { name: '用户设置', exact: true }).click()
    const preferences = page.locator('[data-user-settings]')
    await preferences.getByRole('button', { name: '通用', exact: true }).click()
    await selectChoice(preferences.getByLabel('语言'), 'en')
    await page.locator('[data-user-settings]').getByRole('button', { name: 'Back to workbench' }).click()
    mobileSettings = await openSpaceManagement(page, 'en')
    await mobileSettings.getByRole('heading', { name: 'Space management', exact: true }).waitFor()
    await mobileSettings.getByRole('tab', { name: 'Computers', exact: true }).click()
    await mobileSettings.locator(`[data-platform-computer="${nodeId}"]`).getByText('Online', { exact: true }).waitFor()
    await captureArtifact(page, 'control-node-computers-mobile.png')
    assert.equal(await mobileSettings.evaluate(element => element.scrollWidth <= element.clientWidth), true)
    await closeSpaceManagement(page, 'en')
    const mobileSidebarClose = page.locator('[data-mobile-sidebar-close]')
    if (await mobileSidebarClose.isVisible()) await mobileSidebarClose.click()
    await page.locator('[data-app-frame][data-mobile-sidebar-open]').waitFor({ state: 'detached' })

    const clientState = await page.evaluate(() => ({
      html: document.documentElement.outerHTML,
      local: Object.fromEntries(Array.from({ length: localStorage.length }, (_, index) => localStorage.key(index)).filter(Boolean).map(key => [key, localStorage.getItem(key)])),
      session: Object.fromEntries(Array.from({ length: sessionStorage.length }, (_, index) => sessionStorage.key(index)).filter(Boolean).map(key => [key, sessionStorage.getItem(key)])),
    }))
    const nodeDiagnostics = nodeRuns.map(process => process.diagnostics()).join('\n')
    const accessToken = clientState.session['ternilo.oidc.access']
    assert.ok(accessToken)
    assert.equal(JSON.stringify(clientState).includes(selectedWorkspace), false)
    assert.equal(JSON.stringify(clientState).includes(workspaceRoot), false)
    const exported = await page.evaluate(async exportedSessionId => {
      const token = sessionStorage.getItem('ternilo.oidc.access') ?? ''
      const tenant = localStorage.getItem('ternilo.current-tenant') ?? ''
      const response = await fetch(`/api/v1/sessions/${encodeURIComponent(exportedSessionId)}/export`, {
        headers: { authorization: `Bearer ${token}`, 'x-ternilo-tenant': tenant },
      })
      if (!response.ok) throw new Error(`edge session export failed: ${response.status}`)
      return response.json()
    }, sessionId)
    const exportedText = JSON.stringify(exported)
    assert.equal(exportedText.includes(selectedWorkspace), false)
    assert.equal(exportedText.includes(workspaceRoot), false)
    assert.equal(exportedText.includes('此电脑 / 已绑定 Workspace'), true)
    const expectedExportFilename = `${exported.session.title}.json`
    await page.setViewportSize({ width: 1440, height: 900 })
    const desktopDownload = await downloadSessionFromMenu(page, 'More session actions', 'Export session', sessionId)
    assert.equal(desktopDownload.filename, expectedExportFilename)
    assert.deepEqual(desktopDownload.response, exported)
    assert.deepEqual(desktopDownload.value, exported)
    const desktopRepeatDownload = await downloadSessionFromMenu(page, 'More session actions', 'Export session', sessionId)
    assert.equal(desktopRepeatDownload.filename, expectedExportFilename)
    assert.deepEqual(desktopRepeatDownload.response, exported)
    assert.deepEqual(desktopRepeatDownload.value, exported)
    await page.setViewportSize({ width: 390, height: 844 })
    const mobileDownload = await downloadSessionFromMenu(page, 'More session actions', 'Export session', sessionId)
    assert.equal(mobileDownload.filename, expectedExportFilename)
    assert.deepEqual(mobileDownload.response, exported)
    assert.deepEqual(mobileDownload.value, exported)
    const downloadedExports = JSON.stringify([desktopDownload.value, mobileDownload.value])
    for (const secret of [credentialToken, 'control-node-model-fixture', accessToken]) {
      assert.equal(downloadedExports.includes(secret), false)
    }
    const audit = await page.evaluate(async tenantId => {
      const token = sessionStorage.getItem('ternilo.oidc.access') ?? ''
      const response = await fetch(`/api/v1/tenants/${encodeURIComponent(tenantId)}/audit?limit=200`, {
        headers: { authorization: `Bearer ${token}` },
      })
      const payload = await response.json().catch(() => ({}))
      if (!response.ok) throw new Error(`audit export failed: ${response.status}: ${JSON.stringify(payload)}`)
      return payload
    }, bootstrap.tenantId)
    const auditText = JSON.stringify(audit)
    assert.equal(audit.entries.some(entry => entry.action === 'workspace.create' && entry.metadata?.placement === 'local_node'), true)
    assert.equal(audit.entries.some(entry => entry.action === 'edge_session.create' && entry.metadata?.placement === 'local_node'), true)
    assert.equal(auditText.includes(selectedWorkspace), false)
    assert.equal(auditText.includes(workspaceRoot), false)
    const controlDatabaseDump = await postgres.dump()
    assert.equal(controlDatabaseDump.includes(selectedWorkspace), false)
    assert.equal(controlDatabaseDump.includes(workspaceRoot), false)
    assert.equal(controlDatabaseDump.includes(sessionId), true)
    assert.equal(control.diagnostics().includes(selectedWorkspace), false)
    assert.equal(control.diagnostics().includes(workspaceRoot), false)
    for (const secret of [credentialToken, memberCredentialToken]) {
      assert.equal(JSON.stringify(clientState).includes(secret), false)
      assert.equal(exportedText.includes(secret), false)
      assert.equal(auditText.includes(secret), false)
      assert.equal(controlDatabaseDump.includes(secret), false)
      assert.equal(consoleMessages.join('\n').includes(secret), false)
      assert.equal(control.diagnostics().includes(secret), false)
      assert.equal(nodeDiagnostics.includes(secret), false)
    }
    assert.equal(clientState.html.includes(accessToken), false)
    assert.equal(JSON.stringify(clientState.local).includes(accessToken), false)
    assert.equal(exportedText.includes(accessToken), false)
    assert.equal(controlDatabaseDump.includes(accessToken), false)
    assert.equal(consoleMessages.join('\n').includes(accessToken), false)
    assert.equal(control.diagnostics().includes(accessToken), false)
    assert.equal(nodeDiagnostics.includes(accessToken), false)

    await page.getByRole('button', { name: 'Open sidebar', exact: true }).click()
    await page.locator('[data-app-frame][data-mobile-sidebar-open]').waitFor()
    const revokeSettings = await openSpaceManagement(page, 'en')
    await revokeSettings.getByRole('tab', { name: 'Computers', exact: true }).click()
    const revokeRow = revokeSettings.locator(`[data-platform-computer="${nodeId}"]`)
    await revokeRow.getByRole('button', { name: 'Revoke computer' }).click()
    const revokeDialog = page.getByRole('dialog', { name: 'Revoke this computer?' })
    await revokeDialog.getByRole('button', { name: 'Revoke computer', exact: true }).click()
    await revokeDialog.waitFor({ state: 'detached' })
    await revokeRow.getByText('Revoked', { exact: true }).waitFor()
    await revokeRow.getByText('Offline', { exact: true }).waitFor()
    await closeSpaceManagement(page, 'en')

    assert.deepEqual([...browserOrigins].sort(), [oidc.issuer, origin].sort())
    assert.equal(browserWebSockets.every(record => new URL(record.url).origin === origin.replace('http://', 'ws://')), true)
    assert.deepEqual(pageErrors, [])
  } catch (error) {
    throw new Error(`${error instanceof Error ? error.message : String(error)}\ncontrol diagnostics: ${control?.diagnostics() ?? 'not started'}${nodeRuns.length ? `\nnode diagnostics: ${nodeRuns.map(process => process.diagnostics()).join('\n--- node restart ---\n')}` : ''}`, { cause: error })
  } finally {
    await browser?.close()
    for (const process of nodeRuns) await stopProcess(process)
    await stopProcess(control)
    await oidc.close()
    await postgres.stop()
    await model.close()
    await rm(temporary, { recursive: true, force: true })
  }
})
