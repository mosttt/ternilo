import assert from 'node:assert/strict'
import { randomUUID } from 'node:crypto'
import { mkdir, mkdtemp, readFile, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import { spawn } from 'node:child_process'
import test from 'node:test'

const webRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..')
const repository = path.resolve(webRoot, '..')
const binary = process.env.TERNILO_E2E_DESKTOP_BINARY ?? path.join(repository, 'target', 'debug', 'ternilo-desktop')

function workspaceLink(workspace) {
  return `ternilo://workspace?path=${encodeURIComponent(workspace)}`
}

function desktopEnvironment(temporary) {
  return {
    ...Object.fromEntries(Object.entries(process.env).filter(([key]) => !key.startsWith('TERNILO_'))),
    XDG_CACHE_HOME: path.join(temporary, 'xdg-cache'),
    XDG_CONFIG_HOME: path.join(temporary, 'xdg-config'),
    XDG_DATA_HOME: path.join(temporary, 'xdg-data'),
    XDG_STATE_HOME: path.join(temporary, 'xdg-state'),
  }
}

function startDesktop(temporary, dataDirectory, testInstance, deepLink) {
  const child = spawn(binary, [
    '--listen', '127.0.0.1:0',
    '--data-dir', dataDirectory,
    '--test-instance', testInstance,
    deepLink,
  ], {
    cwd: repository,
    env: desktopEnvironment(temporary),
    stdio: ['ignore', 'pipe', 'pipe'],
  })
  let stdout = ''
  let stderr = ''
  child.stdout.setEncoding('utf8')
  child.stderr.setEncoding('utf8')
  child.stdout.on('data', chunk => { stdout += chunk })
  child.stderr.on('data', chunk => { stderr += chunk })
  return {
    child,
    diagnostics: () => ({ stdout, stderr }),
    origin: () => stdout.match(/Ternilo local web: (http:\/\/[^\s]+)/)?.[1],
  }
}

async function waitForExit(process_, timeoutMs) {
  if (process_.child.exitCode !== null || process_.child.signalCode !== null) return process_.child.exitCode
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      process_.child.off('exit', onExit)
      reject(new Error(`desktop process did not exit: ${JSON.stringify(process_.diagnostics())}`))
    }, timeoutMs)
    function onExit(code) { clearTimeout(timer); resolve(code) }
    process_.child.once('exit', onExit)
  })
}

async function stopDesktop(process_) {
  if (process_.child.exitCode !== null || process_.child.signalCode !== null) return
  process_.child.kill('SIGINT')
  try {
    await waitForExit(process_, 5000)
  } catch {
    process_.child.kill('SIGKILL')
    await waitForExit(process_, 5000)
  }
}

async function stopService(dataDirectory) {
  let connection
  try {
    connection = JSON.parse(await readFile(path.join(dataDirectory, 'runtime', 'service.json'), 'utf8'))
  } catch (error) {
    if (error.code === 'ENOENT') return
    throw error
  }
  const origin = `http://${connection.info.address}`
  const response = await fetch(`${origin}/api/v1/service/stop`, {
    method: 'POST', headers: { authorization: `Bearer ${connection.api_token}` },
  })
  assert.equal(response.status, 202)
  const deadline = Date.now() + 15_000
  while (Date.now() < deadline) {
    try { await readFile(path.join(dataDirectory, 'runtime', 'service.json')) } catch (error) {
      if (error.code === 'ENOENT') return
      throw error
    }
    await new Promise(resolve => setTimeout(resolve, 100))
  }
  throw new Error('desktop service did not finish shutting down')
}

async function waitForApplication(process_) {
  const deadline = Date.now() + 30_000
  while (Date.now() < deadline) {
    if (process_.child.exitCode !== null) {
      throw new Error(`desktop exited during startup: ${JSON.stringify(process_.diagnostics())}`)
    }
    const origin = process_.origin()
    if (!origin) {
      await new Promise(resolve => setTimeout(resolve, 100))
      continue
    }
    try {
      const response = await fetch(origin)
      if (response.ok) {
        const html = await response.text()
        const token = html.match(/"apiToken":"([a-f0-9]+)"/)?.[1]
        if (token) return { origin, token }
      }
    } catch {}
    await new Promise(resolve => setTimeout(resolve, 100))
  }
  throw new Error(`desktop loopback host did not start: ${JSON.stringify(process_.diagnostics())}`)
}

async function waitForWorkspaces(origin, token, expectedPaths) {
  const deadline = Date.now() + 15_000
  while (Date.now() < deadline) {
    const response = await fetch(`${origin}/api/v1/state`, {
      headers: { authorization: `Bearer ${token}` },
    })
    if (response.ok) {
      const state = await response.json()
      const paths = state.workspaces.map(workspace => workspace.path)
      if (expectedPaths.every(expected => paths.includes(expected))) return state
    }
    await new Promise(resolve => setTimeout(resolve, 100))
  }
  throw new Error(`desktop did not activate workspaces: ${expectedPaths.join(', ')}`)
}

