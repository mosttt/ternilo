import { describe, expect, it, vi } from 'vitest'
import { createActionStore } from './action-store'

describe('action store', () => {
  it('keeps snapshots and public methods reference-stable until an action changes state', () => {
    const store = createActionStore({ count: 0 })
    const getSnapshot = store.getSnapshot
    const subscribe = store.subscribe
    const first = store.getSnapshot()
    const listener = vi.fn()
    const unsubscribe = store.subscribe(listener)

    store.dispatch(current => current)
    expect(store.getSnapshot()).toBe(first)
    expect(listener).not.toHaveBeenCalled()

    store.dispatch(current => ({ count: current.count + 1 }))
    const second = store.getSnapshot()
    expect(second).not.toBe(first)
    expect(second).toEqual({ count: 1 })
    expect(listener).toHaveBeenCalledOnce()
    expect(store.getSnapshot()).toBe(second)
    expect(store.getSnapshot).toBe(getSnapshot)
    expect(store.subscribe).toBe(subscribe)

    unsubscribe()
    store.dispatch(current => ({ count: current.count + 1 }))
    expect(listener).toHaveBeenCalledOnce()
  })
})
