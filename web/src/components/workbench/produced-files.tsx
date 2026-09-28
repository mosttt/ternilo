import * as React from 'react'
import { FileText, FolderOpen, Image as ImageIcon, LoaderCircle, RotateCcw } from 'lucide-react'
import type { ChatTurnDeliverable } from '@/domain/chat-turns'
import { useTranslate } from '@/i18n/provider'
import { Button } from '@/components/ui/button'
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from '@/components/ui/dialog'
import { imageDataSource, resolveMessageAttachment } from './message-attachments'
import css from './produced-files.module.css'

const DISPLAY_LIMIT = 6

export function basename(path: string) {
  const index = Math.max(path.lastIndexOf('/'), path.lastIndexOf('\\'))
  return index < 0 ? path : path.slice(index + 1)
}

export function fitProducedFiles(available: number, gap: number, chips: number[], remainder: number[], total = chips.length) {
  if (available <= 0) return chips.length
  let width = 0
  let fit = 0
  for (let shown = 0; shown <= chips.length; shown++) {
    const hidden = total - shown
    const more = hidden > 0 ? remainder[shown] ?? 0 : 0
    const items = shown + (hidden > 0 ? 1 : 0)
    if (width + more + Math.max(0, items - 1) * gap <= available) fit = shown
    width += chips[shown] ?? 0
  }
  return fit
}

function ProducedFilePreview({ sessionId, deliverable, onClose }: {
  sessionId: string
  deliverable: ChatTurnDeliverable | null
  onClose(): void
}) {
  const t = useTranslate('chat')
  const [attempt, setAttempt] = React.useState(0)
  const [state, setState] = React.useState<{ status: 'loading' | 'ready' | 'error'; content?: string; mediaType?: string }>({ status: 'loading' })
  React.useEffect(() => {
    if (!deliverable) return
    let active = true
    setState({ status: 'loading' })
    void resolveMessageAttachment(deliverable.attachment, sessionId).then(value => {
      if (active) setState({ status: 'ready', content: value.content, mediaType: value.media_type })
    }).catch(() => {
      if (active) setState({ status: 'error' })
    })
    return () => { active = false }
  }, [attempt, deliverable, sessionId])

  const attachment = deliverable && state.status === 'ready' ? {
    ...deliverable.attachment,
    content: state.content ?? '',
    media_type: state.mediaType ?? deliverable.attachment.media_type,
  } : null
  const image = attachment ? imageDataSource(attachment) : null
  return <Dialog open={deliverable !== null} onOpenChange={open => { if (!open) onClose() }}>
    <DialogContent className="grid h-[min(760px,calc(100dvh-2rem))] max-w-4xl grid-rows-[auto_minmax(0,1fr)] gap-3 p-4 max-sm:h-[calc(100dvh-1rem)] max-sm:w-[calc(100vw-1rem)]">
      <DialogHeader className={css.dialogHeader}>
        <DialogTitle>{deliverable ? basename(deliverable.path) : t('deliverable.title')}</DialogTitle>
        <DialogDescription className={css.dialogPath}>{deliverable?.path}</DialogDescription>
      </DialogHeader>
      <div className={css.preview} data-produced-file-preview="">
        {state.status === 'loading' && <div className={css.previewState} data-produced-file-state="loading" role="status"><LoaderCircle className="animate-spin" />{t('deliverable.loading')}</div>}
        {state.status === 'error' && <div className={css.previewState} data-produced-file-state="error" role="alert"><span>{t('deliverable.failed')}</span><Button type="button" variant="outline" size="sm" onClick={() => setAttempt(value => value + 1)}><RotateCcw />{t('deliverable.retry')}</Button></div>}
        {state.status === 'ready' && image && <img className={css.previewImage} data-produced-file-state="ready" data-produced-file-image="" src={image} alt={deliverable ? basename(deliverable.path) : t('deliverable.image')} />}
        {state.status === 'ready' && !image && <pre className={css.previewText} data-produced-file-state={state.content ? 'ready' : 'empty'} data-produced-file-text="">{state.content || t('deliverable.empty')}</pre>}
      </div>
    </DialogContent>
  </Dialog>
}

