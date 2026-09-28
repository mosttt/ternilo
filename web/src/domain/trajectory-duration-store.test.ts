import { describe, expect, it, vi } from 'vitest'
import { readTrajectoryDurationMode, TRAJECTORY_DURATION_STORAGE_KEY, writeTrajectoryDurationMode } from './trajectory-duration-store'

describe('trajectory duration preference', () => {
  it('defaults to idle-compressed duration and persists actual time explicitly', () => {
    expect(readTrajectoryDurationMode(undefined)).toBe('duration')
    expect(readTrajectoryDurationMode({ getItem: () => null })).toBe('duration')
    expect(readTrajectoryDurationMode({ getItem: () => 'actual' })).toBe('actual')
    const setItem = vi.fn()
    writeTrajectoryDurationMode({ setItem }, 'actual')
    expect(setItem).toHaveBeenCalledWith(TRAJECTORY_DURATION_STORAGE_KEY, 'actual')
  })
})
