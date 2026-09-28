import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { Attachment } from '@/types'
import {
  imageDataSource, MessageAttachments, resolveMessageImage, type ImageAttachmentResolver,
} from './message-attachments'
import { retainHistoricalImageSession } from '@/domain/historical-images'

let host: HTMLDivElement
let root: Root

const png = (name: string, content = 'data:image/png;base64,iVBORw0KGgo='): Attachment => ({
  name,
  media_type: 'image/png',
  content,
})

function button(label: string) {
  return document.querySelector<HTMLButtonElement>(`button[aria-label="${label}"]`)
}

async function settle() {
  await act(async () => {
    await Promise.resolve()
    await new Promise(resolve => window.setTimeout(resolve, 0))
  })
}

beforeEach(() => {
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true
  host = document.createElement('div')
  document.body.append(host)
  root = createRoot(host)
})

afterEach(() => {
  act(() => root.unmount())
  host.remove()
  vi.restoreAllMocks()
  vi.unstubAllGlobals()
  document.body.style.overflow = ''
  delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
})

describe('message attachment sources', () => {
  it('only accepts a data URL whose declared and encoded media types are images and match', () => {
    const source = 'data:image/png;base64,iVBORw0KGgo='
    expect(imageDataSource(png('valid.png', source))).toBe(source)
    expect(imageDataSource({ name: 'text.txt', media_type: 'text/plain', content: source })).toBeNull()
    expect(imageDataSource(png('mismatch.png', 'data:image/jpeg;base64,/9j/'))).toBeNull()
    expect(imageDataSource(png('reference.png', `ternilo-attachment://sha256/${'a'.repeat(64)}`))).toBeNull()
  })

  it('uses the existing authorized attachment resolver for a durable reference', async () => {
    const reference = png('persisted.png', `ternilo-attachment://sha256/${'a'.repeat(64)}`)
    const resolved = png('persisted.png')
    const fetch = vi.fn(async (_input: RequestInfo | URL, _init?: RequestInit) => new Response(JSON.stringify(resolved), {
      status: 200,
      headers: { 'content-type': 'application/json' },
    }))
    vi.stubGlobal('fetch', fetch)

    await expect(resolveMessageImage(reference, 'session one')).resolves.toBe(resolved.content)
    expect(fetch).toHaveBeenCalledTimes(1)
    expect(fetch.mock.calls[0]?.[0]).toBe('/api/v1/sessions/session%20one/attachments/resolve')
    expect(fetch.mock.calls[0]?.[1]).toMatchObject({
      method: 'POST',
      body: JSON.stringify({ attachment: reference }),
    })
  })

  it('shares one durable image resolution per session between consumers', async () => {
    const reference = png('shared.png', `ternilo-attachment://sha256/${'b'.repeat(64)}`)
    const resolved = png('shared.png')
    const fetch = vi.fn(async () => new Response(JSON.stringify(resolved), {
      status: 200, headers: { 'content-type': 'application/json' },
    }))
    vi.stubGlobal('fetch', fetch)
    const [chat, details] = await Promise.all([
      resolveMessageImage(reference, 'shared-session'),
      resolveMessageImage(reference, 'shared-session'),
    ])
    expect(chat).toBe(resolved.content)
    expect(details).toBe(chat)
    expect(fetch).toHaveBeenCalledTimes(1)
  })

  it('releases a Session cache only after its final consumer leaves', async () => {
    const reference = png('scoped.png', `ternilo-attachment://sha256/${'c'.repeat(64)}`)
    const resolved = png('scoped.png')
    const fetch = vi.fn(async () => new Response(JSON.stringify(resolved), {
      status: 200, headers: { 'content-type': 'application/json' },
    }))
    vi.stubGlobal('fetch', fetch)
    const releaseChat = retainHistoricalImageSession('scoped-session')
    const releaseDetails = retainHistoricalImageSession('scoped-session')

    await resolveMessageImage(reference, 'scoped-session')
    releaseChat()
    await resolveMessageImage(reference, 'scoped-session')
    expect(fetch).toHaveBeenCalledTimes(1)

    releaseDetails()
    await resolveMessageImage(reference, 'scoped-session')
    expect(fetch).toHaveBeenCalledTimes(2)
  })

  it('does not invent a URL for an opaque or malformed image reference', async () => {
    const fetch = vi.fn()
    vi.stubGlobal('fetch', fetch)
    await expect(resolveMessageImage(png('opaque.png', 'https://host.invalid/private.png')))
      .rejects.toThrow('historical_image_source_unavailable')
    expect(fetch).not.toHaveBeenCalled()
  })
})

