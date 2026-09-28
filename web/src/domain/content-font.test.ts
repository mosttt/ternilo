import { describe, expect, it, vi } from 'vitest'
import {
  applyContentFontSize,
  contentFontStorageKey,
  readContentFontSize,
  writeContentFontSize,
} from './content-font'

describe('content font preference', () => {
  it('reads supported sizes and falls back to the default', () => {
    expect(readContentFontSize({ getItem: () => '16' })).toBe(16)
    expect(readContentFontSize({ getItem: () => '15' })).toBe(14)
    expect(readContentFontSize({ getItem: () => null })).toBe(14)
  })

  it('persists and applies the selected size', () => {
    const setItem = vi.fn()
    writeContentFontSize({ setItem }, 18)
    expect(setItem).toHaveBeenCalledWith(contentFontStorageKey, '18')
    expect(document.documentElement.style.getPropertyValue('--ternilo-content-font-size')).toBe('18px')
    applyContentFontSize(14)
  })
})
