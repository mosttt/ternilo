import { copyText } from '@/lib/clipboard'
import * as React from 'react'
import { Copy } from 'lucide-react'
import { api } from '@/api/client'
import type { Workspace } from '@/types'
import { abbreviateHomePath } from '@/domain/workspace-lifecycle'
import { useTranslate } from '@/i18n/provider'
import css from './workspace-rows.module.css'

interface WorkspaceLocation {
  status: 'available' | 'offline' | 'unavailable'
  path: string | null
  home: string | null
  created_at_ms: number | null
}

export function workspacePlacementLabel(workspace: Workspace, t: ReturnType<typeof useTranslate<'workspace'>>) {
  if (workspace.placement === 'cloud') return t('placement.cloud')
  return workspace.node_id ? t('placement.computer', { name: workspace.node_id }) : t('placement.local')
}

function createdLabel(createdAt: number, added: boolean, t: ReturnType<typeof useTranslate<'workspace'>>) {
  const date = new Date(createdAt)
  const pad2 = (value: number) => String(value).padStart(2, '0')
  const day = t('date.ymd', { y: date.getFullYear(), m: date.getMonth() + 1, d: date.getDate() })
  const time = `${day} ${pad2(date.getHours())}:${pad2(date.getMinutes())}`
  return added ? t('hover.added', { time }) : t('hover.created', { time })
}

/** Mount only while the location view is open; remote paths stay in this component. */
export function WorkspaceLocationDetails({ workspace, platform }: { workspace: Workspace; platform: boolean }) {
  const t = useTranslate('workspace')
  const online = workspace.status !== 'offline' && workspace.status !== 'error'
  const remote = platform && workspace.placement === 'local_node'
  const canRead = remote && workspace.access?.is_owner !== false && online
  const [location, setLocation] = React.useState<WorkspaceLocation | null>(null)
  const [copyState, setCopyState] = React.useState<'idle' | 'copied' | 'failed'>('idle')
  React.useEffect(() => {
    if (!canRead) return
    const controller = new AbortController()
    void api.request<WorkspaceLocation>(`/workspaces/${encodeURIComponent(workspace.workspace_id)}/location`, {
      signal: controller.signal, cache: 'no-store',
    }).then(result => {
      if (!controller.signal.aborted) setLocation(result)
    }).catch(() => {
      if (!controller.signal.aborted) setLocation({ status: 'unavailable', path: null, home: null, created_at_ms: null })
    })
    return () => controller.abort()
  }, [canRead, workspace.workspace_id])
  const resolved = canRead && location?.status === 'available' ? location : null
  const path = !platform ? workspace.path : resolved?.path
  const home = !platform ? window.__TERNILO_BOOT__?.home : resolved?.home
  const sourceCreatedAt = remote ? resolved?.created_at_ms : null
  const unavailable = remote
    ? workspace.access?.is_owner === false ? t('location.ownerOnly')
      : !online || location?.status === 'offline' ? t('location.offline')
        : location === null ? t('location.loading') : t('location.unavailable')
    : t('location.cloud')
  return <div className={css.hoverContent} data-workspace-location="">
    <strong className={css.hoverTitle}>{workspace.title}</strong>
    {path ? <button
      type="button"
      className={css.pathButton}
      aria-label={t('path.copy', { path })}
      onClick={() => {
        void copyText(path).then(() => setCopyState('copied'), () => setCopyState('failed'))
      }}
    ><span className={css.hoverPath}>{abbreviateHomePath(path, home ?? undefined)}</span><Copy size={14} aria-hidden="true" /></button>
      : <span className={css.locationHint} data-workspace-location-status="">{unavailable}</span>}
    <span className={css.hoverMeta}>{createdLabel(sourceCreatedAt ?? workspace.created_at_ms, platform && sourceCreatedAt == null, t)}</span>
    <span className={css.hoverMeta}>{workspacePlacementLabel(workspace, t)} · {online && location?.status !== 'offline' ? t('status.online') : t('status.offline')}</span>
    {copyState !== 'idle' && <span role="status" className={`${css.copyFeedback} ${copyState === 'failed' ? 'text-destructive' : 'text-primary'}`}>
      {copyState === 'copied' ? t('path.copied') : t('path.copyFailed')}
    </span>}
  </div>
}
