import { selectChoice } from './browser-select-fixture.mjs'
import assert from 'node:assert/strict'
import { mkdtemp, mkdir, readFile, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { createServer } from 'node:http'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import { execFile, execFileSync, spawn } from 'node:child_process'
import { promisify } from 'node:util'
import test from 'node:test'
import { chromium } from 'playwright'
import { freePort, initializeServer, serverRequest } from './platform-e2e-fixture.mjs'
import { choose, closeSettings, computerModels, settings as modelSettings } from './account-node-provider-fixture.mjs'

const webRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..')
const repository = path.resolve(webRoot, '..')
const relayBinary = process.env.TERNILO_E2E_SERVER_BINARY ?? path.join(repository, 'target', 'debug', 'ternilo-server')
const nodeBinary = process.env.TERNILO_E2E_NODE_BINARY ?? path.join(repository, 'target', 'debug', 'ternilo')
const owner = { username: 'relay-browser-owner', password: 'relay-browser-password' }
const relayImage = (process.env.TERNILO_SERVER_BROWSER_IMAGE ?? process.env.TERNILO_RELAY_BROWSER_IMAGE)?.trim()
const execFileAsync = promisify(execFile)

function sse(payload) {
  return `data: ${JSON.stringify(payload)}\n\n`
}

function completed(text, usage = {}) {
  return sse({
    type: 'response.completed',
    response: {
      status: 'completed',
      output: [{
        type: 'message',
        role: 'assistant',
        content: [{ type: 'output_text', text }],
      }],
      usage: {
        input_tokens: 18,
        output_tokens: 7,
        input_tokens_details: { cached_tokens: 3 },
        output_tokens_details: { reasoning_tokens: 1 },
        ...usage,
      },
    },
  })
}

async function startStopModelFixture() {
  const requests = []
  const requestWaiters = new Map()
  const timers = new Set()
  let slowSettled = false
  let resolveSlowCancelled
  const slowCancelled = new Promise(resolve => { resolveSlowCancelled = resolve })
  const server = createServer((incoming, response) => {
    if (incoming.method !== 'POST' || incoming.url !== '/v1/responses') {
      response.writeHead(404, { 'content-type': 'application/json' })
      response.end('{"error":"not found"}')
      return
    }
    const body = []
    incoming.on('data', chunk => body.push(chunk))
    incoming.on('end', () => {
      const parsed = JSON.parse(Buffer.concat(body).toString('utf8'))
      Object.defineProperty(parsed, 'authorization', { value: incoming.headers.authorization })
      if (typeof parsed.instructions === 'string' && parsed.instructions.includes('You name software-agent conversations')) {
        const title = '中继停止链验收'
        response.writeHead(200, { 'content-type': 'text/event-stream', 'cache-control': 'no-cache' })
        response.end([
          sse({ type: 'response.output_text.delta', output_index: 0, content_index: 0, delta: title }),
          completed(title, { input_tokens: 6, output_tokens: 2 }),
        ].join(''))
        return
      }

      const index = requests.push(parsed) - 1
      requestWaiters.get(index)?.(parsed)
      requestWaiters.delete(index)
      response.writeHead(200, {
        'content-type': 'text/event-stream',
        'cache-control': 'no-cache',
        'x-request-id': `relay-stop-${index + 1}`,
      })
      if (index > 0) {
        const answer = '中继停止后再次发送成功。'
        response.end([
          sse({ type: 'response.output_text.delta', output_index: 0, content_index: 0, delta: answer }),
          completed(answer),
        ].join(''))
        return
      }

      let chunk = 0
      const timer = setInterval(() => {
        if (response.destroyed) return
        chunk += 1
        response.write(sse({
          type: 'response.output_text.delta',
          output_index: 0,
          content_index: 0,
          delta: `远程慢流 ${chunk}\n`,
        }))
        if (chunk < 300) return
        slowSettled = true
        clearInterval(timer)
        timers.delete(timer)
        response.end(completed('不应等待自然完成。'))
      }, 100)
      timers.add(timer)
      response.once('close', () => {
        clearInterval(timer)
        timers.delete(timer)
        if (!slowSettled) resolveSlowCancelled({ chunks: chunk })
      })
    })
  })
  await new Promise((resolve, reject) => {
    server.once('error', reject)
    server.listen(0, '127.0.0.1', resolve)
  })
  const address = server.address()
  return {
    baseUrl: `http://127.0.0.1:${address.port}/v1`,
    request(index) {
      if (requests[index]) return Promise.resolve(requests[index])
      return new Promise(resolve => requestWaiters.set(index, resolve))
    },
    waitForCancellation: () => slowCancelled,
    close: () => new Promise((resolve, reject) => {
      for (const timer of timers) clearInterval(timer)
      timers.clear()
      server.close(error => error ? reject(error) : resolve())
    }),
  }
}

function managedProcess(binary, arguments_, environment, readyPattern) {
  const child = spawn(binary, arguments_, {
    cwd: repository,
    env: { ...process.env, ...environment },
    stdio: ['ignore', 'pipe', 'pipe'],
  })
  let stdout = ''
  let stderr = ''
  let settled = false
  child.stdout.setEncoding('utf8')
  child.stderr.setEncoding('utf8')
  child.stderr.on('data', chunk => { stderr += chunk })
  const ready = new Promise((resolve, reject) => {
    child.stdout.on('data', chunk => {
      stdout += chunk
      const match = stdout.match(readyPattern)
      if (match && !settled) {
        settled = true
        resolve(match[1])
      }
    })
    child.once('error', error => {
      if (!settled) {
        settled = true
        reject(error)
      }
    })
    child.once('exit', code => {
      if (!settled) {
        settled = true
        reject(new Error(`${path.basename(binary)} exited with ${code}: ${stderr || stdout}`))
      }
    })
  })
  return { child, ready, diagnostics: () => ({ stdout, stderr }) }
}

async function stopProcess(process_) {
  const { child } = process_
  if (child.exitCode !== null) return
  child.kill('SIGINT')
  await Promise.race([
    new Promise(resolve => child.once('exit', resolve)),
    new Promise(resolve => setTimeout(resolve, 5000)).then(() => child.kill('SIGKILL')),
  ])
}

async function startContainerRelay(temporary) {
  const container = `ternilo-server-browser-${path.basename(temporary)}`
  const volume = `${container}-data`
  const port = await freePort()
  const origin = `http://127.0.0.1:${port}`
  const isolation = [
    '--user', '0:0', '--read-only', '--tmpfs', '/tmp:size=64m,mode=1777',
    '--cap-drop', 'ALL', '--cap-add', 'CHOWN', '--cap-add', 'DAC_READ_SEARCH',
    '--cap-add', 'KILL', '--cap-add', 'SETUID', '--cap-add', 'SETGID', '--cap-add', 'SETPCAP',
    '--security-opt', 'no-new-privileges:true',
    '--mount', `type=volume,src=${volume},dst=/var/lib/ternilo`,
  ]
  try {
    await execFileAsync('docker', ['volume', 'create', volume])
    await execFileAsync('docker', [
      'run', '--rm', ...isolation,
      '-e', `TERNILO_SERVER_OWNER_USERNAME=${owner.username}`,
      '-e', `TERNILO_SERVER_OWNER_EMAIL=${owner.username}@example.test`,
      '-e', `TERNILO_SERVER_OWNER_PASSWORD=${owner.password}`,
      relayImage, 'server', 'init', '--non-interactive',
      '--config', '/var/lib/ternilo/server.json', '--listen', '0.0.0.0:4321', '--public-url', origin,
    ])
    await execFileAsync('docker', [
      'run', '-d', '--name', container, ...isolation, '-p', `127.0.0.1:${port}:4321`,
      relayImage, 'server', 'serve', '--config', '/var/lib/ternilo/server.json',
    ])
    const deadline = Date.now() + 30_000
    let becameReady = false
    while (Date.now() < deadline) {
      try {
        const response = await fetch(`${origin}/readyz`)
        if (response.ok && (await response.json()).status === 'ready') {
          becameReady = true
          break
        }
      } catch {}
      await new Promise(resolve => setTimeout(resolve, 100))
    }
    if (!becameReady) {
      throw new Error(`production Server container did not become ready: ${execFileSync('docker', ['logs', container], { encoding: 'utf8' })}`)
    }
    return {
      container, volume, origin,
      diagnostics: () => ({ image: relayImage, container, logs: execFileSync('docker', ['logs', container], { encoding: 'utf8' }) }),
    }
  } catch (error) {
    await execFileAsync('docker', ['rm', '-f', container]).catch(() => {})
    await execFileAsync('docker', ['volume', 'rm', volume]).catch(() => {})
    throw error
  }
}

async function stopContainerRelay(relay) {
  await execFileAsync('docker', ['rm', '-f', relay.container]).catch(() => {})
  await execFileAsync('docker', ['volume', 'rm', relay.volume]).catch(() => {})
}

async function waitForConnectedNode(origin, relay, node, token, tenantId) {
  const deadline = Date.now() + 20_000
  while (Date.now() < deadline) {
    const payload = await serverRequest(origin, '/execution-targets', { token, tenantId })
    if (payload.executors?.some(executor => executor.executor_id === 'home' && executor.connected)) return
    await new Promise(resolve => setTimeout(resolve, 100))
  }
  throw new Error(`node did not enroll through Server: ${JSON.stringify({
    relay: relay.diagnostics(), node: node.diagnostics(),
  })}`)
}

async function waitForFile(file, expected) {
  const deadline = Date.now() + 30_000
  while (Date.now() < deadline) {
    try {
      const value = await readFile(file, 'utf8')
      if (value === expected) return
    } catch {}
    await new Promise(resolve => setTimeout(resolve, 100))
  }
  throw new Error(`file did not reach expected contents: ${file}`)
}

async function chooseWorkspace(page, workspace) {
  await page.getByRole('button', { name: '选择工作文件夹' }).first().click()
  const placement = page.getByRole('dialog', { name: '打开工作区' })
  await placement.waitFor()
  const local = placement.getByRole('button', { name: /我的电脑或 VPS/ })
  if (await local.count() && await local.getAttribute('aria-pressed') !== 'true') await local.click()
  await selectChoice(placement.getByLabel('运行电脑'), 'home')
  await placement.getByLabel('工作区名称').fill('Relay Workspace')
  await placement.getByRole('button', { name: '选择文件夹' }).click()
  const dialog = page.getByRole('dialog', { name: '选择 home 上的工作文件夹' })
  await dialog.getByRole('button', { name: '编辑文件夹路径' }).click()
  const pathEditor = dialog.getByRole('textbox', { name: '编辑文件夹路径' })
  await pathEditor.fill(workspace)
  await pathEditor.press('Enter')
  await dialog.getByRole('list', { name: `目录 ${workspace}`, exact: true }).waitFor()
  await dialog.getByRole('button', { name: '选择此文件夹' }).click()
  await dialog.waitFor({ state: 'detached' })
  await placement.getByRole('button', { name: '打开并开始会话', exact: true }).click()
  await placement.waitFor({ state: 'detached' })
  await page.getByRole('textbox', { name: '输入任务' }).waitFor()
}

async function signIn(page, login = page.getByRole('dialog', { name: '登录 Ternilo' })) {
  await login.getByLabel('用户名', { exact: true }).fill(owner.username)
  await login.getByLabel('密码', { exact: true }).fill(owner.password)
  await login.getByRole('button', { name: '登录', exact: true }).click()
  await login.waitFor({ state: 'detached' })
  await page.locator('[data-sidebar-connection][data-live-state="ready"]').waitFor({ timeout: 30_000 })
}

async function connectRemotePage(page, origin) {
  const token = await page.evaluate(() => JSON.parse(sessionStorage.getItem('ternilo.native.session')).access_token)
  assert.ok(token)
  await page.goto(origin, { waitUntil: 'domcontentloaded' })
  await page.locator('[data-sidebar-connection][data-live-state="ready"]').waitFor({ timeout: 30_000 })
  await page.getByRole('dialog', { name: '登录 Ternilo' }).waitFor({ state: 'hidden' })
  assert.equal(await page.evaluate(() => JSON.parse(sessionStorage.getItem('ternilo.native.session')).access_token), token)
}

async function signOutFromMobileSidebar(page) {
  await page.getByRole('button', { name: '打开侧边栏' }).click()
  const sidebar = page.getByRole('complementary', { name: '会话侧边栏' })
  await sidebar.getByRole('button', { name: '退出登录', exact: true }).click()
  const login = page.getByRole('dialog', { name: '登录 Ternilo' })
  await login.waitFor()
  return login
}

function observeLiveTransport(page, pageLabel = 'page') {
  const sockets = []
  const legacyRequests = []
  const milestones = []
  let documentEpoch = 0
  page.on('framenavigated', frame => {
    if (frame !== page.mainFrame()) return
    const navigatedAt = Date.now()
    for (const socket of sockets) {
      if (socket.documentEpoch === documentEpoch && socket.closedAt === null) {
        socket.closedAt = navigatedAt
        socket.closedBy = 'navigation'
      }
    }
    documentEpoch += 1
  })
  page.on('request', request => {
    const url = new URL(request.url())
    if (/\/api\/v1\/sessions\/[^/]+\/(?:event-delta|queue|stats|projection|questions|plugins)$/.test(url.pathname)) {
      legacyRequests.push({ method: request.method(), url: request.url(), at: Date.now() })
    }
  })
  page.on('websocket', socket => {
    if (new URL(socket.url()).pathname !== '/api/v1/live') return
    const record = {
      pageLabel,
      pageUrl: page.url(),
      documentEpoch,
      url: socket.url(),
      openedAt: Date.now(),
      closedAt: null,
      closedBy: null,
      sent: [],
      received: [],
    }
    Object.defineProperty(record, 'socket', { value: socket })
    sockets.push(record)
    const capture = target => event => {
      if (typeof event.payload !== 'string') return
      try { target.push({ frame: JSON.parse(event.payload), at: Date.now() }) } catch {}
    }
    socket.on('framesent', capture(record.sent))
    socket.on('framereceived', capture(record.received))
    socket.on('close', () => {
      if (record.closedAt === null) {
        record.closedAt = Date.now()
        record.closedBy = 'socket'
      }
    })
  })
  return {
    sockets,
    legacyRequests,
    milestones,
    mark(label) {
      milestones.push({ label, at: Date.now(), pageUrl: page.url(), documentEpoch })
    },
    documentEpoch() {
      return documentEpoch
    },
  }
}

function liveSocketTimeline(transport) {
  return {
    milestones: transport.milestones,
    sockets: transport.sockets.map((socket, index) => ({
      index,
      pageLabel: socket.pageLabel,
      pageUrl: socket.pageUrl,
      documentEpoch: socket.documentEpoch,
      url: socket.url,
      openedAt: socket.openedAt,
      closedAt: socket.closedAt,
      closedBy: socket.closedBy,
      isClosed: socket.socket.isClosed(),
      helloAt: socket.sent.find(item => item.frame.type === 'hello')?.at ?? null,
      readyAt: socket.received.find(item => item.frame.type === 'ready')?.at ?? null,
      lastReceivedAt: socket.received.at(-1)?.at ?? null,
      lastReceivedType: socket.received.at(-1)?.frame.type ?? null,
      receivedAfterClose: socket.closedAt === null
        ? []
        : socket.received.filter(item => item.at > socket.closedAt).map(item => ({ type: item.frame.type, at: item.at })),
    })),
  }
}

async function assertSingleReadyLiveSocket(page, transport) {
  await page.locator('[data-sidebar-connection][data-live-state="ready"]').waitFor({ timeout: 30_000 })
  await page.waitForFunction(() => document.querySelector('[data-sidebar-connection]')?.getAttribute('data-live-state') === 'ready')
  const deadline = Date.now() + 5_000
  const currentEpoch = transport.documentEpoch()
  const isActive = socket => socket.documentEpoch === currentEpoch
    && socket.closedAt === null
    && !socket.socket.isClosed()
  while (transport.sockets.filter(isActive).length !== 1 && Date.now() < deadline) {
    await new Promise(resolve => setTimeout(resolve, 25))
  }
  const active = transport.sockets.filter(isActive)
  assert.equal(active.length, 1, `expected one active /api/v1/live socket: ${JSON.stringify(liveSocketTimeline(transport))}`)
  assert.equal(new URL(active[0].url).pathname, '/api/v1/live')
  assert.ok(active[0].received.some(item => item.frame.type === 'ready'))
  assert.ok(active[0].received.some(item => item.frame.type === 'workbench'))
}

async function localApi(origin, pathValue, { method = 'GET', body, token = '', tenantId } = {}) {
  const response = await fetch(`${origin}/api/v1${pathValue}`, {
    method,
    headers: {
      ...(token ? { authorization: `Bearer ${token}` } : {}),
      ...(tenantId ? { 'x-ternilo-tenant': tenantId } : {}),
      ...(body === undefined ? {} : { 'content-type': 'application/json' }),
    },
    body: body === undefined ? undefined : JSON.stringify(body),
  })
  if (!response.ok) throw new Error(`${method} ${pathValue}: ${response.status} ${await response.text()}`)
  return response.status === 204 ? null : response.json()
}

async function configureStopProviderThroughRemoteUi(page, baseUrl) {
  const tenantId = await page.evaluate(() => localStorage.getItem('ternilo.current-tenant'))
  await modelSettings(page)
  const settings = await computerModels(page, tenantId)
  await settings.getByRole('button', { name: '添加 Provider' }).first().click()
  const editor = settings.locator('[data-provider-editor="new"]')
  await editor.getByLabel('Provider ID').fill('relay-stop')
  await editor.getByLabel('显示名称', { exact: true }).fill('Relay Stop Fixture')
  await editor.getByLabel('API Key').fill('relay-stop-fixture-key')
  await editor.getByLabel('API 地址').fill(baseUrl)
  await editor.locator('[id$="-provider-defaults-context"]').fill('128K')
  await editor.locator('[id$="-provider-defaults-output"]').fill('8K')
  await editor.getByLabel('模型 ID 1').fill('relay-stop-model')
  await editor.getByLabel('显示名称（可选） 1').fill('Relay Stop Model')
  await editor.getByRole('button', { name: '添加 Provider', exact: true }).click()
  await settings.getByText('Relay Stop Fixture', { exact: true }).waitFor()
  assert.equal((await settings.textContent()).includes('relay-stop-fixture-key'), false)
  await closeSettings(page)
  await settings.waitFor({ state: 'detached' })
  await choose(page, 'node', 'relay-stop', 'Relay Stop Model')
  await page.getByRole('button', { name: /Relay Stop Model/ }).waitFor({ timeout: 30_000 })
}

async function verifyRemoteSettingsAuthoring(page) {
  await page.getByRole('button', { name: '用户设置', exact: true }).click()
  const settings = page.locator('[data-user-settings]')
  await settings.waitFor()

  const authorizationSnapshot = page.waitForResponse(response => (
    response.request().method() === 'GET'
    && new URL(response.url()).pathname === '/api/v1/authorizations'
  ))
  await settings.getByRole('button', { name: '凭据与登录', exact: true }).click()
  assert.equal((await authorizationSnapshot).status(), 200)
  await settings.getByText('当前插件都不需要交互登录。', { exact: true }).waitFor()
  assert.equal((await settings.textContent()).includes('unknown field `session_id`'), false)

  await settings.getByPlaceholder('MY_SERVICE_TOKEN').fill('RELAY_BROWSER_TOKEN')
  await settings.locator('#credential-value').fill('relay-browser-secret')
  const credentialCreated = page.waitForResponse(response => (
    response.request().method() === 'POST'
    && new URL(response.url()).pathname === '/api/v1/credentials'
  ))
  await settings.getByRole('button', { name: '保存', exact: true }).click()
  assert.equal((await credentialCreated).status(), 204)
  await settings.getByText('RELAY_BROWSER_TOKEN', { exact: true }).waitFor()
  await settings.getByRole('button', { name: '删除凭据: RELAY_BROWSER_TOKEN' }).click()
  const credentialDeleted = page.waitForResponse(response => (
    response.request().method() === 'DELETE'
    && new URL(response.url()).pathname === '/api/v1/credentials/RELAY_BROWSER_TOKEN'
  ))
  await page.getByRole('dialog', { name: '删除此凭据？' }).getByRole('button', { name: '删除', exact: true }).click()
  assert.equal((await credentialDeleted).status(), 204)
  await settings.getByText('RELAY_BROWSER_TOKEN', { exact: true }).waitFor({ state: 'detached' })

  await settings.getByRole('button', { name: 'Agent 预设', exact: true }).click()
  await settings.getByRole('button', { name: '复制预设: 标准模式' }).click()
  const copyDialog = page.getByRole('dialog', { name: /复制预设/ })
  await copyDialog.locator('#preset-copy-id').fill('relay-browser-preset')
  await copyDialog.locator('#preset-copy-name').fill('Relay Browser Preset')
  const presetCopied = page.waitForResponse(response => (
    response.request().method() === 'POST'
    && new URL(response.url()).pathname === '/api/v1/agent-presets'
  ))
  await copyDialog.getByRole('button', { name: '创建预设' }).click()
  assert.equal((await presetCopied).status(), 201)
  const presetCard = settings.locator('article').filter({ hasText: 'relay-browser-preset' })
  await presetCard.waitFor()

  const presetLoaded = page.waitForResponse(response => (
    response.request().method() === 'GET'
    && new URL(response.url()).pathname === '/api/v1/agent-presets/relay-browser-preset'
  ))
  await presetCard.getByRole('button', { name: '查看: Relay Browser Preset' }).click()
  assert.equal((await presetLoaded).status(), 200)
  const viewDialog = page.getByRole('dialog', { name: '查看 Relay Browser Preset' })
  await viewDialog.getByRole('button', { name: '关闭', exact: true }).first().click()
  await viewDialog.waitFor({ state: 'detached' })

  await presetCard.getByRole('button', { name: '编辑: Relay Browser Preset' }).click()
  const editDialog = page.getByRole('dialog', { name: '编辑 Relay Browser Preset' })
  await editDialog.locator('#preset-edit-description').fill('Relay route parity verified')
  const presetUpdated = page.waitForResponse(response => (
    response.request().method() === 'PUT'
    && new URL(response.url()).pathname === '/api/v1/agent-presets/relay-browser-preset'
  ))
  await editDialog.getByRole('button', { name: '保存预设' }).click()
  assert.equal((await presetUpdated).status(), 200)
  await presetCard.getByText('Relay route parity verified', { exact: true }).waitFor()

  const presetDefaulted = page.waitForResponse(response => (
    response.request().method() === 'PUT'
    && new URL(response.url()).pathname === '/api/v1/agent-presets/relay-browser-preset/default'
  ))
  await presetCard.getByRole('button', { name: '设为默认: Relay Browser Preset' }).click()
  assert.equal((await presetDefaulted).status(), 200)
  await presetCard.getByRole('button', { name: '默认: Relay Browser Preset' }).waitFor()
  const standardSetDefault = settings.getByRole('button', { name: '设为默认: 标准模式', exact: true })
  await standardSetDefault.click()
  await settings.getByRole('button', { name: '默认: 标准模式', exact: true }).waitFor()

  await presetCard.getByRole('button', { name: '使用: Relay Browser Preset' }).click()
  await presetCard.getByText('当前使用', { exact: true }).waitFor()
  await settings.getByRole('button', { name: '使用: 标准模式', exact: true }).click()
  await presetCard.getByRole('button', { name: '使用: Relay Browser Preset' }).waitFor()

  await presetCard.getByRole('button', { name: '删除: Relay Browser Preset' }).click()
  const presetDeleted = page.waitForResponse(response => (
    response.request().method() === 'DELETE'
    && new URL(response.url()).pathname === '/api/v1/agent-presets/relay-browser-preset'
  ))
  await page.getByRole('dialog', { name: '删除该预设？' }).getByRole('button', { name: '删除', exact: true }).click()
  assert.equal((await presetDeleted).status(), 204)
  await presetCard.waitFor({ state: 'detached' })

  await settings.getByRole('button', { name: '返回工作台' }).click()
  await settings.waitFor({ state: 'detached' })
}

async function waitForActiveRun(origin, sessionId, token) {
  const deadline = Date.now() + 30_000
  while (Date.now() < deadline) {
    const inbox = await localApi(origin, `/sessions/${encodeURIComponent(sessionId)}/queue`, { token })
    if (inbox.active_run_id) return inbox.active_run_id
    await new Promise(resolve => setTimeout(resolve, 100))
  }
  throw new Error(`session ${sessionId} did not expose an active run`)
}

async function waitForTerminalEvent(origin, sessionId, runId, token) {
  const deadline = Date.now() + 30_000
  while (Date.now() < deadline) {
    const events = await localApi(origin, `/sessions/${encodeURIComponent(sessionId)}/events`, { token })
    const terminal = events.find(event => event.run_id === runId && event.type === 'turn_cancelled')
    if (terminal) return terminal
    await new Promise(resolve => setTimeout(resolve, 100))
  }
  throw new Error(`run ${runId} did not persist turn_cancelled`)
}

async function waitForJsonlEvent(file, predicate) {
  const deadline = Date.now() + 30_000
  while (Date.now() < deadline) {
    try {
      const records = (await readFile(file, 'utf8')).trim().split('\n').filter(Boolean).map(line => JSON.parse(line))
      const event = records.find(predicate)
      if (event) return event
    } catch {}
    await new Promise(resolve => setTimeout(resolve, 100))
  }
  throw new Error(`event was not durably appended to ${file}`)
}

async function withTimeout(promise, timeoutMs, message) {
  let timer
  try {
    return await Promise.race([
      promise,
      new Promise((_, reject) => { timer = setTimeout(() => reject(new Error(message)), timeoutMs) }),
    ])
  } finally {
    clearTimeout(timer)
  }
}

test('unified Server browser drives its enrolled Node and local web observes the same application', { timeout: 240_000 }, async () => {
  const temporary = await mkdtemp(path.join(tmpdir(), 'ternilo-relay-browser-e2e-'))
  const nodeData = path.join(temporary, 'node-data')
  const workspace = path.join(temporary, 'workspace')
  await mkdir(nodeData)
  await mkdir(workspace)
  const model = await startStopModelFixture()

  let relay
  let node
  let browser
  try {
    relay = relayImage
      ? await startContainerRelay(temporary)
      : await initializeServer({ directory: path.join(temporary, 'server'), origin: `http://127.0.0.1:${await freePort()}`, binary: relayBinary, owner, mode: 'single_user' })
    const relayOrigin = relay.origin
    browser = await chromium.launch({
      headless: true,
      executablePath: process.env.TERNILO_BROWSER_EXECUTABLE?.trim() || undefined,
    })
    const context = await browser.newContext({ viewport: { width: 1280, height: 800 } })
    const remotePage = await context.newPage()
    const remoteTransport = observeLiveTransport(remotePage, 'remote')
    const remoteErrors = []
    const remoteApiNotFound = []
    remotePage.on('pageerror', error => remoteErrors.push(error.message))
    remotePage.on('response', response => {
      const url = new URL(response.url())
      if (response.status() === 404 && url.origin === relayOrigin && url.pathname.startsWith('/api/v1/')) {
        remoteApiNotFound.push({ method: response.request().method(), path: url.pathname })
      }
    })
    remoteTransport.mark('initial navigation')
    await remotePage.goto(relayOrigin, { waitUntil: 'domcontentloaded' })
    const login = remotePage.getByRole('dialog', { name: '登录 Ternilo' })
    await login.waitFor()
    await remotePage.waitForTimeout(100)
    assert.equal(await remotePage.getByRole('region', { name: '通知' }).getByText(/HTTP 401/).count(), 0)
    await signIn(remotePage, login)
    const { adminToken, tenantId } = await remotePage.evaluate(() => ({
      adminToken: JSON.parse(sessionStorage.getItem('ternilo.native.session')).access_token,
      tenantId: localStorage.getItem('ternilo.current-tenant'),
    }))
    assert.ok(adminToken)
    assert.ok(tenantId)
    assert.equal(await remotePage.evaluate(() => localStorage.getItem('ternilo.native.session')), null)
    await assertSingleReadyLiveSocket(remotePage, remoteTransport)

    await remotePage.getByRole('button', { name: '用户设置', exact: true }).click()
    const settings = remotePage.locator('[data-user-settings]')
    await settings.getByRole('button', { name: '我的机器', exact: true }).click()
    await settings.getByLabel('电脑 ID', { exact: true }).fill('home')
    await settings.getByRole('button', { name: '生成启动命令', exact: true }).click()
    const launch = remotePage.getByRole('dialog', { name: '启动 Ternilo Node' })
    const command = await launch.locator('[data-node-launch-command]').textContent()
    const nodeToken = /--token "([^"\s]+)"/.exec(command)?.[1]
    assert.ok(nodeToken)
    await launch.getByRole('button', { name: '我已保存，关闭', exact: true }).click()
    await settings.getByRole('button', { name: '返回工作台', exact: true }).click()
    await settings.waitFor({ state: 'detached' })
    node = managedProcess(nodeBinary, ['serve',
      '--gateway-url', `${relayOrigin.replace('http://', 'ws://')}/api/v1/executors/connect`,
      '--allow-insecure-gateway', '--node-id', 'home', '--data-dir', nodeData, '--listen', '127.0.0.1:0',
    ], { TERNILO_LOCAL_TOKEN: nodeToken }, /Ternilo local web: (http:\/\/[^\s]+)/)
    const localOrigin = await node.ready
    await waitForConnectedNode(relayOrigin, relay, node, adminToken, tenantId)
    await assertSingleReadyLiveSocket(remotePage, remoteTransport)

    await chooseWorkspace(remotePage, workspace)
    await verifyRemoteSettingsAuthoring(remotePage)
    const prompt = remotePage.getByRole('textbox', { name: '输入任务' })
    await prompt.fill('/write')
    await remotePage.getByRole('option').filter({ hasText: '/write' }).waitFor()
    await prompt.fill('/write relay-proof.txt relay-chain-ready')
    await remotePage.getByRole('button', { name: '发送' }).click()
    assert.equal(await prompt.inputValue(), '')
    await remotePage.getByRole('button', { name: '发送' }).waitFor({ timeout: 30_000 })
    const remoteWrite = remotePage.locator('[data-tool-call-id]').filter({ hasText: '写入文件' }).last()
    await remoteWrite.waitFor({ timeout: 30_000 })
    await remoteWrite.getByRole('button', { name: '展开 写入文件 结果' }).click()
    await remoteWrite.locator('[data-tool-view="diff"]').waitFor()
    assert.match(await remoteWrite.locator('[data-tool-view="diff"]').textContent(), /relay-proof\.txt/)
    assert.match(await remoteWrite.locator('[data-tool-view="diff"]').textContent(), /relay-chain-ready/)
    await waitForFile(path.join(workspace, 'relay-proof.txt'), 'relay-chain-ready')
    const producedFiles = remotePage.locator('[data-produced-files]').last()
    await producedFiles.waitFor()
    await producedFiles.getByRole('button', { name: '预览生成文件 relay-proof.txt' }).click()
    const preview = remotePage.getByRole('dialog').filter({ hasText: 'relay-proof.txt' })
    await preview.waitFor()
    assert.equal(await preview.locator('[data-produced-file-text]').textContent(), 'relay-chain-ready')
    await preview.getByRole('button', { name: '关闭', exact: true }).click()
    await preview.waitFor({ state: 'detached' })

    await prompt.fill('/code let value = tools::read_file(#{ path: "relay-proof.txt" }); print(value); #{ nested: true }')
    await remotePage.getByRole('button', { name: '发送' }).click()
    await remotePage.getByRole('button', { name: '发送' }).waitFor({ timeout: 30_000 })
    const codeTurn = remotePage.locator('[data-chat-turn]').last()
    const codeTree = codeTurn.locator('[data-tool-call-id]').filter({ hasText: '运行代码' }).last()
    await codeTree.waitFor()
    const nestedRead = codeTree.locator('[data-subcalls] [data-tool-call-id]').filter({ hasText: '读取文件' }).last()
    await nestedRead.waitFor()
    assert.equal(await codeTree.locator(':scope > [data-subcalls]').count(), 1)
    await nestedRead.getByRole('button', { name: '展开 读取文件 结果' }).click()
    assert.match(await nestedRead.locator('[data-tool-view="read"]').textContent(), /relay-chain-ready/)
    const remoteSessionId = await remotePage.evaluate(() => localStorage.getItem('ternilo.current-session'))
    assert.ok(remoteSessionId)

    await context.setOffline(true)
    remoteTransport.mark('browser offline')
    const offlineRunId = `relay-offline-${Date.now()}`
    await localApi(relayOrigin, `/sessions/${encodeURIComponent(remoteSessionId)}/queue`, {
      method: 'POST',
      token: adminToken,
      tenantId,
      body: {
        delivery: 'queue',
        run_id: offlineRunId,
        content: { kind: 'prompt', input: '/write relay-offline.txt relay-offline-backfill' },
        references: [],
        attachments: [],
      },
    })
    await waitForFile(path.join(workspace, 'relay-offline.txt'), 'relay-offline-backfill')
    await context.setOffline(false)
    remoteTransport.mark('browser online')
    const offlineMessage = remotePage.locator('article[data-role="user"]').filter({ hasText: '/write relay-offline.txt relay-offline-backfill' })
    await offlineMessage.waitFor({ timeout: 30_000 })
    assert.equal(await offlineMessage.count(), 1)
    const offlineWrite = remotePage.locator(`[data-chat-run-id="${offlineRunId}"] [data-tool-call-id]`).filter({ hasText: '写入文件' })
    await offlineWrite.waitFor({ timeout: 30_000 })
    remoteTransport.mark('first reconnect login')
    await connectRemotePage(remotePage, relayOrigin)
    await remotePage.locator(`[data-sidebar-session-row][data-session-id="${remoteSessionId}"]`).waitFor({ timeout: 30_000 })
    await offlineMessage.waitFor({ timeout: 30_000 })
    await offlineWrite.waitFor({ timeout: 30_000 })
    assert.equal(await remotePage.locator('article[data-role="user"]').filter({ hasText: '/write relay-offline.txt relay-offline-backfill' }).count(), 1)
    assert.equal(await remotePage.locator(`[data-chat-run-id="${offlineRunId}"] [data-tool-call-id]`).filter({ hasText: '写入文件' }).count(), 1)

    const remoteClientState = await remotePage.evaluate(() => ({
      html: document.documentElement.outerHTML,
      local: Object.fromEntries(Array.from({ length: localStorage.length }, (_, index) => localStorage.key(index)).filter(Boolean).map(key => [key, localStorage.getItem(key)])),
      session: Object.fromEntries(Array.from({ length: sessionStorage.length }, (_, index) => sessionStorage.key(index)).filter(Boolean).map(key => [key, sessionStorage.getItem(key)])),
    }))
    assert.equal(remoteClientState.html.includes(adminToken), false)
    assert.equal(JSON.stringify(remoteClientState.local).includes(adminToken), false)
    assert.equal(JSON.parse(remoteClientState.session['ternilo.native.session']).access_token, adminToken)
    assert.equal(JSON.stringify(remoteClientState).includes(nodeToken), false)

    const localPage = await context.newPage()
    const localTransport = observeLiveTransport(localPage, 'local')
    const localErrors = []
    localPage.on('pageerror', error => localErrors.push(error.message))
    await localPage.goto(localOrigin, { waitUntil: 'domcontentloaded' })
    await localPage.locator('[data-sidebar-connection]').filter({ hasText: '此电脑内核已连接' }).waitFor()
    await assertSingleReadyLiveSocket(localPage, localTransport)
    const localApiToken = await localPage.evaluate(() => window.__TERNILO_BOOT__?.apiToken)
    assert.ok(localApiToken)
    await localPage.locator('[data-sidebar-session-row]').first().waitFor()
    const localWrite = localPage.locator('[data-tool-call-id]').filter({ hasText: '写入文件' }).last()
    await localWrite.waitFor({ timeout: 30_000 })
    await localWrite.getByRole('button', { name: '展开 写入文件 结果' }).click()
    await localWrite.locator('[data-tool-view="diff"]').waitFor()
    const localProducedFiles = localPage.locator('[data-produced-files]').filter({ hasText: 'relay-proof.txt' })
    await localProducedFiles.waitFor()
    assert.equal(await localProducedFiles.getByRole('button', { name: '预览生成文件 relay-proof.txt' }).count(), 1)
    const localSessionId = await localPage.evaluate(() => localStorage.getItem('ternilo.current-session'))
    assert.ok(localSessionId)
    assert.notEqual(localSessionId, remoteSessionId, 'Server public IDs must map to the original Node session')
    assert.equal(await localPage.locator('[data-session-workspace]').textContent(), workspace)
    assert.equal(await localPage.locator('article[data-role="user"]').filter({ hasText: '/write relay-proof.txt relay-chain-ready' }).count(), 1)

    await localApi(localOrigin, `/sessions/${encodeURIComponent(localSessionId)}`, {
      method: 'PATCH',
      token: localApiToken,
      body: { title: 'Local live mutation' },
    })
    await remotePage.locator('[data-session-title]').filter({ hasText: 'Local live mutation' }).waitFor({ timeout: 30_000 })
    await localApi(relayOrigin, `/sessions/${encodeURIComponent(remoteSessionId)}`, {
      method: 'PATCH',
      token: adminToken,
      tenantId,
      body: { title: 'Relay live mutation' },
    })
    await localPage.locator('[data-session-title]').filter({ hasText: 'Relay live mutation' }).waitFor({ timeout: 30_000 })

    const remoteOriginalRow = remotePage.locator('[data-sidebar-session-row]').filter({
      has: remotePage.locator('[data-sidebar-session-active]'),
    })
    await remoteOriginalRow.hover()
    await remoteOriginalRow.getByRole('button', { name: /的操作$/ }).click()
    await remotePage.getByRole('menuitem', { name: '分叉会话' }).click()
    await remotePage.waitForFunction(() => document.querySelectorAll('[data-sidebar-session-row]').length === 2)
    await remotePage.waitForFunction(originalId => {
      const selected = localStorage.getItem('ternilo.current-session')
      return selected && selected !== originalId
    }, remoteSessionId)
    const forkedSessionId = await remotePage.evaluate(() => localStorage.getItem('ternilo.current-session'))
    assert.ok(forkedSessionId)
    assert.notEqual(forkedSessionId, remoteSessionId)
    await remotePage.locator('[data-session-title]').filter({ hasText: /\(1\)$/ }).waitFor()
    assert.match(await remotePage.locator('[data-session-title]').textContent(), /\(1\)$/)

    await localPage.reload({ waitUntil: 'domcontentloaded' })
    await localPage.waitForFunction(() => document.querySelectorAll('[data-sidebar-session-row]').length === 2)
    assert.equal(await localPage.locator('[data-sidebar-session-row]').filter({ hasText: '(1)' }).count(), 1)

    const remoteForkRow = remotePage.locator('[data-sidebar-session-row]').filter({
      has: remotePage.locator('[data-sidebar-session-active]'),
    })
    await remoteForkRow.hover()
    await remoteForkRow.getByRole('button', { name: /的操作$/ }).click()
    await remotePage.getByRole('menuitem', { name: '归档会话' }).click()
    await remotePage.waitForFunction(() => document.querySelectorAll('[data-sidebar-session-row]').length === 1)
    await localPage.reload({ waitUntil: 'domcontentloaded' })
    await localPage.waitForFunction(() => document.querySelectorAll('[data-sidebar-session-row]').length === 1)
    assert.equal(await localPage.locator('[data-sidebar-session-row]').filter({ hasText: '(1)' }).count(), 0)
    assert.match(await localPage.locator('[data-sidebar-session-row]').textContent(), /Relay live mutation/)

    await configureStopProviderThroughRemoteUi(remotePage, model.baseUrl)
    const savedProviders = await localApi(localOrigin, '/providers', { token: localApiToken })
    assert.equal(savedProviders.some(provider => provider.id === 'relay-stop'), true)
    await remotePage.locator(`[data-sidebar-session-row][data-session-id="${remoteSessionId}"]`).waitFor({ timeout: 30_000 })
    await remotePage.getByRole('button', { name: /Relay Stop Model/ }).waitFor({ timeout: 30_000 })
    assert.equal(await remotePage.evaluate(() => localStorage.getItem('ternilo.current-session')), remoteSessionId)
    await prompt.fill('运行一个可停止的远程模型任务')
    await remotePage.getByRole('button', { name: '发送' }).click()
    const [slowRequest, activeRunId] = await Promise.all([
      model.request(0),
      waitForActiveRun(localOrigin, localSessionId, localApiToken),
    ])
    assert.equal(slowRequest.model, 'relay-stop-model')
    assert.equal(slowRequest.authorization, 'Bearer relay-stop-fixture-key')
    await remotePage.getByRole('status').filter({ hasText: '正在运行' }).waitFor()
    await remotePage.locator('.markdown-streaming').filter({ hasText: '远程慢流 2' }).waitFor({ timeout: 30_000 })
    const stop = remotePage.getByRole('button', { name: '停止运行' })
    await stop.waitFor()
    await stop.click()
    const cancellation = await withTimeout(
      model.waitForCancellation(),
      15_000,
      'Node did not close the slow Provider stream after Relay Stop',
    )
    assert.equal(cancellation.chunks > 0 && cancellation.chunks < 300, true)

    const terminal = await waitForTerminalEvent(localOrigin, localSessionId, activeRunId, localApiToken)
    const durableTerminal = await waitForJsonlEvent(
      path.join(nodeData, 'sessions', `${Buffer.from(localSessionId, 'utf8').toString('hex')}.jsonl`),
      event => event.run_id === activeRunId && event.type === 'turn_cancelled',
    )
    assert.equal(durableTerminal.seq, terminal.seq)
    const cancelledTurn = remotePage.locator(`[data-chat-run-id="${activeRunId}"]`)
    await cancelledTurn.filter({ hasText: '已停止' }).waitFor({ timeout: 30_000 })
    assert.equal((await cancelledTurn.textContent()).includes('不应等待自然完成。'), false)
    const recoveredSend = remotePage.getByRole('button', { name: '发送' })
    await recoveredSend.waitFor({ timeout: 30_000 })
    assert.equal(await prompt.isEnabled(), true)
    await prompt.fill('停止后继续发送')
    assert.equal(await recoveredSend.isEnabled(), true)
    await recoveredSend.click()
    const resumedRequest = await model.request(1)
    assert.equal(resumedRequest.model, 'relay-stop-model')
    await remotePage.getByText('中继停止后再次发送成功。', { exact: true }).waitFor({ timeout: 30_000 })
    await recoveredSend.waitFor({ timeout: 30_000 })
    assert.equal(await prompt.isEnabled(), true)

    remoteTransport.mark('second reconnect login')
    await connectRemotePage(remotePage, relayOrigin)
    await remotePage.locator(`[data-sidebar-session-row][data-session-id="${remoteSessionId}"]`).waitFor({ timeout: 30_000 })
    assert.equal(await remotePage.evaluate(() => localStorage.getItem('ternilo.current-session')), remoteSessionId)
    const restoredPrompt = remotePage.getByRole('textbox', { name: '输入任务' })
    await restoredPrompt.waitFor()
    await remotePage.locator(`[data-chat-run-id="${activeRunId}"]`).filter({ hasText: '已停止' }).waitFor()
    assert.equal(await remotePage.getByRole('button', { name: '停止运行' }).count(), 0)
    assert.equal(await remotePage.getByRole('status').filter({ hasText: '正在运行' }).count(), 0)
    assert.equal((await localApi(localOrigin, `/sessions/${encodeURIComponent(localSessionId)}/queue`, { token: localApiToken })).active_run_id ?? null, null)
    assert.equal(await restoredPrompt.isEnabled(), true)

    await assertSingleReadyLiveSocket(remotePage, remoteTransport)
    const settledRequestBoundary = remoteTransport.legacyRequests.length
    const settledSocketBoundary = remoteTransport.sockets.length
    await new Promise(resolve => setTimeout(resolve, 30_000))
    assert.deepEqual(
      remoteTransport.legacyRequests.slice(settledRequestBoundary),
      [],
      'settled Relay page must not issue periodic event-delta/queue/stats/projection/questions/plugins GETs',
    )
    assert.equal(remoteTransport.sockets.length, settledSocketBoundary, 'settled Relay page must keep its single live socket instead of reconnecting')
    await assertSingleReadyLiveSocket(remotePage, remoteTransport)
    const socketTimeline = liveSocketTimeline(remoteTransport)
    assert.deepEqual(
      socketTimeline.sockets.filter(socket => socket.closedBy === 'navigation' && socket.receivedAfterClose.length > 0),
      [],
      `a navigated document kept receiving live frames: ${JSON.stringify(socketTimeline)}`,
    )

    await remotePage.setViewportSize({ width: 390, height: 844 })
    assert.equal(await remotePage.evaluate(() => document.documentElement.scrollWidth), 390)
    assert.equal(await restoredPrompt.isVisible(), true)

    let signedOut = await signOutFromMobileSidebar(remotePage)
    const storedSession = () => remotePage.evaluate(() => ({
      tab: sessionStorage.getItem('ternilo.native.session'),
      persistent: localStorage.getItem('ternilo.native.session'),
    }))
    assert.deepEqual(await storedSession(), { tab: null, persistent: null })
    assert.equal(await remotePage.getByRole('region', { name: '通知' }).getByText(/HTTP 401/).count(), 0)
    await signIn(remotePage, signedOut)
    const signedInStorage = await storedSession()
    assert.ok(JSON.parse(signedInStorage.tab).access_token)
    assert.equal(signedInStorage.persistent, null)
    await remotePage.reload({ waitUntil: 'domcontentloaded' })
    await remotePage.locator('[data-sidebar-connection][data-live-state="ready"]').waitFor({ timeout: 30_000 })
    await remotePage.getByRole('dialog', { name: '登录 Ternilo' }).waitFor({ state: 'detached' })
    assert.equal(await remotePage.getByRole('dialog', { name: '登录 Ternilo' }).count(), 0)
    signedOut = await signOutFromMobileSidebar(remotePage)
    assert.deepEqual(await storedSession(), { tab: null, persistent: null })
    assert.equal(await signedOut.getByText(/HTTP 401/).count(), 0)

    assert.equal(JSON.stringify(relay.diagnostics()).includes(adminToken), false)
    assert.equal(JSON.stringify(relay.diagnostics()).includes(nodeToken), false)
    assert.equal(JSON.stringify(node.diagnostics()).includes(adminToken), false)
    assert.equal(JSON.stringify(node.diagnostics()).includes(nodeToken), false)
    assert.deepEqual(remoteErrors, [])
    assert.deepEqual(remoteApiNotFound, [])
    assert.deepEqual(localErrors, [])
  } catch (error) {
    if (relay) error.message += `\nrelay diagnostics: ${JSON.stringify(relay.diagnostics())}`
    if (node) error.message += `\nnode diagnostics: ${JSON.stringify(node.diagnostics())}`
    throw error
  } finally {
    await browser?.close()
    if (node) await stopProcess(node)
    if (relay) {
      if (relayImage) await stopContainerRelay(relay)
      else await stopProcess(relay)
    }
    await model.close()
    await rm(temporary, { recursive: true, force: true })
  }
})