describe('MessageAttachments', () => {
  it('renders images as a gallery and preserves non-images as accessible file chips', async () => {
    const resolveImage: ImageAttachmentResolver = vi.fn(async attachment => attachment.content)
    act(() => root.render(<MessageAttachments
      attachments={[
        png('one.png'),
        { name: 'notes.txt', media_type: 'text/plain', content: 'notes' },
        png('two.png', 'data:image/png;base64,AA=='),
      ]}
      resolveImage={resolveImage}
    />))
    await settle()

    expect(host.querySelector('[aria-label="图片附件，共 2 张"]')).not.toBeNull()
    expect(host.querySelectorAll('[data-message-image-thumbnail][data-variant="tile"]')).toHaveLength(2)
    expect(host.querySelector<HTMLImageElement>('img[alt="one.png"]')).not.toBeNull()
    expect(host.querySelector<HTMLImageElement>('img[alt="two.png"]')).not.toBeNull()
    expect(host.querySelector('[aria-label="附件：notes.txt，text/plain"]')?.textContent).toContain('notes.txt')
  })

  it('opens a modal gallery, supports buttons and arrow keys, closes on Escape, and restores focus', async () => {
    const resolveImage: ImageAttachmentResolver = vi.fn(async attachment => attachment.content)
    act(() => root.render(<MessageAttachments attachments={[
      png('first.png'), png('second.png', 'data:image/png;base64,AA=='),
    ]} resolveImage={resolveImage} />))
    await settle()

    const opener = button('打开图片 first.png')
    expect(opener).not.toBeNull()
    opener?.focus()
    act(() => opener?.click())
    const dialog = document.querySelector('[role="dialog"][aria-label="图片预览：first.png"]')
    expect(dialog).not.toBeNull()
    expect(document.activeElement).toBe(button('关闭图片预览'))
    expect(dialog?.textContent).toContain('1 / 2')

    act(() => button('下一张图片')?.click())
    expect(document.querySelector('[role="dialog"]')?.getAttribute('aria-label')).toBe('图片预览：second.png')
    expect(document.querySelector('[role="dialog"]')?.textContent).toContain('2 / 2')

    act(() => window.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowLeft' })))
    expect(document.querySelector('[role="dialog"]')?.getAttribute('aria-label')).toBe('图片预览：first.png')
    act(() => window.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape' })))
    expect(document.querySelector('[role="dialog"]')).toBeNull()
    expect(document.activeElement).toBe(opener)
    expect(document.body.style.overflow).toBe('')
  })

  it('shows a named retry state after thumbnail resolution fails', async () => {
    const resolveImage = vi.fn<ImageAttachmentResolver>()
      .mockRejectedValueOnce(new Error('offline'))
      .mockResolvedValueOnce('data:image/png;base64,iVBORw0KGgo=')
    act(() => root.render(<MessageAttachments attachments={[png('retry.png')]} resolveImage={resolveImage} />))
    await settle()

    const retry = button('重新加载图片 retry.png')
    expect(retry?.textContent).toContain('图片加载失败')
    expect(retry?.getAttribute('data-message-image-state')).toBe('error')
    act(() => retry?.click())
    await settle()
    expect(button('打开图片 retry.png')?.getAttribute('data-message-image-state')).toBe('ready')
    expect(resolveImage).toHaveBeenCalledTimes(2)
  })

  it('exposes the pending resolver as an explicit non-interactive loading state', async () => {
    let resolve!: (source: string) => void
    const resolveImage = vi.fn<ImageAttachmentResolver>(() => new Promise(value => { resolve = value }))
    act(() => root.render(<MessageAttachments attachments={[png('pending.png')]} resolveImage={resolveImage} />))
    const pending = button('正在加载图片 pending.png')
    expect(pending?.getAttribute('data-message-image-state')).toBe('loading')
    expect(pending?.disabled).toBe(true)

    await act(async () => {
      resolve('data:image/png;base64,iVBORw0KGgo=')
      await Promise.resolve()
    })
    expect(button('打开图片 pending.png')?.getAttribute('data-message-image-state')).toBe('ready')
  })

  it('moves focus through modal controls without escaping the dialog', async () => {
    const resolveImage: ImageAttachmentResolver = vi.fn(async attachment => attachment.content)
    act(() => root.render(<MessageAttachments attachments={[png('one.png'), png('two.png')]} resolveImage={resolveImage} />))
    await settle()
    act(() => button('打开图片 one.png')?.click())
    const close = button('关闭图片预览')
    expect(document.activeElement).toBe(close)
    act(() => window.dispatchEvent(new KeyboardEvent('keydown', { key: 'Tab' })))
    expect(document.activeElement).toBe(button('上一张图片'))
    act(() => window.dispatchEvent(new KeyboardEvent('keydown', { key: 'Tab', shiftKey: true })))
    expect(document.activeElement).toBe(close)
  })
})