async function createSession(origin, token, workspaceId) {
  const response = await fetch(`${origin}/api/v1/sessions`, {
    method: 'POST',
    headers: {
      authorization: `Bearer ${token}`,
      'content-type': 'application/json',
    },
    body: JSON.stringify({ workspace_id: workspaceId }),
  })
  assert.equal(response.status, 201)
  return response.json()
}

test('desktop binds before navigation and forwards cold/hot deep links through its single instance', { timeout: 90_000 }, async t => {
  if (!process.env.DISPLAY && !process.env.WAYLAND_DISPLAY) {
    t.skip('desktop smoke requires a graphical session or virtual display')
    return
  }

  const temporary = await mkdtemp(path.join(tmpdir(), 'ternilo-desktop-smoke-'))
  const dataDirectory = path.join(temporary, 'data')
  const firstWorkspace = path.join(temporary, 'workspace-one')
  const secondWorkspace = path.join(temporary, 'workspace-two')
  const testInstance = randomUUID()
  await mkdir(dataDirectory)
  await mkdir(firstWorkspace)
  await mkdir(secondWorkspace)

  const primary = startDesktop(temporary, dataDirectory, testInstance, workspaceLink(firstWorkspace))
  let service
  try {
    service = await waitForApplication(primary)
    const { origin, token } = service
    assert.equal(new URL(origin).hostname, '127.0.0.1')
    assert.notEqual(new URL(origin).port, '3210')
    await waitForWorkspaces(origin, token, [firstWorkspace])

    const secondary = startDesktop(temporary, dataDirectory, testInstance, workspaceLink(secondWorkspace))
    assert.equal(await waitForExit(secondary, 10_000), 0)
    assert.equal(primary.child.exitCode, null)
    const state = await waitForWorkspaces(origin, token, [firstWorkspace, secondWorkspace])
    assert.equal(state.workspaces.length, 2)
    const session = await createSession(origin, token, state.workspaces[0].workspace_id)
    assert.equal(session.workspace_id, state.workspaces[0].workspace_id)
    assert.ok(session.identity.session_id)
    await stopDesktop(primary)
    const response = await fetch(`${origin}/api/v1/service`, {
      headers: { authorization: `Bearer ${token}` },
    })
    assert.equal(response.status, 200, 'closing the desktop keeps its service running')
  } finally {
    await stopDesktop(primary)
    await stopService(dataDirectory)
    await rm(temporary, { recursive: true, force: true, maxRetries: 20, retryDelay: 100 })
  }
})

test('desktop reuses a service started by the CLI and keeps its workspace state', { timeout: 90_000 }, async t => {
  if (!process.env.DISPLAY && !process.env.WAYLAND_DISPLAY) {
    t.skip('desktop smoke requires a graphical session or virtual display')
    return
  }
  const temporary = await mkdtemp(path.join(tmpdir(), 'ternilo-desktop-shared-'))
  const dataDirectory = path.join(temporary, 'data')
  const workspace = path.join(temporary, 'workspace')
  await mkdir(workspace)
  const child = spawn(process.env.TERNILO_E2E_NODE_BINARY ?? path.join(repository, 'target', 'debug', 'ternilo'), [
    'serve', '--listen', '127.0.0.1:0', '--data-dir', dataDirectory,
  ], { cwd: repository, env: desktopEnvironment(temporary), stdio: ['ignore', 'pipe', 'pipe'] })
  let output = ''
  child.stdout.on('data', chunk => { output += chunk })
  child.stderr.on('data', chunk => { output += chunk })
  const cli = {
    child, diagnostics: () => output,
    origin: () => output.match(/Ternilo local web: (http:\/\/[^\s]+)/)?.[1],
  }
  let desktop
  try {
    const initial = await waitForApplication(cli)
    const before = JSON.parse(await readFile(path.join(dataDirectory, 'runtime', 'service.json'), 'utf8'))
    desktop = startDesktop(temporary, dataDirectory, randomUUID(), workspaceLink(workspace))
    const attached = await waitForApplication(desktop)
    assert.deepEqual(attached, initial)
    await waitForWorkspaces(initial.origin, initial.token, [workspace])
    const after = JSON.parse(await readFile(path.join(dataDirectory, 'runtime', 'service.json'), 'utf8'))
    assert.equal(after.info.service_id, before.info.service_id)
    assert.equal(after.info.pid, child.pid)
    await stopDesktop(desktop)
    assert.equal(child.exitCode, null)
    await waitForWorkspaces(initial.origin, initial.token, [workspace])
  } finally {
    if (desktop) await stopDesktop(desktop)
    try { await stopService(dataDirectory) } finally { await stopDesktop(cli) }
    await rm(temporary, { recursive: true, force: true, maxRetries: 20, retryDelay: 100 })
  }
})
