import { describe, expect, it } from 'vitest'
import { canAnswerQuestion, ownsResource, resourcePermissions } from './resource-access'

describe('Resource capabilities', () => {
  it('keeps local access unchanged and separates shared stop from submit or configuration', () => {
    expect(resourcePermissions(undefined, true)).toEqual({ view: true, submit: true, stop: true, configure: true })
    const access = { owner_user_id: 'owner', is_owner: false, sources: [], role_limited: false, permissions: { view: true, submit: false, stop: true, configure: false } }
    expect(resourcePermissions(access, true)).toEqual(access.permissions)
    expect(ownsResource(access, true)).toBe(false)
    expect(ownsResource({ ...access, permissions: { ...access.permissions, configure: true } }, true)).toBe(false)
  })
})

describe('Canonical question capabilities', () => {
  const question = { id: 'question', question: 'Continue?', options: [], multi_select: false }
  const read = { view: true, submit: false, configure: false, stop: false }
  it('separates submitting, configuring, stopping and owner-only approvals', () => {
    const cases = [
      ['shell', 'submit'],
      ['extension_set_mounted', 'configure'],
      ['exit_plan_mode', 'configure'],
      ['interrupt_agent', 'stop'],
      ['job_kill', 'stop'],
      ['terminal_close', 'stop'],
      ['schedule_delete', 'stop'],
    ] as const
    for (const [tool_name, capability] of cases) {
      const approval = { ...question, tool_approval: { tool_name, call_id: 'call', reason: '', arguments: {} } }
      for (const granted of ['submit', 'configure', 'stop'] as const) {
        expect(canAnswerQuestion(approval, { ...read, [granted]: true }, false)).toBe(granted === capability)
      }
    }
    for (const tool_name of ['extension_set_enabled', 'extension_revoke']) {
      const approval = { ...question, tool_approval: { tool_name, call_id: 'call', reason: '', arguments: {} } }
      expect(canAnswerQuestion(approval, { view: true, submit: true, stop: true, configure: true }, false)).toBe(false)
      expect(canAnswerQuestion(approval, { ...read, configure: true }, true)).toBe(true)
    }
  })
  it('allows Configure-only plan review and keeps ordinary questions under Submit', () => {
    const plan = { ...question, presentation: { kind: 'plan_review' as const, title: '', plan: '', approve_label: 'Approve' } }
    expect(canAnswerQuestion(plan, { ...read, configure: true }, false)).toBe(true)
    expect(canAnswerQuestion(plan, { ...read, submit: true }, false)).toBe(false)
    expect(canAnswerQuestion(question, { ...read, configure: true }, false)).toBe(false)
    expect(canAnswerQuestion(question, { ...read, submit: true }, false)).toBe(true)
    expect(canAnswerQuestion(question, { view: false, submit: true, stop: true, configure: true }, true)).toBe(false)
  })
})
