import { describe, expect, it, vi } from 'vitest'
import { randomUuid } from './random-id'

describe('randomUuid', () => {
  it('uses the native implementation when the browser exposes it', () => {
    const native = '01234567-89ab-4cde-8fab-0123456789ab'
    const randomUUID = vi.fn(() => native)
    const getRandomValues = vi.fn()

    expect(randomUuid({ randomUUID, getRandomValues })).toBe(native)
    expect(randomUUID).toHaveBeenCalledOnce()
    expect(getRandomValues).not.toHaveBeenCalled()
  })

  it('creates an RFC 4122 version 4 identifier on an insecure LAN origin', () => {
    const getRandomValues = <T extends ArrayBufferView | null>(array: T): T => {
      const bytes = array as Uint8Array
      bytes.set(Array.from({ length: 16 }, (_, index) => index))
      return array
    }

    expect(randomUuid({ getRandomValues })).toBe('00010203-0405-4607-8809-0a0b0c0d0e0f')
  })
})
