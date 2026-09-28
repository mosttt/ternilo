import { describe, expect, it } from 'vitest'
import type { SessionEvent } from '@/types'
import { approvalDetailsSelection } from './approval-details'

function event(seq: number, type: string, values: Record<string, unknown>): SessionEvent {
  return { seq, run_id: 'run-1', occurred_at_ms: seq * 10, type, ...values } as SessionEvent
}

describe('approval details selection', () => {
  const asked = event(1, 'user_question_asked', {
    question: {
      id: 'approval-1',
      question: 'Allow?',
      options: [{ label: 'Allow once' }, { label: 'Deny' }],
      multi_select: false,
      tool_approval: {
        tool_name: 'shell',
        call_id: 'call-1',
        reason: 'Run the focused check',
        arguments: { command: 'cargo test' },
      },
    },
  })

  it('opens the durable question event while the approval is pending', () => {
    expect(approvalDetailsSelection([asked], 'call-1')).toEqual({
      kind: 'approval',
      event: asked,
      approval: {
        tool_name: 'shell',
        call_id: 'call-1',
        reason: 'Run the focused check',
        arguments: { command: 'cargo test' },
      },
    })
  })

  it('prefers the canonical tool trace after the approved call starts', () => {
    const started = event(2, 'tool_call_started', {
      call: { id: 'call-1', name: 'shell', arguments: { command: 'cargo test' } },
    })
    const selection = approvalDetailsSelection([asked, started], 'call-1')
    expect(selection?.kind).toBe('tool')
    if (selection?.kind === 'tool') {
      expect(selection.trace.id).toBe('call-1')
      expect(selection.trace.started).toBe(started)
    }
  })

  it('does not invent details for an unknown call', () => {
    expect(approvalDetailsSelection([asked], 'missing')).toBeNull()
  })
})
