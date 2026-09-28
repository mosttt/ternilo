import { describe, expect, it } from 'vitest'
import { updateTrajectoryFollow } from './trajectory-follow'

describe('trajectory pinned tail', () => {
  it('follows layout growth while a reader remains pinned', () => {
    expect(updateTrajectoryFollow(
      { pinned: true, observedScrollTop: 300 },
      { scrollTop: 300, scrollHeight: 900, clientHeight: 400, readerIntent: false },
    )).toEqual({ pinned: true, observedScrollTop: 300, followTail: true })
  })

  it('freezes when the reader moves upward or explicitly scrolls', () => {
    expect(updateTrajectoryFollow(
      { pinned: true, observedScrollTop: 300 },
      { scrollTop: 250, scrollHeight: 900, clientHeight: 400, readerIntent: true },
    )).toEqual({ pinned: false, observedScrollTop: 250, followTail: false })
  })

  it('stays frozen during later growth until the reader returns to the bottom', () => {
    const frozen = updateTrajectoryFollow(
      { pinned: false, observedScrollTop: 250 },
      { scrollTop: 250, scrollHeight: 1_000, clientHeight: 400, readerIntent: false },
    )
    expect(frozen.pinned).toBe(false)
    expect(frozen.followTail).toBe(false)
    expect(updateTrajectoryFollow(
      frozen,
      { scrollTop: 600, scrollHeight: 1_000, clientHeight: 400, readerIntent: true },
    ).pinned).toBe(true)
  })
})
