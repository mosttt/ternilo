export type StoreAction<Snapshot> = (current: Snapshot) => Snapshot

/**
 * A tiny external store for target-neutral application state. The snapshot is
 * cached, so repeated reads are reference-stable until an action changes it.
 */
export interface ActionStore<Snapshot> {
  getSnapshot(): Snapshot
  subscribe(listener: () => void): () => void
  dispatch(action: StoreAction<Snapshot>): void
}

export function createActionStore<Snapshot>(initialSnapshot: Snapshot): ActionStore<Snapshot> {
  let snapshot = initialSnapshot
  const listeners = new Set<() => void>()

  return {
    getSnapshot: () => snapshot,
    subscribe(listener) {
      listeners.add(listener)
      return () => listeners.delete(listener)
    },
    dispatch(action) {
      const next = action(snapshot)
      if (Object.is(next, snapshot)) return
      snapshot = next
      listeners.forEach(listener => listener())
    },
  }
}
