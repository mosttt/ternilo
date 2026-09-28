import * as React from 'react'
import { createPortal } from 'react-dom'
import {
  ChevronLeft, ChevronRight, FileText, ImageOff, LoaderCircle, RotateCcw, X,
} from 'lucide-react'
import {
  historicalImageSource, imageDataSource, resolveHistoricalAttachment,
} from '@/domain/historical-images'
import { useTranslate } from '@/i18n/provider'
import type { Translate } from '@/i18n/runtime'
import type { Attachment } from '@/types'
import css from './message-attachments.module.css'

export type ImageAttachmentResolver = (attachment: Attachment) => Promise<string>

export async function resolveMessageAttachment(attachment: Attachment, sessionId?: string): Promise<Attachment> {
  return resolveHistoricalAttachment(attachment, sessionId)
}

export { imageDataSource }

export async function resolveMessageImage(attachment: Attachment, sessionId?: string): Promise<string> {
  return historicalImageSource(attachment, sessionId)
}

export interface LoadedImage {
  attachmentIndex: number
  name: string
  source: string
}

function MessageImage({
  attachment, attachmentIndex, tile, resolveImage, onReady, onUnavailable, onOpen, t,
}: {
  attachment: Attachment
  attachmentIndex: number
  tile: boolean
  resolveImage: ImageAttachmentResolver
  onReady(index: number, source: string): void
  onUnavailable(index: number): void
  onOpen(index: number): void
  t: Translate<'chat'>
}) {
  const [attempt, setAttempt] = React.useState(0)
  const [state, setState] = React.useState<{ status: 'loading' | 'ready' | 'failed'; source?: string }>({ status: 'loading' })
  const name = attachment.name.trim() || t('attachment.image')

  React.useEffect(() => {
    let live = true
    setState({ status: 'loading' })
    onUnavailable(attachmentIndex)
    void resolveImage(attachment).then(source => {
      if (!live) return
      setState({ status: 'ready', source })
      onReady(attachmentIndex, source)
    }).catch(() => {
      if (!live) return
      setState({ status: 'failed' })
      onUnavailable(attachmentIndex)
    })
    return () => { live = false }
  }, [attachment, attachmentIndex, attempt, onReady, onUnavailable, resolveImage])

  if (state.status === 'failed') return (
    <button
      type="button"
      className={css.imageFailure}
      data-message-image-failure=""
      data-message-image-state="error"
      data-variant={tile ? 'tile' : 'single'}
      aria-label={t('attachment.retryImage', { name })}
      onClick={() => setAttempt(value => value + 1)}
    >
      <ImageOff aria-hidden="true" />
      <span>{t('attachment.imageFailed')}</span>
      <RotateCcw aria-hidden="true" />
    </button>
  )

  return (
    <button
      type="button"
      className={css.imageThumbnail}
      data-message-image-thumbnail=""
      data-message-image-state={state.status}
      data-variant={tile ? 'tile' : 'single'}
      aria-label={state.status === 'ready' ? t('attachment.openImage', { name }) : t('attachment.loadingImage', { name })}
      disabled={state.status !== 'ready'}
      onClick={() => onOpen(attachmentIndex)}
    >
      {state.status === 'ready' && state.source
        ? <img src={state.source} alt={name} onError={() => { setState({ status: 'failed' }); onUnavailable(attachmentIndex) }} />
        : <span className={css.imageLoading} data-message-image-loading=""><LoaderCircle className="animate-spin" aria-hidden="true" /><span>{t('attachment.imageLoading')}</span></span>}
    </button>
  )
}

export function ImageLightbox({
  image, position, count, onClose, onPrevious, onNext, t: suppliedT,
}: {
  image: LoadedImage
  position: number
  count: number
  onClose(): void
  onPrevious(): void
  onNext(): void
  t?: Translate<'chat'>
}) {
  const localT = useTranslate('chat')
  const t = suppliedT ?? localT
  const closeRef = React.useRef<HTMLButtonElement>(null)
  const dialogRef = React.useRef<HTMLDivElement>(null)
  const restoreFocusRef = React.useRef<HTMLElement | null>(null)
  const previousRef = React.useRef(onPrevious)
  const nextRef = React.useRef(onNext)
  const closeActionRef = React.useRef(onClose)
  const captionId = React.useId()
  previousRef.current = onPrevious
  nextRef.current = onNext
  closeActionRef.current = onClose

  React.useEffect(() => {
    restoreFocusRef.current = document.activeElement instanceof HTMLElement ? document.activeElement : null
    const previousOverflow = document.body.style.overflow
    document.body.style.overflow = 'hidden'
    closeRef.current?.focus()
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape') {
        event.preventDefault()
        closeActionRef.current()
        return
      }
      if (count > 1 && event.key === 'ArrowLeft') {
        event.preventDefault()
        previousRef.current()
        return
      }
      if (count > 1 && event.key === 'ArrowRight') {
        event.preventDefault()
        nextRef.current()
        return
      }
      if (event.key !== 'Tab') return
      const controls = [...(dialogRef.current?.querySelectorAll<HTMLButtonElement>('button:not(:disabled)') ?? [])]
      if (!controls.length) return
      const first = controls[0]
      const last = controls[controls.length - 1]
      if (event.shiftKey && document.activeElement === first) {
        event.preventDefault()
        last?.focus()
      } else if (!event.shiftKey && document.activeElement === last) {
        event.preventDefault()
        first?.focus()
      }
    }
    window.addEventListener('keydown', onKeyDown)
    return () => {
      window.removeEventListener('keydown', onKeyDown)
      document.body.style.overflow = previousOverflow
      restoreFocusRef.current?.focus()
    }
  }, [count])

  return createPortal(
    <div
      ref={dialogRef}
      className={css.lightbox}
      data-message-image-lightbox=""
      role="dialog"
      aria-modal="true"
      aria-label={t('attachment.preview', { name: image.name })}
      aria-describedby={captionId}
    >
      <div className={css.backdrop} aria-hidden="true" onMouseDown={onClose} />
      <div className={css.stage}>
        <img src={image.source} alt={image.name} />
        <div id={captionId} className={css.caption}>
          <span>{image.name}</span>
          <span aria-live="polite">{position + 1} / {count}</span>
        </div>
      </div>
      {count > 1 && <>
        <button type="button" className={css.nav} data-direction="previous" aria-label={t('attachment.previousImage')} onClick={onPrevious}><ChevronLeft aria-hidden="true" /></button>
        <button type="button" className={css.nav} data-direction="next" aria-label={t('attachment.nextImage')} onClick={onNext}><ChevronRight aria-hidden="true" /></button>
      </>}
      <button ref={closeRef} type="button" className={css.close} aria-label={t('attachment.closePreview')} onClick={onClose}><X aria-hidden="true" /></button>
    </div>,
    document.body,
  )
}

