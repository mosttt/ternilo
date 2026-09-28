import * as React from 'react'
import { Play, RefreshCw, Square } from 'lucide-react'
import type { LocalSession, SessionServiceSnapshot } from '@/types'
import { api } from '@/api/client'
import { resourcePermissions } from '@/domain/resource-access'
import { Button } from '@/components/ui/button'
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from '@/components/ui/dialog'
import { useTranslate } from '@/i18n/provider'
import css from './session-services-dialog.module.css'

export function SessionServicesDialog({ session, readOnly = false, onClose }: {
  session: LocalSession
  readOnly?: boolean
  onClose(): void
}) {
  const t = useTranslate('conversation')
  const cloud = session.placement === 'cloud'
  const permissions = resourcePermissions(session.access, !readOnly)
  const statuses: Record<SessionServiceSnapshot['status'], string> = {
    idle: t('services.status.idle'), starting: t('services.status.starting'), running: t('services.status.running'),
    stopping: t('services.status.stopping'), stopped: t('services.status.stopped'), failed: t('services.status.failed'),
  }
  const path = `/sessions/${encodeURIComponent(session.identity.session_id)}/services`
  const [services, setServices] = React.useState<SessionServiceSnapshot[] | null>(null)
  const [error, setError] = React.useState('')
  const [pending, setPending] = React.useState<Record<string, 'start' | 'stop'>>({})
  const mounted = React.useRef(true)
  const readVersion = React.useRef(0)
  const actions = React.useRef(new Map<string, number>())
  const refresh = React.useCallback(async (signal?: AbortSignal) => {
    const version = ++readVersion.current
    try {
      const result = await api.request<SessionServiceSnapshot[]>(path, { signal })
      if (mounted.current && !signal?.aborted && version === readVersion.current) setServices(result)
    } catch (cause) {
      if (mounted.current && !signal?.aborted && version === readVersion.current) setError(cause instanceof Error ? cause.message : String(cause))
    }
  }, [path])
  React.useEffect(() => {
    mounted.current = true
    const controller = new AbortController()
    let timer: ReturnType<typeof setTimeout>
    const poll = async () => {
      await refresh(controller.signal)
      if (!controller.signal.aborted) timer = setTimeout(() => void poll(), 1_000)
    }
    void poll()
    return () => { mounted.current = false; controller.abort(); clearTimeout(timer) }
  }, [refresh])
  const change = async (service: SessionServiceSnapshot, action: 'start' | 'stop') => {
    if (action === 'start' ? !permissions.submit : !permissions.stop) return
    const revision = (actions.current.get(service.id) ?? 0) + 1
    actions.current.set(service.id, revision)
    readVersion.current += 1
    setPending(current => ({ ...current, [service.id]: action }))
    setError('')
    // A second stop request remains available while a startup request is pending.
    setServices(current => current?.map(item => item.id === service.id
      ? { ...item, status: action === 'start' ? 'starting' : 'stopping' } : item) ?? null)
    try {
      await api.request(`${path}/${encodeURIComponent(service.id)}/${action}`, { method: 'POST' })
    } catch (cause) {
      if (mounted.current && actions.current.get(service.id) === revision) setError(cause instanceof Error ? cause.message : String(cause))
    } finally {
      if (mounted.current && actions.current.get(service.id) === revision) {
        setPending(current => { const next = { ...current }; delete next[service.id]; return next })
        await refresh()
      }
    }
  }
  return <Dialog open onOpenChange={open => { if (!open) onClose() }}>
    <DialogContent className={css.dialog}>
      <DialogHeader>
        <DialogTitle>{t('services.title')}</DialogTitle>
        <DialogDescription>{t(cloud ? 'services.cloudDescription' : 'services.description')}</DialogDescription>
      </DialogHeader>
      <div className={css.toolbar}>
        <span>{t(cloud ? 'services.cloudScope' : 'services.scope')}</span>
        <Button variant="ghost" size="sm" onClick={() => void refresh()} aria-label={t('services.refresh')}><RefreshCw size={14} />{t('services.refresh')}</Button>
      </div>
      {error && <p className={css.error} role="alert">{error}</p>}
      {services === null ? <p className={css.empty} role="status">{t('services.loading')}</p>
        : services.length === 0 ? <p className={css.empty}>{t('services.empty')}</p>
          : <ul className={css.list}>{services.map(service => {
            const canStop = service.status === 'running' || service.status === 'starting'
            const stopping = service.status === 'stopping' || pending[service.id] === 'stop'
            const starting = service.status === 'starting' || pending[service.id] === 'start'
            const failed = service.status === 'failed' && !starting && !stopping
            return <li key={service.id} className={css.service} data-session-service={service.id}>
              <div className={css.row}>
                <div className={css.identity}><strong>{service.name}</strong><span>{service.kind === 'mcp' ? 'MCP' : t('services.languageServer')}</span></div>
                <span className={css.status} data-status={service.status}>{statuses[service.status]}</span>
              </div>
              {service.error && <p className={css.error}>{service.error}</p>}
              <div className={`${css.row} ${css.controlsRow}`}>
                <p className={css.hint}>{service.active_calls > 0 ? t('services.activeCall')
                  : service.status === 'running' ? t(cloud ? 'services.cloudRunningHint' : 'services.runningHint')
                    : starting ? t('services.startingHint')
                    : failed ? t('services.failedHint')
                      : service.status === 'stopped' ? t('services.stoppedHint') : t('services.startHint')}</p>
                <div className={css.controls}>
                  {(canStop || starting || stopping || failed) && <Button variant="outline" size="sm" disabled={!permissions.stop || stopping || service.active_calls > 0} onClick={() => void change(service, 'stop')}><Square size={13} />{t('services.stop')}</Button>}
                  {!canStop && !starting && !stopping && <Button variant="outline" size="sm" disabled={!permissions.submit || service.active_calls > 0} onClick={() => void change(service, 'start')}><Play size={13} />{t('services.start')}</Button>}
                </div>
              </div>
            </li>
          })}</ul>}
    </DialogContent>
  </Dialog>
}
