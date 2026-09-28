import { afterEach, describe, expect, it, vi } from 'vitest'
import { downloadJson } from './utils'

afterEach(() => {
  vi.useRealTimers()
  vi.restoreAllMocks()
})

describe('downloadJson', () => {
  it('clicks a mounted anchor and revokes the object URL after the click turn', () => {
    vi.useFakeTimers()
    vi.spyOn(URL, 'createObjectURL').mockReturnValue('blob:ternilo-export')
    const revoke = vi.spyOn(URL, 'revokeObjectURL').mockImplementation(() => undefined)
    let clickedWhileMounted = false
    let filename = ''
    vi.spyOn(HTMLAnchorElement.prototype, 'click').mockImplementation(function click(this: HTMLAnchorElement) {
      clickedWhileMounted = this.isConnected
      filename = this.download
    })

    downloadJson('session.json', { schema_version: 1 })

    expect(clickedWhileMounted).toBe(true)
    expect(filename).toBe('session.json')
    expect(document.querySelector('a[download="session.json"]')).toBeNull()
    expect(revoke).not.toHaveBeenCalled()
    vi.runAllTimers()
    expect(revoke).toHaveBeenCalledWith('blob:ternilo-export')
  })
})
