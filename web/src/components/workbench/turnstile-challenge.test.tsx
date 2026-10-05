import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { LocaleProvider } from '@/i18n/provider'
import { TurnstileChallenge } from './turnstile-challenge'

type Options = Parameters<NonNullable<Window['turnstile']>['render']>[1]
let root: Root, host: HTMLDivElement, resize: () => void, width: number
let options: Options[]
const onToken = vi.fn()
beforeEach(() => {
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true
  width = 280
  options = []
  host = document.createElement('div')
  document.body.append(host)
  root = createRoot(host)
  vi.spyOn(HTMLElement.prototype, 'clientWidth', 'get').mockImplementation(() => width)
  vi.stubGlobal('ResizeObserver', class {
    constructor(callback: () => void) { resize = callback }
    observe() {}
    disconnect() {}
  })
  window.turnstile = {
    render(_container, settings) { options.push(settings); return `widget-${options.length}` },
    remove: vi.fn(),
  }
})
afterEach(() => {
  act(() => root.unmount())
  host.remove()
  delete window.turnstile
  vi.restoreAllMocks()
  vi.unstubAllGlobals()
  onToken.mockClear()
})
async function settle(action: () => void) { await act(async () => { action(); await Promise.resolve() }) }

it('keeps a new mobile widget token when retired widget callbacks arrive and surfaces current provider errors', async () => {
  await settle(() => root.render(<LocaleProvider><TurnstileChallenge siteKey="fixture-site" action="login" attempt={0} onToken={onToken} /></LocaleProvider>))
  const retired = options[0]
  expect(retired.size).toBe('compact')
  await settle(() => { width = 350; resize() })
  const current = options[1]
  expect(current.size).toBe('flexible')
  await settle(() => current.callback('fresh-token'))
  await settle(() => { retired['error-callback']('600010'); retired['expired-callback'](); retired.callback('retired-token') })
  expect(onToken).toHaveBeenLastCalledWith('fresh-token')
  expect(document.querySelector('[role="alert"]')).toBeNull()
  await settle(() => { expect(current['error-callback']('600010')).toBe(true) })
  expect(onToken).toHaveBeenLastCalledWith('')
  expect(document.querySelector('[role="alert"]')?.textContent).toContain('600010')
  await settle(() => [...host.querySelectorAll('button')].find(button => button.textContent === '重新验证')!.click())
  const retried = options[2]
  expect(retried.retry).toBe('never')
  expect(retried['refresh-expired']).toBe('manual')
  await settle(() => retried.callback('retry-token'))
  await settle(() => current['timeout-callback']())
  expect(onToken).toHaveBeenLastCalledWith('retry-token')
  expect(document.querySelector('[role="alert"]')).toBeNull()
})
