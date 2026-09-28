import type { SessionEvent } from '@/types'

export function regenerationTarget(event: SessionEvent): number | undefined {
  return event.type === 'user_message' && event.source?.kind === 'submission'
    ? event.source.regenerate_from ?? undefined : undefined
}

export function conversationEvents(events: SessionEvent[]): SessionEvent[] {
  if (!events.some(event => regenerationTarget(event) !== undefined)) return events
  const active: SessionEvent[] = []
  const discarded = new Set<string>()
  let changed = false
  for (const event of events) {
    const target = regenerationTarget(event)
    const original = target === undefined ? undefined : active.find(entry => entry.seq === target)
    if (original) {
      const prefix = active.some(entry => entry.run_id === original.run_id && entry.seq < original.seq && entry.type === 'user_message')
      const boundary = prefix ? active.indexOf(original) : active.findIndex(entry => entry.run_id === original.run_id)
      const currentStart = active.findIndex(entry => entry.run_id === event.run_id)
      const current = currentStart === -1 ? [] : active.splice(currentStart)
      for (const removed of active.splice(boundary)) discarded.add(removed.run_id)
      active.push(...current)
      changed = true
    }
    if (!discarded.has(event.run_id)) active.push(event)
  }
  return changed ? active : events
}
