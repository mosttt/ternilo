import { afterEach, describe, expect, it, vi } from 'vitest'
import type { Translate } from '@/i18n/runtime'
import { en } from '@/i18n/resources/conversation'
import { attachmentFromFile } from './attachment-ingest'

const t: Translate<'conversation'> = (key, params) => en[key].replace(/\{(\w+)\}/g, (match, name: string) => (
  params && Object.hasOwn(params, name) ? String(params[name]) : match
))

afterEach(() => vi.restoreAllMocks())

describe('attachment ingestion', () => {
  it('keeps supported UTF8 text inline, including literal data URLs and byte order marks', async () => {
    await expect(attachmentFromFile(new File(['hello'], 'note.md', { type: 'text/markdown' }), t)).resolves.toMatchObject({
      name: 'note.md', media_type: 'text/markdown', content: 'hello',
    })
    for (const content of ['data:text/plain;base64,YWJj', '\uFEFF{"value":"你好"}\r\n']) {
      const file = new File([content], 'literal.json', { type: 'application/json; charset=utf-8' })
      const result = await attachmentFromFile(file, t)
      expect(result.content).toBe(content)
      expect(Array.from(new TextEncoder().encode(result.content))).toEqual(Array.from(new Uint8Array(await file.arrayBuffer())))
    }
  })

  it('rejects binary and malformed UTF8 rather than replacing its bytes', async () => {
    const bytes = new Uint8Array([0x50, 0x4b, 3, 4, 0, 0xff, 0xfe, 0xc3, 0x28])
    for (const [name, mediaType] of [['archive.zip', 'application/zip'], ['legacy.txt', 'text/plain']]) {
      await expect(attachmentFromFile(new File([bytes], name, { type: mediaType }), t))
        .rejects.toThrow(`${name} is not a supported UTF-8 text file`)
    }
    await expect(attachmentFromFile(new File(['bad\0data'], 'bad.txt', { type: 'text/plain' }), t))
      .rejects.toThrow('bad.txt is not a supported UTF-8 text file')
  })

  it('retains text without a reported MIME and preserves the inline text limit', async () => {
    await expect(attachmentFromFile(new File(['print("hello")\n'], 'main.py'), t)).resolves.toMatchObject({
      media_type: 'text/plain', content: 'print("hello")\n',
    })
    await expect(attachmentFromFile(new File(['x'.repeat(512 * 1024 + 1)], 'huge.txt', { type: 'text/plain' }), t))
      .rejects.toThrow('huge.txt exceeds the 512 KiB limit')
  })

  it('normalizes a small image into a metadata-free request image', async () => {
    const close = vi.fn()
    const create = vi.fn(async () => ({ width: 800, height: 600, close }))
    vi.stubGlobal('createImageBitmap', create)
    vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockReturnValue({ drawImage: vi.fn() } as unknown as CanvasRenderingContext2D)
    vi.spyOn(HTMLCanvasElement.prototype, 'toBlob').mockImplementation(callback => {
      callback(new Blob(['webp'], { type: 'image/webp' }))
    })
    const file = new File(['png-bytes'], 'diagram.png', { type: 'image/png' })

    const result = await attachmentFromFile(file, t)

    expect(result).toMatchObject({ name: 'diagram.webp', media_type: 'image/webp' })
    expect(result.content).toMatch(/^data:image\/webp;base64,/)
    expect(create).toHaveBeenCalledWith(file, { imageOrientation: 'from-image' })
    expect(close).toHaveBeenCalledOnce()
  })

  it('normalizes a large phone camera image before creating the attachment', async () => {
    const close = vi.fn()
    vi.stubGlobal('createImageBitmap', vi.fn(async () => ({ width: 4032, height: 3024, close })))
    const drawImage = vi.fn()
    vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockReturnValue({ drawImage } as unknown as CanvasRenderingContext2D)
    vi.spyOn(HTMLCanvasElement.prototype, 'toBlob').mockImplementation(callback => {
      callback(new Blob([new Uint8Array(1024 * 1024)], { type: 'image/jpeg' }))
    })
    const file = new File([new Uint8Array(5 * 1024 * 1024)], 'IMG_0001.HEIC', { type: 'image/heic' })

    const result = await attachmentFromFile(file, t)

    expect(result.name).toBe('IMG_0001.jpg')
    expect(result.media_type).toBe('image/jpeg')
    expect(result.content).toMatch(/^data:image\/jpeg;base64,/)
    expect(drawImage).toHaveBeenCalledWith(expect.anything(), 0, 0, 2365, 1774)
    expect(close).toHaveBeenCalledOnce()
  })

  it('uses the actual canvas fallback type when WebP encoding is unavailable', async () => {
    vi.stubGlobal('createImageBitmap', vi.fn(async () => ({ width: 100, height: 100, close: vi.fn() })))
    vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockReturnValue({ drawImage: vi.fn() } as unknown as CanvasRenderingContext2D)
    vi.spyOn(HTMLCanvasElement.prototype, 'toBlob').mockImplementation(callback => {
      callback(new Blob(['png-fallback'], { type: 'image/png' }))
    })

    const result = await attachmentFromFile(new File(['png'], 'alpha.png', { type: 'image/png' }), t)

    expect(result).toMatchObject({ name: 'alpha.png', media_type: 'image/png' })
    expect(result.content).toMatch(/^data:image\/png;base64,/)
  })

  it('reports an actionable error before decoding an oversized source photo', async () => {
    const create = vi.fn()
    vi.stubGlobal('createImageBitmap', create)
    const file = new File([new Uint8Array(20 * 1024 * 1024 + 1)], 'huge.jpg', { type: 'image/jpeg' })

    await expect(attachmentFromFile(file, t)).rejects.toThrow('huge.jpg exceeds the 20 MiB limit')
    expect(create).not.toHaveBeenCalled()
  })
})
