import assert from 'node:assert/strict'
import { mkdir, readFile, writeFile } from 'node:fs/promises'
import path from 'node:path'
import { serverRequest } from './platform-e2e-fixture.mjs'

const mcpSource = String.raw`
import { appendFileSync, existsSync, writeFileSync } from 'node:fs'
import { createInterface } from 'node:readline'
import path from 'node:path'
const file = name => path.join(process.env.RELAY_SERVICE_DIRECTORY, name)
appendFileSync(file('starts'), 'start\n')
writeFileSync(file('pid'), String(process.pid))
setInterval(() => {
  if (existsSync(file('stop'))) process.exit(0)
  try { appendFileSync(file('heartbeat'), 'tick\n') } catch { process.exit(0) }
}, 20)
const input = createInterface({ input: process.stdin })
input.on('close', () => process.exit(0))
input.on('line', line => {
  const request = JSON.parse(line)
  if (request.id === undefined) return
  const result = request.method === 'initialize' ? {
    protocolVersion: request.params.protocolVersion,
    capabilities: { tools: {} }, serverInfo: { name: 'relay-service', version: '1' },
  } : request.method === 'tools/list' ? { tools: [{
    name: 'echo', description: 'Echo fixture text', inputSchema: { type: 'object' },
  }] } : { content: [{ type: 'text', text: 'Relay fixture' }] }
  process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id: request.id, result }) + '\n')
})
`

async function until(read, ready, label) {
  const deadline = Date.now() + 20_000
  while (Date.now() < deadline) {
    const value = await read()
    if (ready(value)) return value
    await new Promise(resolve => setTimeout(resolve, 50))
  }
  throw new Error(`Timed out: ${label}`)
}

async function liveProcess(pid) {
  try { process.kill(pid, 0); return true } catch (error) {
    if (error.code === 'ESRCH') return false
    throw error
  }
}

