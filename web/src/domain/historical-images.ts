import { api } from '@/api/client'
import type { Attachment } from '@/types'

export const ATTACHMENT_REFERENCE_PREFIX = 'ternilo-attachment://sha256/'

const sources = new Map<string, Promise<string>>()
const sessionConsumers = new Map<string, number>()

export function imageDataSource(attachment: Attachment): string | null {
  const declaredType = attachment.media_type.trim().toLowerCase().split(';', 1)[0]
  if (!declaredType.startsWith('image/')) return null
  const comma = attachment.content.indexOf(',')
  if (comma < 6 || !attachment.content.slice(0, 5).toLowerCase().startsWith('data:')) return null
  const encodedType = attachment.content.slice(5, comma).split(';', 1)[0]?.trim().toLowerCase()
  if (!encodedType?.startsWith('image/') || encodedType !== declaredType) return null
  return attachment.content
}

export async function resolveHistoricalAttachment(attachment: Attachment, sessionId?: string): Promise<Attachment> {
  if (!attachment.content.startsWith(ATTACHMENT_REFERENCE_PREFIX)) return attachment
  return api.request<Attachment>(sessionId
    ? `/sessions/${encodeURIComponent(sessionId)}/attachments/resolve`
    : '/attachments/resolve', { method: 'POST', body: { attachment } })
}

/** One durable reference resolves once per session and is shared by Chat and Details. */
export function historicalImageSource(attachment: Attachment, sessionId?: string): Promise<string> {
  const inline = imageDataSource(attachment)
  if (inline) return Promise.resolve(inline)
  if (!attachment.content.startsWith(ATTACHMENT_REFERENCE_PREFIX)) {
    return Promise.reject(new Error('historical_image_source_unavailable'))
  }
  const key = `${sessionId ?? ''}\u0000${attachment.media_type}\u0000${attachment.content}`
  const existing = sources.get(key)
  if (existing) return existing
  let pending: Promise<string>
  pending = resolveHistoricalAttachment(attachment, sessionId).then(value => {
    const source = imageDataSource(value)
    if (!source) throw new Error('historical_image_resolve_invalid')
    return source
  }).catch(cause => {
    if (sources.get(key) === pending) sources.delete(key)
    throw cause
  })
  sources.set(key, pending)
  return pending
}

export function clearHistoricalImageSession(sessionId: string) {
  const prefix = `${sessionId}\u0000`
  for (const key of sources.keys()) if (key.startsWith(prefix)) sources.delete(key)
}

/** Keep a shared Session image scope alive until its final Chat/Details consumer leaves. */
export function retainHistoricalImageSession(sessionId: string): () => void {
  sessionConsumers.set(sessionId, (sessionConsumers.get(sessionId) ?? 0) + 1)
  let released = false
  return () => {
    if (released) return
    released = true
    const remaining = (sessionConsumers.get(sessionId) ?? 1) - 1
    if (remaining > 0) {
      sessionConsumers.set(sessionId, remaining)
      return
    }
    sessionConsumers.delete(sessionId)
    clearHistoricalImageSession(sessionId)
  }
}
