import { describe, expect, it } from 'vitest'
import type { SessionEvent } from '@/types'
import { createSessionEventBuffer, mergeSessionEventBuffer } from './session-event-buffer'

const event = (seq: number, type = 'assistant_message') => ({ seq, type } as SessionEvent)

describe('session event buffer', () => {
  it('rejects a stale delta after selection moves to another session', () => {
    let buffer = createSessionEventBuffer('source')
    buffer = mergeSessionEventBuffer(buffer, 'source', [event(1), event(4)])
    buffer = createSessionEventBuffer('fork')

    const afterStaleSourceDelta = mergeSessionEventBuffer(buffer, 'source', [event(8)])
    expect(afterStaleSourceDelta).toBe(buffer)
    expect(afterStaleSourceDelta.events).toEqual([])

    buffer = mergeSessionEventBuffer(buffer, 'fork', [event(1)])
    expect(buffer.events.map(item => item.seq)).toEqual([1])
  })

  it('merges replacement events by seq in stable order for the active session', () => {
    const original = event(2, 'assistant_message')
    const replacement = event(2, 'turn_finished')
    let buffer = createSessionEventBuffer('active')
    buffer = mergeSessionEventBuffer(buffer, 'active', [original, event(4)])
    buffer = mergeSessionEventBuffer(buffer, 'active', [event(3), replacement])

    expect(buffer.events.map(item => item.seq)).toEqual([2, 3, 4])
    expect(buffer.events[0]).toBe(replacement)
  })
})
