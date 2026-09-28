import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { LocaleProvider } from '@/i18n/provider'
import { SessionModelLimitDialog } from './session-model-limit-dialog'

let host: HTMLDivElement
let root: Root
beforeEach(() => {
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true
  host = document.createElement('div'); document.body.append(host); root = createRoot(host)
})
afterEach(() => { act(() => root.unmount()); host.remove() })
function fill(value: string) {
  const input = document.querySelector('input')!
  act(() => {
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')!.set!.call(input, value)
    input.dispatchEvent(new Event('input', { bubbles: true }))
  })
}
function saveButton() { return [...document.querySelectorAll('button')].find(button => button.textContent === '保存')! }

it('rejects invalid limits and retains the entered value after an API failure so the user can retry', async () => {
  const onSave = vi.fn().mockRejectedValueOnce(new Error('Budget is unavailable')).mockResolvedValue(undefined)
  const onClose = vi.fn()
  act(() => root.render(<LocaleProvider><SessionModelLimitDialog limit={32768} onSave={onSave} onClose={onClose} /></LocaleProvider>))
  expect(document.querySelector('input')!.value).toBe('32768')
  for (const invalid of ['', '0', '-1', '1.5', '9007199254740992']) {
    fill(invalid)
    expect(saveButton().disabled).toBe(true)
  }
  fill('262144')
  await act(async () => saveButton().click())
  expect(onSave).toHaveBeenCalledExactlyOnceWith(262144)
  expect(document.querySelector('[role="alert"]')?.textContent).toBe('Budget is unavailable')
  expect(document.querySelector('input')!.value).toBe('262144')
  expect(onClose).not.toHaveBeenCalled()
  await act(async () => saveButton().click())
  expect(onSave).toHaveBeenCalledTimes(2)
  expect(onClose).toHaveBeenCalledOnce()
})
