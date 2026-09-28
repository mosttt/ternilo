import { expect, it } from 'vitest'
import { followAfterScroll } from '@/domain/conversation-follow'

it('releases tail following on the first intentional upward pixel instead of waiting for the tail threshold', () => {
  const paused = followAfterScroll(true, -1, 1000, 999.75, true)
  expect(paused).toBe(false)
  expect(followAfterScroll(paused, null, 999.75, 999.75, true)).toBe(false)
  expect(followAfterScroll(paused, -1, 999.75, 1000, true)).toBe(false)
  expect(followAfterScroll(paused, 1, 999.75, 1000, true)).toBe(true)
})

it('keeps a paused reader in place during passive growth and only resumes a downward return to the bottom', () => {
  expect(followAfterScroll(false, null, 1000, 1010, true)).toBe(false)
  expect(followAfterScroll(false, 1, 1000, 1004, false)).toBe(false)
  expect(followAfterScroll(false, 0, 1000, 990, false)).toBe(false)
  expect(followAfterScroll(false, 0, 990, 1100, true)).toBe(true)
})
