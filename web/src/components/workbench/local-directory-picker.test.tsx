import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { LocaleProvider } from '@/i18n/provider'
import type { DirectoryListing } from '@/types'
import { DirectoryBrowser, type DirectoryBrowserProps } from './local-directory-picker'

const HOME = '/home/u'
const DOCS = `${HOME}/Documents`
const HARNESS = `${DOCS}/harness`

function listing(path = HOME, withFresh = false): DirectoryListing {
  if (path === HOME) return {
    path: HOME,
    home: HOME,
    crumbs: [
      { name: '/', path: '/', hidden: false },
      { name: 'home', path: '/home', hidden: false },
      { name: 'u', path: HOME, hidden: false },
    ],
    entries: [
      { name: '.config', path: `${HOME}/.config`, hidden: true },
      { name: 'Documents', path: DOCS, hidden: false },
      ...(withFresh ? [{ name: 'fresh ', path: `${HOME}/fresh `, hidden: false }] : []),
    ],
    truncated: false,
  }
  if (path === DOCS) return {
    path: DOCS,
    home: HOME,
    crumbs: [...listing(HOME).crumbs, { name: 'Documents', path: DOCS, hidden: false }],
    entries: [{ name: 'harness', path: HARNESS, hidden: false }],
    truncated: false,
  }
  if (path === HARNESS || path === `${HOME}/fresh `) return {
    path,
    home: HOME,
    crumbs: [...listing(path === HARNESS ? DOCS : HOME).crumbs, {
      name: path === HARNESS ? 'harness' : 'fresh ', path, hidden: false,
    }],
    entries: [],
    truncated: false,
  }
  throw new Error(`cannot list ${path}`)
}

let host: HTMLDivElement
let root: Root

beforeEach(() => {
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true
  host = document.createElement('div')
  document.body.append(host)
  root = createRoot(host)
})

afterEach(() => {
  act(() => root.unmount())
  host.remove()
  delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
  vi.useRealTimers()
})

function renderBrowser(overrides: Partial<DirectoryBrowserProps> = {}) {
  const listDirectory = vi.fn(async (path?: string) => listing(path ?? HOME))
  const createDirectory = vi.fn(async (path: string, name: string) => `${path}/${name}`)
  const onOpen = vi.fn(async () => {})
  const onClose = vi.fn()
  const props: DirectoryBrowserProps = {
    open: true,
    listDirectory,
    createDirectory,
    onOpen,
    onClose,
    ...overrides,
  }
  act(() => root.render(<LocaleProvider><DirectoryBrowser {...props} /></LocaleProvider>))
  return { props, listDirectory, createDirectory, onOpen, onClose }
}

async function flush() {
  await act(async () => { await Promise.resolve(); await Promise.resolve() })
}

function buttons(): HTMLButtonElement[] {
  return [...document.querySelectorAll<HTMLButtonElement>('button')]
}

function button(name: string): HTMLButtonElement {
  const match = buttons().find(item => item.getAttribute('aria-label') === name || item.textContent?.trim() === name)
  if (!match) throw new Error(`missing button ${name}`)
  return match
}

function textbox(label: string): HTMLInputElement {
  const match = [...document.querySelectorAll<HTMLInputElement>('input')]
    .find(item => item.getAttribute('aria-label') === label)
  if (!match) throw new Error(`missing textbox ${label}`)
  return match
}

function click(element: HTMLElement) {
  act(() => element.click())
}

function write(input: HTMLInputElement, value: string) {
  act(() => {
    const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')?.set
    setter?.call(input, value)
    input.dispatchEvent(new Event('input', { bubbles: true }))
  })
}

function key(element: Element, value: string) {
  act(() => element.dispatchEvent(new KeyboardEvent('keydown', { key: value, bubbles: true, cancelable: true })))
}

