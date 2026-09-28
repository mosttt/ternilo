import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { LocaleProvider } from '@/i18n/provider'
import type { ProviderModel } from '@/types'
import { modelDraft, ProviderModelEditor } from './provider-model-editor'

const candidates: ProviderModel[] = [
  { id: 'vendor/alpha', display_name: 'Quick Alpha', settings: { mode: 'automatic', upstream: { context_window: 200_000 }, overrides: {} } },
  { id: 'vendor/beta', display_name: '中文模型', settings: { mode: 'automatic', upstream: { context_window: 300_000 }, overrides: {} } },
  { id: 'vendor/Gamma-9', display_name: 'Large model', settings: { mode: 'inherit' } },
]
const original = modelDraft({ id: 'vendor/alpha', display_name: 'My model name', settings: { mode: 'automatic', upstream: { context_window: 100_000 }, overrides: { context_window: 64_000 } } })
const onDiscover = vi.fn(async () => candidates)
const onChange = vi.fn()
let root: Root
let host: HTMLDivElement
let storage: Map<string, string>

beforeEach(() => {
  vi.stubGlobal('IS_REACT_ACT_ENVIRONMENT', true)
  storage = new Map([['ternilo.locale', 'zh']])
  vi.stubGlobal('localStorage', { getItem: (key: string) => storage.get(key) ?? null, setItem: (key: string, value: string) => storage.set(key, value) })
  onDiscover.mockClear(); onChange.mockClear()
  host = document.createElement('div'); document.body.append(host); root = createRoot(host)
})
afterEach(() => { act(() => root.unmount()); host.remove(); vi.restoreAllMocks(); vi.unstubAllGlobals() })

function button(label: string) {
  const found = [...document.querySelectorAll<HTMLButtonElement>('button')].find(element => element.textContent === label)
  expect(found, label).toBeDefined()
  return found!
}
async function click(label: string) { await act(async () => button(label).click()) }
function dialog() { return document.querySelector<HTMLElement>('[role="dialog"]')! }
function rows() { return [...dialog().querySelectorAll<HTMLLabelElement>('[data-discovered-models] label')] }
function selected() { return rows().filter(row => row.querySelector<HTMLInputElement>('input')!.checked).map(row => row.querySelector('code')!.textContent) }
async function search(value: string) {
  const input = dialog().querySelector<HTMLInputElement>('input[type="search"]')!
  await act(async () => {
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')!.set!.call(input, value)
    input.dispatchEvent(new Event('input', { bubbles: true }))
  })
  return input
}
async function open(locale = 'zh') {
  storage.set('ternilo.locale', locale)
  await act(async () => root.render(<LocaleProvider><ProviderModelEditor defaults={{ contextWindow: '128K', maxOutputTokens: '16K' }} models={[original]} onDiscover={onDiscover} onChange={onChange} /></LocaleProvider>))
  await click(locale === 'zh' ? '获取可用模型' : 'Fetch available models')
}

it('filters by display name or ID without case sensitivity, refetching or applying while typing', async () => {
  await open()
  expect(rows()).toHaveLength(3)
  await search('  中文  ')
  expect(rows().map(row => row.textContent)).toEqual(['中文模型vendor/beta'])
  await search(' GAMMA-9 ')
  expect(rows().map(row => row.textContent)).toEqual(['Large modelvendor/Gamma-9'])
  await search(' quick ALPHA ')
  expect(rows().map(row => row.textContent)).toEqual(['Quick Alphavendor/alpha'])
  expect(dialog().querySelector('[role="status"]')!.textContent).toBe('显示 1 / 3 · 已选 3')
  const keyboard = new KeyboardEvent('keydown', { key: 'Enter', bubbles: true, cancelable: true })
  await act(async () => { dialog().querySelector('input[type="search"]')!.dispatchEvent(keyboard) })
  expect(keyboard.defaultPrevented).toBe(true)
  expect(onDiscover).toHaveBeenCalledOnce()
  expect(onChange).not.toHaveBeenCalled()
})

it('only toggles matching results and preserves hidden choices and manual settings when applying', async () => {
  await open()
  await click('取消全选')
  await search('alpha')
  await click('全选结果')
  await search('beta')
  await click('全选结果')
  await search('alpha')
  expect(selected()).toEqual(['vendor/alpha'])
  await click('取消结果选择')
  await search('')
  expect(selected()).toEqual(['vendor/beta'])
  expect(dialog().querySelector('[role="status"]')!.textContent).toBe('显示 3 / 3 · 已选 1')
  await click('应用所选')
  expect(onChange).toHaveBeenCalledOnce()
  const [models] = onChange.mock.calls[0]
  expect(models.map((model: { id: string }) => model.id)).toEqual(['vendor/alpha', 'vendor/beta'])
  expect(models[0]).toEqual(original)
  expect(models[1].upstream.context_window).toBe(300_000)
  expect(document.querySelector('[role="dialog"]')).toBeNull()
})

it('explains no matches, keeps hidden picks, and resets the search on a new discovery', async () => {
  await open()
  await search('not-a-model')
  expect(rows()).toHaveLength(0)
  expect(dialog().textContent).toContain('没有匹配的模型')
  expect(dialog().querySelector('[role="status"]')!.textContent).toBe('显示 0 / 3 · 已选 3')
  expect(button('全选结果').disabled).toBe(true)
  await click('取消')
  expect(onChange).not.toHaveBeenCalled()
  await click('获取可用模型')
  expect(dialog().querySelector<HTMLInputElement>('input[type="search"]')!.value).toBe('')
  expect(rows()).toHaveLength(3)
  expect(selected()).toHaveLength(3)
})

it('provides English search guidance and resets list scroll when the filter changes', async () => {
  await open('en')
  const list = dialog().querySelector<HTMLElement>('[data-discovered-models]')!
  list.scrollTop = 400
  const input = await search('none-found')
  expect(input.getAttribute('aria-label')).toBe('Search model name or ID…')
  expect(dialog().textContent).toContain('No matching models. Try another search.')
  expect(dialog().querySelector('[role="status"]')!.textContent).toBe('Showing 0 / 3 · Selected 3')
  expect(list.scrollTop).toBe(0)
})
