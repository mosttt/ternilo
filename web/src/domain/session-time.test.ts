import { describe, expect, it } from 'vitest'
import { sessionAbsoluteTime, sessionRelativeTime } from './session-time'

describe('session time labels', () => {
  const now = Date.UTC(2026, 7, 28, 8, 0, 0)

  it('uses compact relative time buckets', () => {
    expect(sessionRelativeTime(now - 15_000, now)).toBe('刚刚')
    expect(sessionRelativeTime(now - 5 * 60_000, now)).toBe('5分钟')
    expect(sessionRelativeTime(now - 3 * 3_600_000, now)).toBe('3小时')
    expect(sessionRelativeTime(now - 4 * 86_400_000, now)).toBe('4天')
    expect(sessionRelativeTime(now - 62 * 86_400_000, now)).toBe('2个月')
    expect(sessionRelativeTime(now - 800 * 86_400_000, now)).toBe('2年')
  })

  it('retains a complete absolute timestamp for hover and assistive copy', () => {
    expect(sessionAbsoluteTime(now)).toMatch(/2026/)
    expect(sessionAbsoluteTime(now)).toMatch(/08/)
  })
})