describe('DirectoryBrowser navigation', () => {
  it('lands a selected directory as a Miller pair and aborts a superseded child scan', async () => {
    let resolveChild!: (value: DirectoryListing) => void
    let childSignal: AbortSignal | undefined
    const listDirectory = vi.fn((path?: string, signal?: AbortSignal) => {
      if (path === DOCS) {
        childSignal = signal
        return new Promise<DirectoryListing>(resolve => { resolveChild = resolve })
      }
      return Promise.resolve(listing(path ?? HOME))
    })
    renderBrowser({ listDirectory })
    await flush()
    click(button('Documents'))
    expect(document.querySelectorAll('[role="list"]')).toHaveLength(1)
    click(button('主目录'))
    expect(childSignal?.aborted).toBe(true)
    await flush()
    expect(document.querySelectorAll('[role="list"]')).toHaveLength(1)
    act(() => resolveChild(listing(DOCS)))
    await flush()
    expect(document.querySelectorAll('[role="list"]')).toHaveLength(1)

    click(button('Documents'))
    act(() => resolveChild(listing(DOCS)))
    await flush()
    expect(document.querySelectorAll('[role="list"]')).toHaveLength(2)
    expect(document.querySelector('button[aria-current="true"]')?.textContent).toContain('Documents')
  })

  it('keeps the stale view through a slow path scan and submits a legal trailing space verbatim', async () => {
    vi.useFakeTimers()
    let rejectPath!: (reason: unknown) => void
    const listDirectory = vi.fn((path?: string) => {
      if (path === `${DOCS} `) return new Promise<DirectoryListing>((_, reject) => { rejectPath = reject })
      return Promise.resolve(listing(path ?? HOME))
    })
    renderBrowser({ listDirectory })
    await flush()
    click(button('编辑文件夹路径'))
    const input = textbox('编辑文件夹路径')
    write(input, `${DOCS} `)
    expect(button('打开所选文件夹').disabled).toBe(true)
    expect(button('新建文件夹').disabled).toBe(true)
    act(() => input.dispatchEvent(new CompositionEvent('compositionstart', { bubbles: true })))
    key(input, 'Enter')
    expect(listDirectory).not.toHaveBeenCalledWith(`${DOCS} `, expect.anything())
    act(() => input.dispatchEvent(new CompositionEvent('compositionend', { bubbles: true })))
    key(input, 'Enter')
    expect(listDirectory).toHaveBeenCalledWith(`${DOCS} `, expect.any(AbortSignal))
    expect(document.body.textContent).toContain('Documents')
    expect(document.body.textContent).not.toContain('正在读取目录…')
    act(() => vi.advanceTimersByTime(300))
    expect(document.body.textContent).toContain('正在读取目录…')
    act(() => rejectPath(new Error('unreadable')))
    await flush()
    expect(document.querySelector('[role="alert"]')?.textContent).toBe('unreadable')
    expect(textbox('编辑文件夹路径').value).toBe(`${DOCS} `)
  })

  it('keeps path editing as the escape route after the initial home listing fails', async () => {
    let first = true
    const listDirectory = vi.fn(async (path?: string) => {
      if (first) {
        first = false
        throw new Error('home unavailable')
      }
      return listing(path ?? HOME)
    })
    renderBrowser({ listDirectory })
    await flush()
    expect(document.querySelector('[role="alert"]')?.textContent).toBe('home unavailable')
    click(button('编辑文件夹路径'))
    const input = textbox('编辑文件夹路径')
    expect(input.value).toBe('')
    write(input, DOCS)
    key(input, 'Enter')
    await flush()
    expect(document.body.textContent).toContain('harness')
    expect(document.querySelectorAll('[role="list"]')).toHaveLength(2)
  })

  it('keeps the Web directory browser available beside a failing native picker', async () => {
    const onNativePick = vi.fn(async () => { throw new Error('portal unavailable') })
    renderBrowser({ onNativePick })
    await flush()

    expect(button('打开所选文件夹').disabled).toBe(false)
    click(button('系统选择器'))
    await flush()

    expect(onNativePick).toHaveBeenCalledOnce()
    expect(document.querySelector('[role="alert"]')?.textContent).toBe('portal unavailable')
    expect(button('打开所选文件夹').disabled).toBe(false)
    expect(button('系统选择器').disabled).toBe(false)
  })

  it('makes the parent inert while the topmost create dialog is open and restores focus on cancel', async () => {
    renderBrowser()
    await flush()
    const trigger = button('新建文件夹')
    click(trigger)
    expect(document.querySelector('[inert]')).not.toBeNull()
    expect(button('打开所选文件夹').disabled).toBe(true)
    const name = textbox('文件夹名称')
    key(name, 'Escape')
    await flush()
    expect(document.querySelector('input[aria-label="文件夹名称"]')).toBeNull()
    expect(document.querySelectorAll('[role="dialog"]')).toHaveLength(1)
    expect(document.activeElement).toBe(trigger)
  })

  it('preserves a created folder name, selects it after relist, and keeps create failures nested', async () => {
    let created = false
    const listDirectory = vi.fn(async (path?: string) => listing(path ?? HOME, created))
    const createDirectory = vi.fn(async (_path: string, name: string) => {
      created = true
      return `${HOME}/${name}`
    })
    renderBrowser({ listDirectory, createDirectory })
    await flush()
    click(button('新建文件夹'))
    const name = textbox('文件夹名称')
    write(name, 'fresh ')
    act(() => name.dispatchEvent(new CompositionEvent('compositionstart', { bubbles: true })))
    key(name, 'Enter')
    expect(createDirectory).not.toHaveBeenCalled()
    act(() => name.dispatchEvent(new CompositionEvent('compositionend', { bubbles: true })))
    key(name, 'Enter')
    await flush()
    expect(createDirectory).toHaveBeenCalledWith(HOME, 'fresh ')
    expect(document.querySelector('button[aria-current="true"]')?.textContent).toContain('fresh ')

    click(button('新建文件夹'))
    write(textbox('文件夹名称'), 'taken')
    createDirectory.mockRejectedValueOnce(new Error('already exists'))
    key(textbox('文件夹名称'), 'Enter')
    await flush()
    expect(document.querySelector('input[aria-label="文件夹名称"]')).not.toBeNull()
    expect(document.querySelector('[role="alert"]')?.textContent).toBe('already exists')
  })
})
