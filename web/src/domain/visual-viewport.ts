const VIEWPORT_HEIGHT = '--ternilo-visual-viewport-height'
const VIEWPORT_TOP = '--ternilo-visual-viewport-top'

export function installVisualViewportVariables(): () => void {
  const viewport = window.visualViewport
  const root = document.documentElement
  const sync = () => {
    const useVisualViewport = viewport !== null && Math.abs(viewport.scale - 1) < 0.01
    const height = useVisualViewport ? viewport.height : window.innerHeight
    const top = useVisualViewport ? viewport.offsetTop : 0
    root.style.setProperty(VIEWPORT_HEIGHT, `${Math.max(1, Math.round(height))}px`)
    root.style.setProperty(VIEWPORT_TOP, `${Math.max(0, Math.round(top))}px`)
  }

  sync()
  window.addEventListener('resize', sync)
  viewport?.addEventListener('resize', sync)
  viewport?.addEventListener('scroll', sync)

  return () => {
    window.removeEventListener('resize', sync)
    viewport?.removeEventListener('resize', sync)
    viewport?.removeEventListener('scroll', sync)
    root.style.removeProperty(VIEWPORT_HEIGHT)
    root.style.removeProperty(VIEWPORT_TOP)
  }
}
