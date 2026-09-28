import { act } from 'react'
import { expect, vi } from 'vitest'

export function setupChoiceSelect() {
  vi.stubGlobal('ResizeObserver', class { observe() {} unobserve() {} disconnect() {} })
  if (!HTMLElement.prototype.scrollIntoView) Object.defineProperty(HTMLElement.prototype, 'scrollIntoView', { configurable: true, writable: true, value() {} })
  vi.spyOn(HTMLElement.prototype, 'scrollIntoView').mockImplementation(() => {})
}

export async function openChoiceSelect(trigger: HTMLElement) {
  await act(async () => {
    trigger.focus()
    trigger.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowDown', bubbles: true, cancelable: true }))
  })
}

export async function selectChoice(trigger: HTMLElement, value: string) {
  await openChoiceSelect(trigger)
  const option = [...document.querySelectorAll<HTMLElement>('[role="option"][data-choice-option]')].find(item => item.dataset.choiceOption === value)
  expect(option).toBeDefined()
  await act(async () => {
    option!.focus()
    option!.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true, cancelable: true }))
  })
}
