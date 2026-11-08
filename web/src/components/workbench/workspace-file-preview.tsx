import * as React from 'react'
import DOMPurify from 'dompurify'
import { Copy, Download, LoaderCircle } from 'lucide-react'
import { renderMarkdown } from '../../../rich-text.source.js'
import { useTranslate } from '@/i18n/provider'
import { copyText } from '@/lib/clipboard'
import { downloadBlob } from '@/lib/utils'
import { api } from '@/api/client'
import { StreamingCodeBlock } from './chat/streaming-code-block'
import { useWorkspaceRead, type WorkspacePreview } from './workspace-panel-context'
import css from './workspace-panel.module.css'

export function WorkspaceFilePreview({ sessionId, path, revision }: { sessionId: string; path: string; revision: number }) {
  const t = useTranslate('files')
  const chatT = useTranslate('chat')
  const result = useWorkspaceRead<WorkspacePreview>(sessionId, { kind: 'read', path }, revision)
  const [rendered, setRendered] = React.useState(/\.(md|markdown)$/i.test(path))
  const [objectUrl, setObjectUrl] = React.useState('')
  const [error, setError] = React.useState('')
  const [downloading, setDownloading] = React.useState(false)
  const downloadRequest = React.useRef<AbortController | null>(null)
  React.useEffect(() => {
    setError(''); setDownloading(false)
    return () => { downloadRequest.current?.abort(); downloadRequest.current = null }
  }, [sessionId, path])
  const download = async () => {
    if (downloadRequest.current) return
    const controller = new AbortController()
    downloadRequest.current = controller
    setDownloading(true); setError('')
    try {
      const blob = await api.download(`/sessions/${encodeURIComponent(sessionId)}/workspace/download`, { method: 'POST', body: { path }, signal: controller.signal })
      if (!controller.signal.aborted) downloadBlob(path.split('/').at(-1)!, blob)
    } catch (cause) {
      if (!controller.signal.aborted) setError(t('downloadError', { message: cause instanceof Error ? cause.message : String(cause) }))
    } finally {
      if (downloadRequest.current === controller) {
        downloadRequest.current = null
        setDownloading(false)
      }
    }
  }
  const [palette, setPalette] = React.useState(() => ({ background: getComputedStyle(document.body).getPropertyValue('--background'), foreground: getComputedStyle(document.body).getPropertyValue('--foreground') }))
  React.useEffect(() => {
    const observer = new MutationObserver(() => setPalette({ background: getComputedStyle(document.body).getPropertyValue('--background'), foreground: getComputedStyle(document.body).getPropertyValue('--foreground') }))
    observer.observe(document.documentElement, { attributes: true, attributeFilter: ['class'] })
    return () => observer.disconnect()
  }, [])
  React.useEffect(() => {
    setObjectUrl('')
    const file = result.data
    if (!file || file.truncated || file.encoding === 'unsupported') return
    const bytes = file.encoding === 'base64' ? Uint8Array.from(atob(file.content), char => char.charCodeAt(0)) : file.content
    const url = URL.createObjectURL(new Blob([bytes], { type: file.media_type }))
    setObjectUrl(url)
    return () => URL.revokeObjectURL(url)
  }, [result.data])
  const file = result.data
  const renderable = /\.(md|markdown|html?|svg)$/i.test(path)
  const html = React.useMemo(() => {
    if (!file || file.encoding !== 'utf8' || !rendered || !renderable) return ''
    const content = /\.(md|markdown)$/i.test(path) ? renderMarkdown(file.content, { copy: chatT('message.copy'), copyCode: chatT('message.copyCode'), taskCompleted: chatT('message.taskCompleted'), taskPending: chatT('message.taskPending') }) : file.content
    const clean = DOMPurify.sanitize(content, {
      FORBID_TAGS: ['button', 'iframe', 'form', 'style', 'link', 'meta', 'base'],
      FORBID_ATTR: ['srcset', 'style'],
      ALLOWED_URI_REGEXP: /^(?:#|data:image\/(?:png|jpeg|gif|webp);base64,)/i,
    })
    return `<!doctype html><html><head><meta charset="utf-8"><meta http-equiv="Content-Security-Policy" content="default-src 'none'; img-src data:; style-src 'unsafe-inline'; form-action 'none'; base-uri 'none'"><style>html{color-scheme:light dark}body{margin:18px;background:${palette.background};color:${palette.foreground};font:14px/1.7 system-ui;overflow-wrap:anywhere}pre{overflow:auto;padding:12px;background:rgba(128,128,128,.08)}code{font-family:monospace}img{max-width:100%}table{border-collapse:collapse}td,th{border:1px solid #888;padding:6px}a{color:#679efe}</style></head><body>${clean}</body></html>`
  }, [file, rendered, renderable, path, palette, chatT])
  if (result.error) return <div className={css.note} role="alert">{result.error}</div>
  if (!file) return <div className={css.note}>{t('previewLoading')}</div>
  return <div className={css.document} data-workspace-preview={path}>
    <div className={css.previewToolbar}>
      <span>{t('workspace.bytes', { count: file.bytes })}</span>
      {renderable && file.encoding === 'utf8' && <button type="button" onClick={() => setRendered(value => !value)}>{t(rendered ? 'workspace.source' : 'workspace.render')}</button>}
      {file.encoding === 'utf8' && <button type="button" aria-label={t('workspace.copy')} onClick={() => void copyText(file.content).catch(cause => setError(String(cause)))}><Copy /></button>}
      <button type="button" disabled={downloading} onClick={() => void download()}>{downloading ? <LoaderCircle className={css.spinner} /> : <Download />}{t(downloading ? 'downloading' : 'download')}</button>
      {downloading && <button type="button" onClick={() => downloadRequest.current?.abort()}>{t('cancelDownload')}</button>}
    </div>
    {error && <div role="alert" className={css.note}>{error}</div>}
    {file.truncated && <div className={css.note}>{t('workspace.previewTruncated')}</div>}
    {file.encoding === 'utf8' ? rendered && renderable
      ? <><div className={css.safety}>{t('workspace.safePreview')}</div><iframe className={css.documentFrame} title={t('preview', { name: path })} sandbox="" srcDoc={html} /></>
      : <div className={css.code} onClick={event => { if ((event.target as HTMLElement).closest('.markdown-copy')) void copyText(file.content).catch(cause => setError(String(cause))) }}><StreamingCodeBlock code={file.content} lang={path.split('.').at(-1)} copyLabel={t('workspace.copy')} copyAria={t('workspace.copy')} /></div>
      : file.media_type.startsWith('image/') && objectUrl ? <div className={css.image}><img src={objectUrl} alt={path} /></div>
      : file.media_type === 'application/pdf' && objectUrl ? <iframe className={css.documentFrame} title={t('preview', { name: path })} sandbox="" src={objectUrl} />
      : <div className={css.note}>{t('workspace.unsupported')}</div>}
  </div>
}
