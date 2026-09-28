import type { Translate } from '@/i18n/runtime'
import type { Attachment } from '@/types'

const MAX_TEXT_BYTES = 512 * 1024
const MAX_IMAGE_SOURCE_BYTES = 20 * 1024 * 1024
const MAX_SOURCE_PIXELS = 64_000_000
const MAX_SOURCE_EDGE = 8192
const NORMALIZED_IMAGE_PIXELS = 2048 * 2048
const TARGET_IMAGE_BYTES = 4 * 1024 * 1024

type DecodedImage = {
  source: CanvasImageSource
  width: number
  height: number
  dispose(): void
}

function imageMediaType(file: File): string | null {
  if (file.type.startsWith('image/')) return file.type.toLowerCase()
  const extension = file.name.split('.').pop()?.toLowerCase()
  return extension === 'jpg' || extension === 'jpeg' ? 'image/jpeg'
    : extension === 'png' ? 'image/png'
      : extension === 'webp' ? 'image/webp'
        : extension === 'gif' ? 'image/gif'
          : extension === 'heic' ? 'image/heic'
            : extension === 'heif' ? 'image/heif'
              : null
}

function readAsDataUrl(blob: Blob, t: Translate<'conversation'>): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader()
    reader.addEventListener('load', () => resolve(String(reader.result)), { once: true })
    reader.addEventListener('error', () => reject(reader.error ?? new Error(t('attachment.readFailed'))), { once: true })
    reader.readAsDataURL(blob)
  })
}

async function decodeImage(file: File): Promise<DecodedImage> {
  if (typeof createImageBitmap === 'function') {
    try {
      const bitmap = await createImageBitmap(file, { imageOrientation: 'from-image' })
      return {
        source: bitmap,
        width: bitmap.width,
        height: bitmap.height,
        dispose: () => bitmap.close(),
      }
    } catch {
      // Safari can decode some camera formats through <img> but not createImageBitmap.
    }
  }

  const url = URL.createObjectURL(file)
  const image = new Image()
  try {
    await new Promise<void>((resolve, reject) => {
      image.addEventListener('load', () => resolve(), { once: true })
      image.addEventListener('error', () => reject(new Error('decode failed')), { once: true })
      image.src = url
    })
    return {
      source: image,
      width: image.naturalWidth,
      height: image.naturalHeight,
      dispose: () => URL.revokeObjectURL(url),
    }
  } catch (cause) {
    URL.revokeObjectURL(url)
    throw cause
  }
}

function canvasBlob(canvas: HTMLCanvasElement, mediaType: 'image/jpeg' | 'image/webp', quality: number): Promise<Blob> {
  return new Promise((resolve, reject) => canvas.toBlob(
    blob => blob ? resolve(blob) : reject(new Error('encode failed')),
    mediaType,
    quality,
  ))
}

function normalizedName(name: string, mediaType: string): string {
  const stem = name.replace(/\.[^.]+$/, '') || 'image'
  const extension = mediaType === 'image/webp' ? 'webp' : mediaType === 'image/png' ? 'png' : 'jpg'
  return `${stem}.${extension}`
}

async function normalizedImage(
  file: File,
  mediaType: string,
  t: Translate<'conversation'>,
): Promise<Attachment> {
  if (file.size > MAX_IMAGE_SOURCE_BYTES) {
    throw new Error(t('attachment.fileTooLarge', { name: file.name, limit: '20 MiB' }))
  }

  let decoded: DecodedImage
  try {
    decoded = await decodeImage(file)
  } catch {
    throw new Error(t('attachment.imageDecodeFailed', { name: file.name }))
  }

  try {
    const sourcePixels = decoded.width * decoded.height
    if (sourcePixels > MAX_SOURCE_PIXELS || Math.max(decoded.width, decoded.height) > MAX_SOURCE_EDGE) {
      throw new Error(t('attachment.imageDimensionsTooLarge', { name: file.name, limit: '8192 px / 64 MP' }))
    }
    const pixelScale = Math.min(1, Math.sqrt(NORMALIZED_IMAGE_PIXELS / sourcePixels))
    let width = Math.max(1, Math.round(decoded.width * pixelScale))
    let height = Math.max(1, Math.round(decoded.height * pixelScale))
    const canvas = document.createElement('canvas')
    const context = canvas.getContext('2d')
    if (!context) throw new Error(t('attachment.imageDecodeFailed', { name: file.name }))
    const outputMediaType = ['image/png', 'image/webp', 'image/gif'].includes(mediaType)
      ? 'image/webp' as const
      : 'image/jpeg' as const

    let blob: Blob | null = null
    for (let resize = 0; resize < 4; resize += 1) {
      canvas.width = width
      canvas.height = height
      context.drawImage(decoded.source, 0, 0, width, height)
      for (const quality of [0.88, 0.78, 0.68, 0.58]) {
        blob = await canvasBlob(canvas, outputMediaType, quality)
        if (blob.size <= TARGET_IMAGE_BYTES) break
      }
      if (blob && blob.size <= TARGET_IMAGE_BYTES) break
      width = Math.max(1, Math.round(width * 0.8))
      height = Math.max(1, Math.round(height * 0.8))
    }
    if (!blob || blob.size > TARGET_IMAGE_BYTES) {
      throw new Error(t('attachment.imageCompressFailed', { name: file.name }))
    }
    const encodedMediaType = ['image/jpeg', 'image/png', 'image/webp'].includes(blob.type)
      ? blob.type
      : outputMediaType
    return {
      name: normalizedName(file.name, encodedMediaType),
      media_type: encodedMediaType,
      content: await readAsDataUrl(blob, t),
    }
  } catch (cause) {
    if (cause instanceof Error && cause.message.includes(file.name)) throw cause
    throw new Error(t('attachment.imageCompressFailed', { name: file.name }))
  } finally {
    decoded.dispose()
  }
}

export async function attachmentFromFile(file: File, t: Translate<'conversation'>): Promise<Attachment> {
  const mediaType = imageMediaType(file)
  if (mediaType) return normalizedImage(file, mediaType, t)
  if (file.size > MAX_TEXT_BYTES) {
    throw new Error(t('attachment.fileTooLarge', { name: file.name, limit: '512 KiB' }))
  }
  let content: string
  try {
    content = new TextDecoder('utf-8', { fatal: true, ignoreBOM: true }).decode(await file.arrayBuffer())
  } catch {
    throw new Error(t('attachment.unsupportedText', { name: file.name }))
  }
  if (content.includes('\0')) throw new Error(t('attachment.unsupportedText', { name: file.name }))
  return { name: file.name, media_type: file.type || 'text/plain', content }
}
