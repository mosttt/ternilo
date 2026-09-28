import type { SessionEvent } from '@/types'

export interface SessionEventBuffer {
  sessionId: string | null
  events: SessionEvent[]
}

export function createSessionEventBuffer(sessionId: string | null): SessionEventBuffer {
  return { sessionId, events: [] }
}

export function mergeSessionEventBuffer(
  current: SessionEventBuffer,
  sessionId: string,
  incoming: SessionEvent[],
): SessionEventBuffer {
  if (current.sessionId !== sessionId || incoming.length === 0) return current
  let previousSeq = current.events.at(-1)?.seq ?? -1
  const appendOnly = incoming.every(event => {
    const ordered = event.seq > previousSeq
    previousSeq = event.seq
    return ordered
  })
  if (appendOnly) return { sessionId, events: current.events.concat(incoming) }
  const merged = new Map(current.events.map(event => [event.seq, event]))
  incoming.forEach(event => merged.set(event.seq, event))
  return { sessionId, events: [...merged.values()].sort((left, right) => left.seq - right.seq) }
}
