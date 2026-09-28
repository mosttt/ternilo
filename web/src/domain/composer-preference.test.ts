import { describe, expect, it } from 'vitest'
import {
  DEFAULT_BUSY_ENTER_BEHAVIOR,
  readBusyEnterBehavior,
  resolveComposerDelivery,
} from './composer-preference'

describe('composer submission preference', () => {
  it('defaults unknown persisted values to queue', () => {
    expect(readBusyEnterBehavior({ getItem: () => null })).toBe(DEFAULT_BUSY_ENTER_BEHAVIOR)
    expect(readBusyEnterBehavior({ getItem: () => 'old-value' })).toBe('queue')
    expect(readBusyEnterBehavior({ getItem: () => 'steer' })).toBe('steer')
  })

  it('uses the configured busy Enter mode and reverses it for Cmd/Ctrl+Enter', () => {
    expect(resolveComposerDelivery(false, true, 'steer')).toBe('queue')
    expect(resolveComposerDelivery(true, false, 'queue')).toBe('queue')
    expect(resolveComposerDelivery(true, true, 'queue')).toBe('steer')
    expect(resolveComposerDelivery(true, false, 'steer')).toBe('steer')
    expect(resolveComposerDelivery(true, true, 'steer')).toBe('queue')
  })
})
