import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { Translate } from '@/i18n/runtime'
import { zh } from '@/i18n/resources/conversation'
import { ComposerDropOverlay } from './composer-drop-overlay'

const t: Translate<'conversation'> = (key, params) => zh[key].replace(/\{(\w+)\}/g, (match, name: string) => (
  params && Object.hasOwn(params, name) ? String(params[name]) : match
))

let root: Root
let host: HTMLDivElement
beforeEach(() => {
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true
  host = document.createElement('div'); document.body.append(host); root = createRoot(host)
})
afterEach(() => { act(() => root.unmount()); host.remove(); document.querySelector('[data-drop-overlay]')?.remove() })

function fileDrag(type: string, files: File[]) {
  const event = new Event(type, { bubbles: true, cancelable: true }) as DragEvent
  Object.defineProperty(event, 'dataTransfer', {
    value: { types: ['Files'], files, dropEffect: 'none' }, configurable: true,
  })
  return event
}

describe('ComposerDropOverlay', () => {
  it('covers the full page during file drag and delivers the dropped batch exactly once', () => {
    const onFiles = vi.fn()
    act(() => root.render(<ComposerDropOverlay disabled={false} onFiles={onFiles} t={t} />))
    const file = new File(['hello'], 'note.txt', { type: 'text/plain' })
    act(() => document.dispatchEvent(fileDrag('dragenter', [file])))
    expect(document.querySelector('[data-drop-overlay]')?.textContent).toContain('将文件放到这里')
    act(() => document.dispatchEvent(fileDrag('drop', [file])))
    expect(onFiles).toHaveBeenCalledOnce()
    expect(onFiles.mock.calls[0]?.[0]?.[0]?.name).toBe('note.txt')
    expect(document.querySelector('[data-drop-overlay]')).toBeNull()
  })
})
