import { afterEach, describe, expect, it, vi } from 'vitest'
import { copyText } from './clipboard'

afterEach(() => { document.body.replaceChildren(); vi.unstubAllGlobals(); vi.restoreAllMocks() })

describe('clipboard compatibility', () => {
  it.each([undefined, { writeText: async () => { throw new DOMException('Denied', 'NotAllowedError') } }])('copies exact text and restores the editor when the async API is unavailable or denied', async clipboard => {
    vi.stubGlobal('navigator', { clipboard })
    const dialog = document.createElement('div')
    dialog.setAttribute('role', 'dialog')
    const editor = document.createElement('textarea')
    editor.value = 'draft to keep'
    dialog.append(editor)
    document.body.append(dialog)
    editor.focus()
    editor.setSelectionRange(2, 7)
    const text = '<svg>\n  $literal `value`\n</svg>'
    const copy = vi.fn(() => {
      const active = document.activeElement as HTMLTextAreaElement
      expect(dialog.contains(active)).toBe(true)
      expect(active.value.slice(active.selectionStart, active.selectionEnd)).toBe(text)
      return true
    })
    Object.defineProperty(document, 'execCommand', { configurable: true, value: copy })
    await copyText(text)
    expect(copy).toHaveBeenCalledWith('copy')
    expect(document.activeElement).toBe(editor)
    expect([editor.selectionStart, editor.selectionEnd]).toEqual([2, 7])
    expect(dialog.querySelectorAll('textarea')).toHaveLength(1)
  })

  it('reports denied copying and removes its temporary field', async () => {
    vi.stubGlobal('navigator', {})
    Object.defineProperty(document, 'execCommand', { configurable: true, value: () => false })
    await expect(copyText('keep this')).rejects.toThrow('Browser denied clipboard access')
    expect(document.querySelector('textarea')).toBeNull()
  })
})
