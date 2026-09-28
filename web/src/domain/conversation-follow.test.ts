import { afterEach, expect, it } from 'vitest'
import type { SessionEvent } from '@/types'
import { outerScrollReceives, visibleOutputPosition } from './conversation-follow'

afterEach(() => document.body.replaceChildren())

it('ends output reflow tracking throughout dependency waits, capacity waits and readmission until new output arrives', () => {
  const event = (seq: number, type: string, extra = {}) => ({ seq, type, ...extra } as SessionEvent)
  const events = [event(1, 'turn_started'), event(2, 'tool_call_started')]
  for (const phase of ['waiting_for_subagents', 'waiting_for_capacity', 'running']) {
    events.push(event(events.length + 1, 'execution_activity_changed', { phase }))
    expect(visibleOutputPosition(events)).toEqual({ seq: 2, afterBoundary: false })
  }
  events.push(event(6, 'assistant_message_delta', { delta: 'Resumed output' }))
  expect(visibleOutputPosition(events)).toEqual({ seq: 6, afterBoundary: true })
})

it('keeps internal scrolling and contained boundaries separate from the conversation', () => {
  const host = document.createElement('div')
  const inner = document.createElement('pre')
  const target = document.createElement('span')
  host.append(inner); inner.append(target); document.body.append(host)
  inner.style.overflowY = 'auto'
  Object.defineProperties(inner, { scrollHeight: { value: 200 }, clientHeight: { value: 100 } })
  inner.scrollTop = 30
  expect(outerScrollReceives(target, host, -1)).toBe(false)
  inner.scrollTop = 0
  expect(outerScrollReceives(target, host, -1)).toBe(true)
  inner.style.overscrollBehaviorY = 'contain'
  expect(outerScrollReceives(target, host, -1)).toBe(false)
  inner.style.overflowY = 'hidden'
  expect(outerScrollReceives(target, host, -1)).toBe(true)
})

it('only tracks visible model output and disables reflow tracking while waiting for the next model response', () => {
  const event = (seq: number, type: string, extra = {}) => ({ seq, type, ...extra } as SessionEvent)
  const history = [event(1, 'turn_started'), event(2, 'model_request_started'), event(3, 'model_retry_scheduled')]
  expect(visibleOutputPosition(history)).toEqual({ seq: 0, afterBoundary: false })
  history.push(event(4, 'assistant_reasoning_delta', { delta: 'Thinking' }))
  expect(visibleOutputPosition(history)).toEqual({ seq: 4, afterBoundary: true })
  history.push(event(5, 'tool_call_started'), event(6, 'tool_call_finished'))
  expect(visibleOutputPosition(history)).toEqual({ seq: 6, afterBoundary: true })
  history.push(event(7, 'model_request_started'))
  expect(visibleOutputPosition(history)).toEqual({ seq: 6, afterBoundary: false })
  history.push(event(8, 'assistant_message', { response: { content: 'Done' } }))
  expect(visibleOutputPosition(history)).toEqual({ seq: 8, afterBoundary: true })
})