export async function verifyNodeServices({ origin, owner, member, ownerRequest, localRequest,
  tenantId, memberIdentity, sessionId, localSessionId, workspacePath, artifacts, model }) {
  const directory = path.join(workspacePath, 'relay-service')
  await mkdir(directory)
  const script = path.join(directory, 'fixture.mjs')
  await writeFile(script, mcpSource)
  const bytes = name => readFile(path.join(directory, name)).then(value => value.length)
    .catch(error => { if (error.code === 'ENOENT') return 0; throw error })
  const serviceId = 'mcp:relay-fixture'
  const servicePath = `/sessions/${encodeURIComponent(sessionId)}/services`
  const localServicePath = `/sessions/${encodeURIComponent(localSessionId)}/services`
  const sharingPath = `/sessions/${encodeURIComponent(sessionId)}/sharing/user/${memberIdentity.user.user_id}`
  const memberRequest = resource => serverRequest(origin, resource, { token: memberIdentity.access_token, tenantId })
  const current = async () => (await ownerRequest(servicePath, { tenantId })).find(service => service.id === serviceId)
  const observations = { denied: [], processId: null, modelRequestsBefore: model.records.length }
  const denied = async (resource, method) => {
    const response = await fetch(`${origin}/api/v1${resource}`, {
      method, headers: { authorization: `Bearer ${memberIdentity.access_token}`, 'x-ternilo-tenant': tenantId },
    })
    assert.equal(response.status, 403, `${method} ${resource} is denied by resource permissions`)
    assert.equal((await response.json()).error.code, 'policy_denied')
    observations.denied.push({ method, resource, status: response.status })
  }
  try {
    await localRequest(`/sessions/${localSessionId}`, { method: 'PATCH', body: {
      permissions: 'full_access', profile_plugins: [{ id: 'relay-mcp', kind: 'ternilo.mcp.stdio', enabled: true,
        config: { server_name: 'relay-fixture', command: process.execPath, args: [script],
          env: { RELAY_SERVICE_DIRECTORY: directory }, startup_timeout_ms: 5000, tool_call_timeout_ms: 5000 } }],
    } })
    await ownerRequest(sharingPath, { tenantId, method: 'PUT', body: { view: true, submit: false, stop: false, configure: false } })
    assert.equal((await current()).status, 'idle')
    assert.equal((await memberRequest(servicePath)).find(service => service.id === serviceId).status, 'idle')
    assert.equal(await bytes('starts'), 0, 'cold queries through the Server must not launch Node services')

    await owner.setViewportSize({ width: 1440, height: 960 })
    await owner.reload()
    await owner.getByRole('button', { name: '更多会话操作', exact: true }).click()
    await owner.getByRole('menuitem', { name: '后台服务', exact: true }).click()
    const ownerDialog = owner.getByRole('dialog', { name: '后台服务', exact: true })
    const ownerService = ownerDialog.locator(`[data-session-service="${serviceId}"]`)
    await ownerService.locator('[data-status="idle"]').waitFor()
    await member.setViewportSize({ width: 390, height: 844 })
    await member.reload()
    const readOnlyEntry = member.getByRole('button', { name: '后台服务', exact: true })
    await readOnlyEntry.waitFor()
    assert.ok((await readOnlyEntry.boundingBox()).height >= 40, 'the mobile read-only service entry remains usable')
    await readOnlyEntry.click()
    const memberDialog = member.getByRole('dialog', { name: '后台服务', exact: true })
    const memberService = memberDialog.locator(`[data-session-service="${serviceId}"]`)
    await memberService.locator('[data-status="idle"]').waitFor()
    assert.equal(await memberService.getByRole('button', { name: '启动', exact: true }).isDisabled(), true)
    assert.equal(await bytes('starts'), 0, 'opening both service dialogs must remain a cold read')
    await denied(`${servicePath}/${encodeURIComponent(serviceId)}/start`, 'POST')
    assert.equal(await bytes('starts'), 0, 'a forbidden Start must never reach the MCP process')

    async function change(action, label, status) {
      const response = owner.waitForResponse(response => response.request().method() === 'POST'
        && response.url() === `${origin}/api/v1${servicePath}/${encodeURIComponent(serviceId)}/${action}`)
      await ownerService.getByRole('button', { name: label, exact: true }).click()
      assert.equal((await response).ok(), true, `owner ${action} succeeds through the Server relay`)
      await ownerService.locator(`[data-status="${status}"]`).waitFor()
      assert.equal((await localRequest(localServicePath)).find(service => service.id === serviceId).status, status)
      await memberService.locator(`[data-status="${status}"]`).waitFor()
    }
    await change('start', '启动', 'running')
    observations.processId = Number(await readFile(path.join(directory, 'pid'), 'utf8'))
    assert.ok(observations.processId > 0)
    assert.equal(await liveProcess(observations.processId), true)
    await until(() => bytes('heartbeat'), size => size >= 20, 'the actual Node MCP process is writing')
    assert.equal(await bytes('starts'), 'start\n'.length)
    assert.equal(await memberService.getByRole('button', { name: '停止', exact: true }).isDisabled(), true)
    await denied(`${servicePath}/${encodeURIComponent(serviceId)}/stop`, 'POST')
    const heartbeatBeforeDenial = await bytes('heartbeat')
    await until(() => bytes('heartbeat'), size => size > heartbeatBeforeDenial, 'a forbidden Stop leaves the owner service running')
    await owner.screenshot({ path: path.join(artifacts, 'node-services-owner-desktop.png'), animations: 'disabled' })
    assert.equal(await member.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true)
    await member.screenshot({ path: path.join(artifacts, 'node-services-viewer-mobile.png'), animations: 'disabled' })

    await change('stop', '停止', 'stopped')
    assert.equal(await liveProcess(observations.processId), false, 'successful relayed Stop joins the actual MCP process')
    const stoppedBytes = await bytes('heartbeat')
    await new Promise(resolve => setTimeout(resolve, 150))
    assert.equal(await bytes('heartbeat'), stoppedBytes, 'the stopped Node service cannot continue writing')
    assert.equal(model.records.length, observations.modelRequestsBefore, 'service controls do not submit model tasks')
    assert.equal((await current()).status, 'stopped')
    await owner.keyboard.press('Escape')
    await ownerDialog.waitFor({ state: 'hidden' })
    await member.keyboard.press('Escape')
    await memberDialog.waitFor({ state: 'hidden' })
    // End the subscriber before intentionally revoking access; denied requests are checked above and below.
    await member.close()
    await ownerRequest(sharingPath, { tenantId, method: 'DELETE' })
    await denied(servicePath, 'GET')
    observations.stoppedHeartbeatBytes = stoppedBytes
    observations.modelRequestsAfter = model.records.length
  } finally {
    await writeFile(path.join(directory, 'stop'), '')
    await writeFile(path.join(artifacts, 'node-services-observations.json'), JSON.stringify(observations, null, 2))
  }
}
