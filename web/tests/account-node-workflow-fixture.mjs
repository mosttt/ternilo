import assert from 'node:assert/strict'
import { until } from './model-device-fixture.mjs'

export async function workflowOriginTask({ page, owner, member, session, sessionId, upstream, resourceOwner }) {
  const before = new Set((await member('/model-access/requests?limit=100')).requests.map(request => request.request_id))
  const previousSessions = new Set((await owner('/state')).sessions.map(value => value.identity.session_id))
  const hold = upstream.holdNext(body => body.stream && JSON.stringify(body.messages.findLast(message => message.role === 'user')).includes('workflow-origin-proof'))
  try {
    const workflow = {
      meta: { name: 'model-origin-workflow', description: 'Verify parallel and pipeline model authority', phases: [{ title: 'parallel' }, { title: 'verify' }] },
      script: 'phase("parallel"); let results = parallel([task("workflow-origin-proof alpha", #{ label: "alpha" }), task("workflow-origin-proof beta", #{ label: "beta" })]); phase("verify"); pipeline(results, [stage("workflow-origin-proof verify {{prev}}", #{ label: "verify" })])',
      args: {},
    }
    const accepted = await member(`/sessions/${sessionId}/queue`, { body: { content: { kind: 'prompt', input: `/workflow ${JSON.stringify(workflow)}` } } })
    await page.getByRole('button', { name: '允许一次', exact: true }).last().click()
    await Promise.race([hold.began, new Promise((_, reject) => setTimeout(() => reject(new Error('workflow did not reach its authorized model gateway')), 15000))])
    const later = await owner(`/sessions/${sessionId}/queue`, { body: { content: { kind: 'prompt', input: '/code "owner-input-after-workflow"' } } })
    assert.notEqual(later.run_id, accepted.run_id)
    hold.release()
    const events = await until(() => owner(`/sessions/${sessionId}/events`), values => values.some(event => event.run_id === accepted.run_id && ['turn_finished', 'turn_failed'].includes(event.type)), 'workflow completion')
    const workflowEvents = events.filter(event => event.run_id === accepted.run_id)
    assert.equal(workflowEvents.some(event => event.type === 'turn_failed'), false, JSON.stringify(workflowEvents))
    const finished = workflowEvents.find(event => event.type === 'workflow_run_finished')
    assert.equal(finished?.stop_reason, 'completed', JSON.stringify(workflowEvents))
    assert.equal(finished.agents_started, 4)
    const children = (await until(() => owner('/state'), state => state.sessions.filter(value => value.parent_session_id === sessionId && !previousSessions.has(value.identity.session_id)).length === 4, 'four workflow child sessions')).sessions.filter(value => value.parent_session_id === sessionId && !previousSessions.has(value.identity.session_id))
    for (const child of children) {
      const history = await owner(`/sessions/${child.identity.session_id}/events`)
      assert.ok(history.some(event => event.type === 'user_message' && event.run_id === accepted.run_id), 'workflow child retains its accepted parent run')
      assert.ok(history.some(event => event.type === 'turn_finished'), 'each workflow task and stage completes')
      assert.equal(history.some(event => event.type === 'turn_failed'), false, JSON.stringify(history))
    }
    const requests = (await member('/model-access/requests?limit=100')).requests.filter(request => !before.has(request.request_id))
    assert.ok(requests.length >= 4, 'both fan-out tasks and their verification stages use the gateway')
    for (const request of requests) {
      assert.equal(request.actor_user_id, session.user.user_id)
      assert.equal(request.model_beneficiary_user_id, session.user.user_id)
      assert.equal(request.resource_owner_user_id, resourceOwner)
      assert.equal(request.source, 'user_provider')
      assert.ok(request.accounted_tokens > 0)
    }
    await until(() => owner(`/sessions/${sessionId}/queue`), queue => !queue.active_run_id && !queue.items.some(item => item.placement === 'queued'), 'later owner input completes')
  } finally { hold.release() }
}

export async function workflowInterruptedTask({ page, owner, member, session, sessionId, upstream, resourceOwner, revoke }) {
  const before = new Set((await member('/model-access/requests?limit=100')).requests.map(request => request.request_id))
  const previousSessions = new Set((await owner('/state')).sessions.map(value => value.identity.session_id))
  const marker = revoke ? 'workflow-revocation-proof' : 'workflow-cancellation-proof'
  const calls = upstream.calls.length
  const hold = upstream.holdNext(body => body.stream && JSON.stringify(body.messages.findLast(message => message.role === 'user')).includes(marker))
  try {
    const workflow = {
      meta: { name: marker, description: 'Stop a workflow without starting its next stage', phases: [{ title: 'verify' }] },
      script: `pipeline(["input"], [stage("${marker} first {{item}}"), stage("${marker} must-not-run {{prev}}")])`,
      args: {},
    }
    const accepted = await member(`/sessions/${sessionId}/queue`, { body: { content: { kind: 'prompt', input: `/workflow ${JSON.stringify(workflow)}` } } })
    await page.getByRole('button', { name: '允许一次', exact: true }).last().click()
    await Promise.race([hold.began, new Promise((_, reject) => setTimeout(() => reject(new Error(`${marker} did not reach its model gateway`)), 15000))])
    if (revoke) await revoke()
    else await member(`/sessions/${sessionId}/turns/${accepted.run_id}`, { method: 'DELETE' })
    const terminal = revoke ? 'turn_finished' : 'turn_cancelled'
    await until(() => owner(`/sessions/${sessionId}/events`), events => events.some(event => event.run_id === accepted.run_id && event.type === terminal), `${marker} finishes while the upstream is still held`)
    await until(() => owner(`/sessions/${sessionId}/queue`), queue => !queue.active_run_id, `${marker} queue is idle`)
    const children = (await owner('/state')).sessions.filter(value => value.parent_session_id === sessionId && !previousSessions.has(value.identity.session_id))
    assert.equal(children.length, 1, 'interruption must prevent the next pipeline stage from starting')
    const history = await owner(`/sessions/${children[0].identity.session_id}/events`)
    assert.ok(history.some(event => event.run_id === accepted.run_id && event.type === (revoke ? 'turn_failed' : 'turn_cancelled')), JSON.stringify(history))
    const requests = (await until(() => member('/model-access/requests?limit=100'), value => {
      const created = value.requests.filter(request => !before.has(request.request_id))
      return created.length > 0 && created.every(request => request.state !== 'pending')
    }, `${marker} ledger settles`)).requests.filter(request => !before.has(request.request_id))
    for (const request of requests) {
      assert.equal(request.actor_user_id, session.user.user_id)
      assert.equal(request.model_beneficiary_user_id, session.user.user_id)
      assert.equal(request.resource_owner_user_id, resourceOwner)
      assert.equal(request.source, 'user_provider')
      assert.equal(request.accounted_tokens, null, 'interruption before upstream usage remains unknown, not zero')
    }
    assert.equal(upstream.calls.slice(calls).filter(call => call.stream).length, 1, 'no retry or later stage may reach the upstream')
  } finally { hold.release() }
}
