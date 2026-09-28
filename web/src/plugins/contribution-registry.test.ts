import { describe, expect, it, vi } from 'vitest'
import { ContributionRegistry } from './contribution-registry'

interface Entry { id: string; order: number }

describe('ContributionRegistry', () => {
  it('publishes stable ordered snapshots and disposes one registration once', () => {
    const registry = new ContributionRegistry<Entry>(entry => entry.id, (a, b) => a.order - b.order)
    const changed = vi.fn()
    const unsubscribe = registry.subscribe(changed)
    const firstEmpty = registry.getSnapshot()
    expect(registry.getSnapshot()).toBe(firstEmpty)

    const disposeLater = registry.register({ id: 'later', order: 20 })
    const disposeFirst = registry.register({ id: 'first', order: 10 })
    expect(registry.getSnapshot().map(entry => entry.id)).toEqual(['first', 'later'])
    expect(changed).toHaveBeenCalledTimes(2)

    disposeLater()
    disposeLater()
    expect(registry.getSnapshot().map(entry => entry.id)).toEqual(['first'])
    expect(changed).toHaveBeenCalledTimes(3)

    unsubscribe()
    disposeFirst()
    expect(changed).toHaveBeenCalledTimes(3)
  })

  it('rejects empty and duplicate keys without changing the live snapshot', () => {
    const registry = new ContributionRegistry<Entry>(entry => entry.id)
    registry.register({ id: 'chat', order: 1 })
    const snapshot = registry.getSnapshot()

    expect(() => registry.register({ id: 'chat', order: 2 })).toThrow(/already registered/)
    expect(() => registry.register({ id: '', order: 3 })).toThrow(/must not be empty/)
    expect(registry.getSnapshot()).toBe(snapshot)
  })
})
