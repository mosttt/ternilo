import { afterEach, describe, expect, it } from 'vitest'
import { installVisualViewportVariables } from './visual-viewport'

const originalViewport = Object.getOwnPropertyDescriptor(window, 'visualViewport')
const originalInnerHeight = Object.getOwnPropertyDescriptor(window, 'innerHeight')
type MutableVisualViewport = { -readonly [Key in keyof VisualViewport]: VisualViewport[Key] }

function setVisualViewport(value: VisualViewport | null) {
  Object.defineProperty(window, 'visualViewport', { configurable: true, value })
}

function fakeViewport(metrics: { height: number; offsetTop: number; scale: number }) {
  return Object.assign(new EventTarget(), metrics) as MutableVisualViewport
}

afterEach(() => {
  document.documentElement.style.removeProperty('--ternilo-visual-viewport-height')
  document.documentElement.style.removeProperty('--ternilo-visual-viewport-top')
  if (originalViewport) Object.defineProperty(window, 'visualViewport', originalViewport)
  else delete (window as unknown as { visualViewport?: VisualViewport }).visualViewport
  if (originalInnerHeight) Object.defineProperty(window, 'innerHeight', originalInnerHeight)
})

describe('visual viewport variables', () => {
  it('tracks soft-keyboard geometry and releases its listeners', () => {
    const viewport = fakeViewport({ height: 430, offsetTop: 12, scale: 1 })
    setVisualViewport(viewport)
    const dispose = installVisualViewportVariables()
    const root = document.documentElement.style

    expect(root.getPropertyValue('--ternilo-visual-viewport-height')).toBe('430px')
    expect(root.getPropertyValue('--ternilo-visual-viewport-top')).toBe('12px')

    viewport.height = 844
    viewport.offsetTop = 0
    viewport.dispatchEvent(new Event('resize'))
    expect(root.getPropertyValue('--ternilo-visual-viewport-height')).toBe('844px')
    expect(root.getPropertyValue('--ternilo-visual-viewport-top')).toBe('0px')

    dispose()
    expect(root.getPropertyValue('--ternilo-visual-viewport-height')).toBe('')
    expect(root.getPropertyValue('--ternilo-visual-viewport-top')).toBe('')
  })

  it('does not reflow the app during pinch zoom', () => {
    const viewport = fakeViewport({ height: 300, offsetTop: 80, scale: 2 })
    setVisualViewport(viewport)
    Object.defineProperty(window, 'innerHeight', { configurable: true, value: 844 })
    const dispose = installVisualViewportVariables()

    expect(document.documentElement.style.getPropertyValue('--ternilo-visual-viewport-height')).toBe('844px')
    expect(document.documentElement.style.getPropertyValue('--ternilo-visual-viewport-top')).toBe('0px')
    dispose()
  })
})
