import type { SessionEvent } from '@/types'

/** Canonical event fields consumed by Chat lifecycle projections. */
export interface ModelRetryScheduledEvent extends SessionEvent {
  type: 'model_retry_scheduled'
  retry_id: string
  retry: number
  max_retries: number | null
  delay_ms: number
  failure: { message: string; code?: string }
}

export interface ModelRetryStartedEvent extends SessionEvent {
  type: 'model_retry_started'
  retry_id: string
  retry: number
}

export interface ModelRetryCancelledEvent extends SessionEvent {
  type: 'model_retry_cancelled'
  retry_id: string
  retry: number
}

export interface CommandStartedEvent extends SessionEvent {
  type: 'command_started'
  command_id: string
  command_name: string
  arguments?: unknown
}

export interface CommandFinishedEvent extends SessionEvent {
  type: 'command_finished'
  command_id: string
  outcome: {
    kind: 'success' | 'error'
    text?: string
    code?: string
    parameters?: Record<string, string>
  }
}

export interface ContextCompactionStartedEvent extends SessionEvent {
  type: 'context_compaction_started'
  compaction_id: string
  automatic: boolean
  source_command_id?: string
  turn: number
}

export interface ChatRetryLifecycle {
  id: string
  anchor: ModelRetryScheduledEvent
  event: ModelRetryScheduledEvent
  state: 'scheduled' | 'started' | 'cancelled'
}

export interface ChatCommandLifecycle {
  id: string
  event: CommandStartedEvent | CommandFinishedEvent
  name: string | null
  arguments: unknown
  outcome: CommandFinishedEvent['outcome'] | null
}

export function retryLifecycle(events: SessionEvent[]) {
  const values = new Map<string, ChatRetryLifecycle>()
  for (const raw of events) {
    if (raw.type === 'model_retry_scheduled') {
      const event = raw as ModelRetryScheduledEvent
      const current = values.get(event.retry_id)
      values.set(event.retry_id, {
        id: event.retry_id,
        anchor: current?.anchor ?? event,
        event,
        state: 'scheduled',
      })
      continue
    }
    if (raw.type !== 'model_retry_started' && raw.type !== 'model_retry_cancelled') continue
    const event = raw as ModelRetryStartedEvent | ModelRetryCancelledEvent
    const current = values.get(event.retry_id)
    if (current && current.event.retry === event.retry) {
      values.set(event.retry_id, {
        ...current,
        state: event.type === 'model_retry_started' ? 'started' : 'cancelled',
      })
    }
  }
  return values
}

export function commandLifecycle(events: SessionEvent[]) {
  const values = new Map<string, ChatCommandLifecycle>()
  for (const raw of events) {
    if (raw.type === 'command_started') {
      const event = raw as CommandStartedEvent
      values.set(event.command_id, {
        id: event.command_id,
        event,
        name: event.command_name,
        arguments: event.arguments ?? null,
        outcome: null,
      })
      continue
    }
    if (raw.type !== 'command_finished') continue
    const event = raw as CommandFinishedEvent
    const current = values.get(event.command_id)
    values.set(event.command_id, {
      id: event.command_id,
      event: current?.event ?? event,
      name: current?.name ?? null,
      arguments: current?.arguments ?? null,
      outcome: event.outcome,
    })
  }
  return values
}

export function turnReachedMaxTokens(event: SessionEvent) {
  return event.type === 'turn_finished' && event.finish_reason === 'max_tokens'
}
