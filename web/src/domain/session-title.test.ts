import { describe, expect, it } from 'vitest'
import type { SessionEvent } from '@/types'
import { sessionTitleGenerationPending } from './session-title'

function event(seq: number, type: string, values: Record<string, unknown> = {}): SessionEvent {
  return { seq, type, run_id: 'run-title', occurred_at_ms: seq, ...values }
}

describe('session title generation state', () => {
  it('tracks the canonical generation lifecycle independently from the model turn', () => {
    expect(sessionTitleGenerationPending([
      event(1, 'turn_finished'),
      event(2, 'session_title_generation_started'),
    ])).toBe(true)
    expect(sessionTitleGenerationPending([
      event(1, 'session_title_generation_started'),
      event(2, 'session_title_generation_finished', { generated: false }),
    ])).toBe(false)
    expect(sessionTitleGenerationPending([
      event(1, 'session_title_generation_started'),
      event(2, 'session_title_generated', { title: 'Review provider flow' }),
      event(3, 'session_title_generation_finished', { generated: true }),
    ])).toBe(true)
  })
})
