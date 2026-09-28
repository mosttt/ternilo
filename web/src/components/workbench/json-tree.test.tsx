import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { Translate } from '@/i18n/runtime'
import { JsonInspector } from './json-tree'

const messages: Record<string, string> = {
  'details.json.treeLabel': '{label} JSON tree',
  'details.json.expandNode': 'Expand JSON node {path}',
  'details.json.collapseNode': 'Collapse JSON node {path}',
  'details.json.showRaw': 'View raw JSON',
  'details.json.showTree': 'View structured JSON',
  'details.json.copyFull': 'Copy full JSON',
  'details.json.copied': 'Full JSON copied',
  'details.json.copyFailed': 'Copy failed',
}

const t = ((key: string, params?: Record<string, unknown>) => {
  const template = messages[key] ?? key
  return template.replace(/\{(\w+)\}/g, (match, name: string) => params && Object.hasOwn(params, name) ? String(params[name]) : match)
}) as Translate<'chat'>

let host: HTMLDivElement
let root: Root
let writeText: ReturnType<typeof vi.fn>

beforeEach(() => {
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true
  host = document.createElement('div')
  document.body.append(host)
  root = createRoot(host)
  writeText = vi.fn().mockResolvedValue(undefined)
  Object.defineProperty(navigator, 'clipboard', { configurable: true, value: { writeText } })
})

afterEach(() => {
  act(() => root.unmount())
  host.remove()
  delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
})

describe('Details JsonInspector', () => {
  it('renders bounded previews and keyboard-operable nested nodes', () => {
    act(() => root.render(<JsonInspector
      value={{ nested: { answer: 42 }, list: ['alpha', 'beta'], wide: { a: 1, b: 2, c: 3, d: 4, e: 5 } }}
      label="Input"
      t={t}
    />))

    const tree = host.querySelector<HTMLElement>('[role="tree"]')!
    const viewport = host.querySelector<HTMLElement>('[data-json-tree]')!
    expect(viewport.tabIndex).toBe(0)
    expect(viewport.getAttribute('aria-label')).toBe('Input JSON tree')
    expect(tree.getAttribute('aria-label')).toBe('Input JSON tree')
    expect(tree.textContent).toContain('nested:{answer: 42}')
    expect(tree.textContent).toContain('wide:{a: 1, b: 2, c: 3, d: 4, …}')

    let expanders = [...tree.querySelectorAll<HTMLButtonElement>('[data-json-expander]')]
    expect(expanders).toHaveLength(3)
    expect(expanders[0]?.tabIndex).toBe(0)
    expect(expanders[1]?.tabIndex).toBe(-1)

    act(() => expanders[0]?.click())
    expect(expanders[0]?.getAttribute('aria-expanded')).toBe('true')
    expect(tree.textContent).toContain('answer:42')

    act(() => {
      expanders[0]?.focus()
      expanders[0]?.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowDown', bubbles: true }))
    })
    expanders = [...tree.querySelectorAll<HTMLButtonElement>('[data-json-expander]')]
    expect(document.activeElement).toBe(expanders[1])
    expect(expanders[1]?.tabIndex).toBe(0)

    act(() => expanders[1]?.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowRight', bubbles: true })))
    expect(expanders[1]?.getAttribute('aria-expanded')).toBe('true')
    act(() => expanders[1]?.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowLeft', bubbles: true })))
    expect(expanders[1]?.getAttribute('aria-expanded')).toBe('false')
  })

  it('shows and copies the complete raw JSON without truncating the tree value', async () => {
    const value = { nested: { answer: 42 }, list: ['alpha', 'beta'] }
    act(() => root.render(<JsonInspector value={value} label="Input" t={t} />))

    const rawToggle = [...host.querySelectorAll('button')].find(button => button.textContent?.includes('View raw JSON'))!
    act(() => rawToggle.click())
    const raw = host.querySelector<HTMLElement>('[data-json-raw]')!
    expect(raw.textContent).toBe(JSON.stringify(value, null, 2))

    const streamedValue = { ...value, streamed: 'next delta' }
    act(() => root.render(<JsonInspector value={streamedValue} label="Input" t={t} />))
    expect(host.querySelector('[data-json-raw]')?.textContent).toBe(JSON.stringify(streamedValue, null, 2))
    expect(host.querySelector('[role="tree"]')).toBeNull()

    const copy = host.querySelector<HTMLButtonElement>('button[aria-label="Copy full JSON"]')!
    await act(async () => { copy.click(); await Promise.resolve() })
    expect(writeText).toHaveBeenCalledWith(JSON.stringify(streamedValue, null, 2))
    expect(host.querySelector('button[aria-label="Full JSON copied"]')).not.toBeNull()

    const treeToggle = [...host.querySelectorAll('button')].find(button => button.textContent?.includes('View structured JSON'))!
    act(() => treeToggle.click())
    expect(host.querySelector('[role="tree"]')).not.toBeNull()
  })

  it('preserves plain string bodies and exposes localized copy failure', async () => {
    writeText.mockRejectedValueOnce(new Error('denied'))
    act(() => root.render(<JsonInspector value={'plain\ntext'} label="Output" t={t} />))

    expect(host.querySelector('[role="tree"]')).toBeNull()
    expect(host.querySelector('[data-json-raw]')?.textContent).toBe('plain\ntext')
    const copy = host.querySelector<HTMLButtonElement>('button[aria-label="Copy full JSON"]')!
    await act(async () => { copy.click(); await Promise.resolve() })
    expect(writeText).toHaveBeenCalledWith('plain\ntext')
    expect(host.querySelector('button[aria-label="Copy failed"]')).not.toBeNull()
  })
})
