import * as React from 'react'
import { createPortal } from 'react-dom'
import { ImageOff, LoaderCircle, RotateCcw } from 'lucide-react'
import { workspaceImagePath } from '@/domain/workspace-image'
import type { Translate } from '@/i18n/runtime'
import { ImageLightbox } from '../message-attachments'
import { workspaceRequest, type WorkspacePreview } from '../workspace-panel-context'
import css from './markdown-workspace-images.module.css'

export const WorkspaceImageContext = React.createContext<{ sessionId: string; root: string; scope: string } | null>(null)

function WorkspaceImage({ sessionId, root, reference, alt, t }: {
  sessionId: string; root: string; reference: string; alt: string; t: Translate<'chat'>
}) {
  const path = workspaceImagePath(reference, root)
  const [attempt, retry] = React.useReducer(value => value + 1, 0)
  const [result, setResult] = React.useState<{ key: string; source?: string; failed?: boolean } | null>(null)
  const [expanded, setExpanded] = React.useState(false)
  const key = JSON.stringify([sessionId, path, attempt])
  const current = result?.key === key ? result : null
  React.useEffect(() => {
    if (!path) return
    const controller = new AbortController()
    void workspaceRequest<WorkspacePreview>(sessionId, { kind: 'read', path }, controller.signal).then(file => {
      if (controller.signal.aborted) return
      if (file.encoding !== 'base64' || file.truncated || !/^image\/(png|jpeg|gif|webp)$/.test(file.media_type)) {
        setResult({ key, failed: true }); return
      }
      setResult({ key, source: `data:${file.media_type};base64,${file.content}` })
    }).catch(() => { if (!controller.signal.aborted) setResult({ key, failed: true }) })
    return () => controller.abort()
  }, [key, path, sessionId])
  if (!path || current?.failed) return <span className={css.notice} role="status">
    <ImageOff aria-hidden="true" /><span>{alt || reference} · {t(path ? 'image.workspaceUnavailable' : 'image.outsideWorkspace')}</span>
    {path && <button type="button" aria-label={t('attachment.retryImage', { name: alt || path })} onClick={retry}><RotateCcw aria-hidden="true" /></button>}
  </span>
  if (!current?.source) return <span className={css.notice} role="status"><LoaderCircle className={css.spin} aria-hidden="true" />{t('image.workspaceLoading')}</span>
  return <>
    <button type="button" className={css.preview} onClick={() => setExpanded(true)} aria-label={t('image.workspaceOpen', { name: alt || path })}>
      <img src={current.source} alt={alt || path} onError={() => setResult({ key, failed: true })} />
    </button>
    {expanded && <ImageLightbox image={{ attachmentIndex: 0, name: alt || path, source: current.source }}
      position={0} count={1} onClose={() => setExpanded(false)} onPrevious={() => undefined} onNext={() => undefined} t={t} />}
  </>
}

export function MarkdownWorkspaceImages({ container, markup, t }: {
  container: React.RefObject<HTMLDivElement | null>; markup: string; t: Translate<'chat'>
}) {
  const scope = React.useContext(WorkspaceImageContext)
  const key = JSON.stringify([markup, scope?.sessionId, scope?.root, scope?.scope])
  const [targets, setTargets] = React.useState<{ key: string; nodes: HTMLElement[] }>({ key: '', nodes: [] })
  const mounted = React.useRef(new WeakSet<HTMLElement>())
  React.useEffect(() => {
    if (!scope || !container.current) return
    const next = [...container.current.querySelectorAll<HTMLElement>('[data-workspace-image]')]
    for (const node of next) {
      if (!mounted.current.has(node)) { node.replaceChildren(); mounted.current.add(node) }
    }
    setTargets({ key, nodes: next })
  }, [container, key, scope?.sessionId, scope?.root, scope?.scope])
  if (!scope || targets.key !== key) return null
  return targets.nodes.map((target, index) => createPortal(<WorkspaceImage {...scope}
    reference={target.dataset.workspaceImage!} alt={target.dataset.workspaceImageAlt ?? ''} t={t} />, target, `${scope.scope}:${scope.sessionId}:${index}`))
}
