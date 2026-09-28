import { act, useState } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { openChoiceSelect, selectChoice, setupChoiceSelect } from '@/test/choice-select'
import { Select } from './field'

let host: HTMLDivElement
let root: Root
beforeEach(() => {
  vi.stubGlobal('IS_REACT_ACT_ENVIRONMENT', true)
  setupChoiceSelect()
  host = document.createElement('div')
  document.body.append(host)
  root = createRoot(host)
})
afterEach(() => { act(() => root.unmount()); host.remove(); vi.restoreAllMocks(); vi.unstubAllGlobals() })

it('supports clearing a filter while preserving required validation and unencoded form values', async () => {
  function Form() {
    const [value, setValue] = useState('')
    return <form><label htmlFor="model">Model</label><Select id="model" name="model" required value={value} onValueChange={setValue}><option value="">Choose a model</option><option value="model-a">Model A</option><option disabled value="unavailable">Unavailable</option></Select></form>
  }
  await act(async () => root.render(<Form />))
  const form = host.querySelector('form')!
  const control = host.querySelector<HTMLElement>('[role="combobox"]')!
  expect(form.checkValidity()).toBe(false)
  await selectChoice(control, 'model-a')
  expect(form.checkValidity()).toBe(true)
  expect(new FormData(form).get('model')).toBe('model-a')
  await selectChoice(control, '')
  expect(control.textContent).toBe('Choose a model')
  expect(new FormData(form).get('model')).toBe('')
  expect(form.checkValidity()).toBe(false)
  await openChoiceSelect(control)
  expect(document.querySelector('[data-choice-option="unavailable"]')?.getAttribute('aria-disabled')).toBe('true')
})

it('does not change a controlled choice when form fields or options are mounted', async () => {
  const changed = vi.fn()
  await act(async () => root.render(<form><Select value="enabled" onValueChange={changed}><option value="enabled">Enabled</option></Select></form>))
  await act(async () => root.render(<form><Select value="enabled" onValueChange={changed}><option value="enabled">Enabled</option><option value="disabled">Disabled</option></Select><input name="new-field" /></form>))
  expect(changed).not.toHaveBeenCalled()
  expect(host.querySelector('[role="combobox"]')?.textContent).toBe('Enabled')
})
