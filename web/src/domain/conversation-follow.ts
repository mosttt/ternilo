import type { SessionEvent } from '@/types'

export type ScrollIntent = -1 | 0 | 1 | null

export function followAfterScroll(following: boolean, intent: ScrollIntent, previousTop: number, nextTop: number, atBottom: boolean): boolean {
  if (intent === null) return following
  if (intent !== 1 && nextTop < previousTop) return false
  if (intent !== -1 && nextTop > previousTop && atBottom) return true
  return following
}

export function touchScrollTarget(target: EventTarget | null, host: HTMLElement): Element {
  let element = target instanceof Element ? target : null
  while (element && element !== host) {
    if (/^(auto|scroll|overlay)$/.test(getComputedStyle(element).overflowY)) return element
    element = element.parentElement
  }
  return host
}

export function outerScrollReceives(target: EventTarget | null, host: HTMLElement, delta: number): boolean {
  let element = target instanceof Element ? target : null
  while (element && element !== host) {
    const style = getComputedStyle(element)
    if (/^(auto|scroll|overlay)$/.test(style.overflowY)) {
      if (delta < 0 ? element.scrollTop > 0 : element.scrollTop < element.scrollHeight - element.clientHeight) return false
      if (style.overscrollBehaviorY === 'contain' || style.overscrollBehaviorY === 'none') return false
    }
    element = element.parentElement
  }
  return element === host
}

export function visibleOutputPosition(events: readonly SessionEvent[]) {
  let seq = 0
  let boundary = 0
  for (const event of events) {
    if (event.type === 'turn_started' || event.type === 'model_request_started' || event.type === 'model_retry_scheduled'
      || event.type === 'execution_activity_changed' || event.type === 'workspace_execution_waiting' || event.type === 'workspace_execution_acquired'
      || event.type === 'turn_finished' || event.type === 'turn_failed' || event.type === 'turn_cancelled') boundary = event.seq
    if ((event.type === 'assistant_message_delta' || event.type === 'assistant_reasoning_delta') && event.delta) seq = event.seq
    if (event.type === 'assistant_message') {
      const response = event.response as { content?: string; reasoning_content?: string; tool_calls?: unknown[] } | undefined
      if (response?.content || response?.reasoning_content || response?.tool_calls?.length) seq = event.seq
    }
    if (event.type === 'tool_call_started' || event.type === 'tool_call_finished'
      || event.type === 'code_dispatch_started' || event.type === 'code_dispatch_finished') seq = event.seq
  }
  return { seq, afterBoundary: seq > boundary }
}