export function ProducedFiles({ sessionId, deliverables }: { sessionId: string; deliverables: ChatTurnDeliverable[] }) {
  const t = useTranslate('chat')
  const values = React.useMemo(() => deliverables.slice(0, DISPLAY_LIMIT), [deliverables])
  const [shown, setShown] = React.useState(values.length)
  const [preview, setPreview] = React.useState<ChatTurnDeliverable | null>(null)
  const [listOpen, setListOpen] = React.useState(false)
  const rowRef = React.useRef<HTMLDivElement>(null)
  const chipRefs = React.useRef<Array<HTMLButtonElement | null>>([])
  const moreRef = React.useRef<HTMLSpanElement>(null)

  React.useLayoutEffect(() => {
    const row = rowRef.current
    const more = moreRef.current
    if (!row || !more) return
    const measure = () => {
      const gap = Number.parseFloat(getComputedStyle(row).columnGap || getComputedStyle(row).gap) || 0
      const chips = values.map((_, index) => chipRefs.current[index]?.getBoundingClientRect().width ?? 0)
      const remainder = Array.from({ length: values.length + 1 }, (_, index) => {
        more.textContent = `+ ${deliverables.length - index}`
        return more.getBoundingClientRect().width
      })
      setShown(fitProducedFiles(row.clientWidth, gap, chips, remainder, deliverables.length))
    }
    measure()
    const observer = typeof ResizeObserver === 'undefined' ? null : new ResizeObserver(measure)
    observer?.observe(row)
    return () => observer?.disconnect()
  }, [deliverables.length, values])

  if (!deliverables.length) return null
  const visible = values.slice(0, shown)
  const hidden = deliverables.length - visible.length
  return <>
    <div className={css.root} data-produced-files="">
      <span className={css.label}><FolderOpen />{t('deliverable.files')}</span>
      <div ref={rowRef} className={css.row} data-produced-files-row="">
        {visible.map(value => <button type="button" className={css.chip} data-produced-file-chip="" title={value.path} aria-label={t('deliverable.preview', { path: value.path })} key={value.path} onClick={() => setPreview(value)}>{value.attachment.media_type.startsWith('image/') ? <ImageIcon /> : <FileText />}{basename(value.path)}</button>)}
        {hidden > 0 && <button type="button" className={css.moreButton} aria-label={t('deliverable.showAll', { count: deliverables.length })} onClick={() => setListOpen(true)}>+ {hidden}</button>}
      </div>
      <div className={css.measure} aria-hidden="true">
        {values.map((value, index) => <button ref={node => { chipRefs.current[index] = node }} type="button" tabIndex={-1} className={css.chip} key={value.path}>{basename(value.path)}</button>)}
        <span ref={moreRef} className={css.more} />
      </div>
    </div>
    <Dialog open={listOpen} onOpenChange={setListOpen}>
      <DialogContent className="grid max-h-[min(720px,calc(100dvh-1rem))] max-w-xl grid-rows-[auto_minmax(0,1fr)] gap-3 overflow-hidden p-4 max-sm:w-[calc(100vw-1rem)]">
        <DialogHeader className={css.dialogHeader}>
          <DialogTitle>{t('deliverable.allTitle')}</DialogTitle>
          <DialogDescription>{t('deliverable.allDescription', { count: deliverables.length })}</DialogDescription>
        </DialogHeader>
        <div className={css.fileList} data-produced-files-list="">
          {deliverables.map((value, index) => (
            <button
              type="button"
              className={css.fileRow}
              aria-label={t('deliverable.preview', { path: value.path })}
              title={value.path}
              key={`${value.path}:${index}`}
              onClick={() => { setListOpen(false); setPreview(value) }}
            >
              {value.attachment.media_type.startsWith('image/') ? <ImageIcon /> : <FileText />}
              <span>{basename(value.path)}</span>
              <code>{value.path}</code>
            </button>
          ))}
        </div>
      </DialogContent>
    </Dialog>
    <ProducedFilePreview sessionId={sessionId} deliverable={preview} onClose={() => setPreview(null)} />
  </>
}
