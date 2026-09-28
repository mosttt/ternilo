import { describe, expect, it } from 'vitest'
import type { SessionEvent } from '@/types'
import { conversationEvents } from './conversation-events'
import { currentTurnActivity } from './turn-activity'
import { buildTrajectory } from './trajectory'

function event(seq: number, type: string, runId = 'batch', content?: string): SessionEvent {
  return { seq, type, run_id: runId, content, occurred_at_ms: seq * 100 }
}

describe('conversation replacement', () => {
  it('replaces an input inside a batch without resending or deleting its prefix', () => {
    const history = [event(0, 'turn_started'), event(1, 'user_message', 'batch', '1'),
      event(2, 'user_message', 'batch', '12'), event(3, 'user_message', 'batch', '3'),
      event(4, 'turn_cancelled'), event(5, 'turn_started', 'replacement'),
      { ...event(6, 'user_message', 'replacement', 'edited 12'), source: {
        kind: 'submission' as const, submission_id: 'replacement-input', created_at_ms: 600,
        delivery: 'queue' as const, regenerate_from: 2,
      } }, event(7, 'turn_finished', 'replacement'), event(8, 'turn_failed'),
    ]
    const active = conversationEvents(history)
    expect(active.map(event => event.seq)).toEqual([0, 1, 5, 6, 7])
    expect(active.filter(event => event.type === 'user_message').map(event => event.content)).toEqual(['1', 'edited 12'])
    expect(history).toHaveLength(9)
    expect(conversationEvents(active)).toBe(active)
    expect(currentTurnActivity(active)).toBeNull()
    expect(buildTrajectory(active).map(turn => turn.status)).toEqual(['complete', 'complete'])
  })
})
