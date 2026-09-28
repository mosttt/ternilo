import { afterEach, describe, expect, it } from 'vitest'
import { applyThemePreference } from './theme'

afterEach(() => {
  document.documentElement.classList.remove('dark')
  document.documentElement.removeAttribute('data-theme')
  document.documentElement.style.removeProperty('color-scheme')
})

describe('applyThemePreference', () => {
  it('applies the resolved palette and browser color scheme', () => {
    applyThemePreference('dark', false)
    expect(document.documentElement.classList.contains('dark')).toBe(true)
    expect(document.documentElement.dataset.theme).toBe('dark')
    expect(document.documentElement.style.colorScheme).toBe('dark')

    applyThemePreference('light', true)
    expect(document.documentElement.classList.contains('dark')).toBe(false)
    expect(document.documentElement.style.colorScheme).toBe('light')
  })

  it('resolves the system preference each time it is applied', () => {
    applyThemePreference('system', true)
    expect(document.documentElement.classList.contains('dark')).toBe(true)

    applyThemePreference('system', false)
    expect(document.documentElement.classList.contains('dark')).toBe(false)
  })
})
