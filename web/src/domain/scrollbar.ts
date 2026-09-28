const HIDE_DELAY_MS = 1200
const SCROLL_KEYS = new Set(['ArrowUp', 'ArrowDown', 'ArrowLeft', 'ArrowRight', 'PageUp', 'PageDown', 'Home', 'End', ' '])

export function installScrollbarActivity(): () => void {
  const timers = new Map<HTMLElement, number>()

  const show = (element: HTMLElement) => {
    window.clearTimeout(timers.get(element))
    element.setAttribute('data-scrollbar-active', '')
    timers.set(element, window.setTimeout(() => {
      element.removeAttribute('data-scrollbar-active')
      timers.delete(element)
    }, HIDE_DELAY_MS))
  }

  const activate = (event: Event) => {
    if (event instanceof KeyboardEvent && !SCROLL_KEYS.has(event.key)) return
    if (event instanceof MouseEvent && event.type === 'pointermove' && event.movementX === 0 && event.movementY === 0) return
    let element = event.target instanceof Element ? event.target : null
    while (element) {
      if (element instanceof HTMLElement && (element.scrollHeight > element.clientHeight || element.scrollWidth > element.clientWidth)) {
        const style = getComputedStyle(element)
        if ((element.scrollHeight > element.clientHeight && /^(auto|scroll)$/.test(style.overflowY))
          || (element.scrollWidth > element.clientWidth && /^(auto|scroll)$/.test(style.overflowX))) show(element)
      }
      element = element.parentElement
    }
  }

  const scroll = (event: Event) => {
    const element = event.target === document ? document.documentElement : event.target
    if (element instanceof HTMLElement && timers.has(element)) show(element)
  }
  const clear = () => {
    for (const [element, timer] of timers) {
      window.clearTimeout(timer)
      element.removeAttribute('data-scrollbar-active')
    }
    timers.clear()
  }
  const events = ['pointermove', 'pointerdown', 'wheel', 'touchmove', 'keydown']
  for (const event of events) document.addEventListener(event, activate, { capture: true, passive: true })
  document.addEventListener('scroll', scroll, { capture: true, passive: true })
  window.addEventListener('blur', clear)

  return () => {
    for (const event of events) document.removeEventListener(event, activate, true)
    document.removeEventListener('scroll', scroll, true)
    window.removeEventListener('blur', clear)
    clear()
  }
}