export function MessageAttachments({
  attachments, sessionId, resolveImage, align = 'start',
}: {
  attachments: Attachment[]
  sessionId?: string
  resolveImage?: ImageAttachmentResolver
  align?: 'start' | 'end'
}) {
  const t = useTranslate('chat')
  const effectiveResolver = React.useCallback<ImageAttachmentResolver>(
    attachment => resolveImage ? resolveImage(attachment) : resolveMessageImage(attachment, sessionId),
    [resolveImage, sessionId],
  )
  const images = React.useMemo(() => attachments
    .map((attachment, attachmentIndex) => ({ attachment, attachmentIndex }))
    .filter(item => item.attachment.media_type.trim().toLowerCase().startsWith('image/')), [attachments])
  const files = React.useMemo(() => attachments.filter(attachment => !attachment.media_type.trim().toLowerCase().startsWith('image/')), [attachments])
  const [sources, setSources] = React.useState<Record<number, string>>({})
  const [openIndex, setOpenIndex] = React.useState<number | null>(null)

  React.useEffect(() => {
    setSources({})
    setOpenIndex(null)
  }, [attachments])

  const onReady = React.useCallback((index: number, source: string) => {
    setSources(current => current[index] === source ? current : { ...current, [index]: source })
  }, [])
  const onUnavailable = React.useCallback((index: number) => {
    setSources(current => {
      if (!(index in current)) return current
      const next = { ...current }
      delete next[index]
      return next
    })
    setOpenIndex(current => current === index ? null : current)
  }, [])
  const loaded = React.useMemo<LoadedImage[]>(() => images.flatMap(({ attachment, attachmentIndex }) => {
    const source = sources[attachmentIndex]
    return source ? [{ attachmentIndex, name: attachment.name.trim() || t('attachment.image'), source }] : []
  }), [images, sources, t])
  const openPosition = openIndex === null ? -1 : loaded.findIndex(image => image.attachmentIndex === openIndex)
  const move = React.useCallback((offset: number) => {
    if (!loaded.length || openPosition < 0) return
    const next = (openPosition + offset + loaded.length) % loaded.length
    setOpenIndex(loaded[next]?.attachmentIndex ?? null)
  }, [loaded, openPosition])

  if (!attachments.length) return null
  return (
    <>
      {images.length > 0 && <div className={css.gallery} data-message-image-gallery="" data-align={align} data-count={images.length} aria-label={t('attachment.imageCount', { count: images.length })}>
        {images.map(({ attachment, attachmentIndex }) => <MessageImage
          key={`${attachment.name}-${attachmentIndex}`}
          attachment={attachment}
          attachmentIndex={attachmentIndex}
          tile={images.length > 1}
          resolveImage={effectiveResolver}
          onReady={onReady}
          onUnavailable={onUnavailable}
          onOpen={setOpenIndex}
          t={t}
        />)}
      </div>}
      {files.length > 0 && <div className={css.fileList} data-message-file-list="" data-align={align} aria-label={t('attachment.files')}>
        {files.map((attachment, index) => <span
          key={`${attachment.name}-${index}`}
          className={css.fileChip}
          data-message-file-chip=""
          aria-label={t('attachment.fileDescription', { name: attachment.name || t('attachment.unnamedFile'), type: attachment.media_type || t('attachment.unknownType') })}
          title={attachment.name}
        ><FileText aria-hidden="true" /><span>{attachment.name || t('attachment.unnamedFile')}</span></span>)}
      </div>}
      {openPosition >= 0 && loaded[openPosition] && <ImageLightbox
        image={loaded[openPosition]}
        position={openPosition}
        count={loaded.length}
        onClose={() => setOpenIndex(null)}
        onPrevious={() => move(-1)}
        onNext={() => move(1)}
      />}
    </>
  )
}
