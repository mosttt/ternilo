import assert from 'node:assert/strict'
import { spawn } from 'node:child_process'
import { mkdir, mkdtemp, readFile, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { freePort, initializeServer, repository, serverRequest, startProcess, stopProcess, waitForHttp } from './platform-e2e-fixture.mjs'
import { localApi, until } from './model-device-fixture.mjs'

function sdkProcess(language, config) {
  const command = language === 'python' ? process.env.TERNILO_E2E_PYTHON ?? 'python3' : process.execPath
  const file = language === 'python' ? 'sdk/python/tests/server_smoke.py' : 'sdk/typescript/test/server-smoke.ts'
  const child = spawn(command, [path.join(repository, file)], { cwd: repository, env: { ...process.env,
    PYTHONPATH: path.join(repository, 'sdk/python/src'), TERNILO_SDK_TEST_CONFIG: JSON.stringify({ ...config, language }),
  }, stdio: ['ignore', 'pipe', 'pipe'] })
  let output = '', errors = ''
  child.stdout.setEncoding('utf8'); child.stderr.setEncoding('utf8')
  child.stdout.on('data', chunk => { output += chunk })
  child.stderr.on('data', chunk => { errors += chunk })
  const result = { child, diagnostics: () => `${output}\n${errors}` }
  result.phase = name => until(async () => {
    if (child.exitCode !== null && child.exitCode !== 0) throw new Error(`${language} SDK exited ${child.exitCode}: ${result.diagnostics()}`)
    return output.split('\n').filter(Boolean).map(line => JSON.parse(line))
  }, frames => frames.some(frame => frame.phase === name), `${language} SDK ${name}`)
  return result
}

test('Python and TypeScript SDKs execute shared Node files, resume after Server restart and stop on revoked sharing', { timeout: 240_000 }, async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'ternilo-server-sdk-'))
  const processes = []
  const environment = { XDG_STATE_HOME: path.join(directory, 'state') }
  try {
    const origin = `http://127.0.0.1:${await freePort()}`
    let server = await initializeServer({ directory: path.join(directory, 'server'), origin, environment })
    processes.push(server)
    const configPath = server.configPath, token = server.owner.session.access_token
    const unscoped = (resource, options = {}) => serverRequest(origin, resource, { token, ...options })
    const team = (await unscoped('/tenants', { body: { slug: 'sdk-team', display_name: 'SDK team' } })).tenant
    const tenantId = team.tenant_id
    const owner = (resource, options = {}) => unscoped(resource, { tenantId, ...options })
    const invitation = await owner('/admin/invitations', { body: { tenant_id: null, role: 'member', expires_in_seconds: 3600 } })
    const member = await serverRequest(origin, '/auth/invitations/accept', { body: { token: invitation.token, username: 'sdk-member', email: 'sdk-member@example.test', password: 'sdk-member-password' } })
    await owner(`/tenants/${tenantId}/members/${member.user.user_id}`, { method: 'PUT', body: { role: 'member' } })
    const project = (await owner('/projects')).projects[0]
    const enrollment = (await owner(`/tenants/${tenantId}/my-computer-enrollments`, { body: { executor_id: 'sdk-node', project_id: project.project_id, ttl_seconds: 600 } })).enrollment
    const credential = (await owner('/enrollments/consume', { body: { token: enrollment.token } })).credential
    const nodeOrigin = `http://127.0.0.1:${await freePort()}`
    const node = startProcess(process.env.TERNILO_E2E_NODE_BINARY ?? path.join(repository, 'target/debug/ternilo'),
      ['serve', '--listen', new URL(nodeOrigin).host, '--data-dir', path.join(directory, 'node'), '--node-id', 'sdk-node',
        '--gateway-url', `${origin.replace('http:', 'ws:')}/api/v1/executors/connect`, '--allow-insecure-gateway'],
      { ...environment, TERNILO_LOCAL_TOKEN: credential.token })
    processes.push(node); await waitForHttp(nodeOrigin, node)
    const local = await localApi(nodeOrigin)
    const folder = path.join(directory, 'workspace'); await mkdir(folder)
    const workspace = await local('/workspaces', { body: { path: folder } })
    for (const language of ['python', 'typescript']) {
      const session = await local('/sessions', { body: { workspace_id: workspace.workspace_id } })
      const localId = session.identity.session_id
      await local(`/sessions/${localId}`, { method: 'PATCH', body: { title: `SDK ${language}` } })
      const state = await until(() => owner('/state'), value => value.sessions.some(value => value.title === `SDK ${language}`), `${language} session mapped`)
      const publicId = state.sessions.find(value => value.title === `SDK ${language}`).identity.session_id
      const sharing = `/sessions/${publicId}/sharing/user/${member.user.user_id}`
      await owner(sharing, { method: 'PUT', body: { view: true, submit: true, stop: true, configure: false } })
      const sdk = sdkProcess(language, { origin, token: member.access_token, tenant: tenantId, session: publicId })
      processes.push(sdk)
      await sdk.phase('ready')
      assert.equal(await readFile(path.join(folder, `sdk-${language}.txt`), 'utf8'), `${language} remote proof`)
      await stopProcess(server)
      const runId = `offline-sdk-${language}`
      await local(`/sessions/${localId}/queue`, { body: { run_id: runId, content: { kind: 'prompt', input: `/read sdk-${language}.txt` } } })
      await until(() => local(`/sessions/${localId}/events`), events => events.some(event => event.run_id === runId && event.type === 'turn_finished'), 'offline task completed')
      server = startProcess(process.env.TERNILO_E2E_SERVER_BINARY ?? path.join(repository, 'target/debug/ternilo-server'), ['serve', '--config-dir', path.dirname(configPath)], environment)
      processes.push(server); await waitForHttp(`${origin}/readyz`, server)
      await sdk.phase('resumed')
      await sdk.phase('revoke')
      await owner(sharing, { method: 'DELETE' })
      await sdk.phase('done')
      await until(async () => sdk.child.exitCode, value => value === 0, `${language} SDK clean exit`)
      const events = await owner(`/sessions/${publicId}/events`)
      const remoteInputs = events.filter(event => event.type === 'user_message' && event.run_id !== runId)
      assert.ok(remoteInputs.length >= 2)
      assert.ok(remoteInputs.every(event => event.provenance?.author.user_id === member.user.user_id))
    }
  } catch (error) {
    error.message += `\n${processes.map(process => process.diagnostics()).join('\n')}`
    throw error
  } finally {
    for (const process of processes.reverse()) await stopProcess(process)
    await rm(directory, { recursive: true, force: true })
  }
})
