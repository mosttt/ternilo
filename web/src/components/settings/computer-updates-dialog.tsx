import * as React from 'react'
import { Download, LoaderCircle, RefreshCw } from 'lucide-react'
import { api } from '@/api/client'
import { randomUuid } from '@/lib/random-id'
import { useTranslate } from '@/i18n/provider'
import type { ManagedExecutionTarget } from '@/types'
import { Button } from '@/components/ui/button'
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from '@/components/ui/dialog'

import { ComputerUpdateStatus, type UpdateStatus } from './computer-update-status'
type Progress = NonNullable<UpdateStatus['job']>
const phaseKeys = {
  preparing: 'updates.phase.preparing',
  downloading: 'updates.phase.downloading',
  waiting_idle: 'updates.phase.waiting_idle',
  installing: 'updates.phase.installing',
  awaiting_reconnect: 'updates.phase.awaiting_reconnect',
  completed: 'updates.phase.completed',
  failed: 'updates.phase.failed',
  rolled_back: 'updates.phase.rolled_back',
  cancelled: 'updates.phase.cancelled',
} as const
const active = (progress?: Progress | null) => progress && !['completed', 'failed', 'rolled_back', 'cancelled'].includes(progress.phase)

export function ComputerUpdatesDialog({ tenantId, computers, onClose, onStatusesChange }: {
  tenantId: string; computers: ManagedExecutionTarget[]; onClose(): void; onStatusesChange?(statuses: Record<string, UpdateStatus>, errors: Record<string, string>): void
}) {
  const t = useTranslate('settings')
  const common = useTranslate('common')
  const [statuses, setStatuses] = React.useState<Record<string, UpdateStatus>>({})
  const [errors, setErrors] = React.useState<Record<string, string>>({})
  const [selected, setSelected] = React.useState<Set<string>>(new Set())
  const [loading, setLoading] = React.useState(true)
  const [submitting, setSubmitting] = React.useState(false)
  const abort = React.useRef<AbortController | null>(null)
  const requestIds = React.useRef(new Map<string, string>())
  const polling = React.useRef(false)
  const endpoint = React.useCallback((id: string) => `/tenants/${encodeURIComponent(tenantId)}/my-computers/${encodeURIComponent(id)}/updates`, [tenantId])
  const eligible = (status?: UpdateStatus) => status?.availability === 'available' && !active(status.job) && !active(status.info?.progress)

  const refresh = React.useCallback(async (fresh: boolean, signal: AbortSignal) => {
    if (polling.current) return
    polling.current = true
    if (fresh) setLoading(true)
    try {
      const pending = [...computers]
      await Promise.all(Array.from({ length: Math.min(4, pending.length) }, async () => {
        while (pending.length && !signal.aborted) {
          const computer = pending.shift()!
          try {
            const status = await api.request<UpdateStatus>(`${endpoint(computer.executor_id)}${fresh ? '?refresh=true' : ''}`, { signal })
            if (!signal.aborted) {
              setStatuses(previous => ({ ...previous, [computer.executor_id]: status }))
              if (fresh) setErrors(previous => { const next = { ...previous }; delete next[computer.executor_id]; return next })
              if (computers.length === 1 && status.availability === 'available' && !active(status.job) && !active(status.info?.progress)) setSelected(new Set([computer.executor_id]))
            }
          } catch (cause) {
            if (!signal.aborted) setErrors(previous => ({ ...previous, [computer.executor_id]: cause instanceof Error ? cause.message : String(cause) }))
          }
        }
      }))
    } finally { polling.current = false; if (!signal.aborted) setLoading(false) }
  }, [computers, endpoint])

  React.useEffect(() => {
    const controller = new AbortController()
    abort.current = controller
    void refresh(true, controller.signal)
    return () => { controller.abort(); abort.current = null }
  }, [refresh])
  React.useEffect(() => { onStatusesChange?.(statuses, errors) }, [onStatusesChange, statuses, errors])
  const inProgress = Object.values(statuses).some(status => active(status.job) || active(status.info?.progress))
  React.useEffect(() => {
    if (!inProgress) return
    const timer = setInterval(() => { if (abort.current) void refresh(false, abort.current.signal) }, 2000)
    return () => clearInterval(timer)
  }, [inProgress, refresh])

  const start = async () => {
    const controller = abort.current
    if (!controller) return
    setSubmitting(true)
    for (const computer of computers.filter(computer => selected.has(computer.executor_id) && eligible(statuses[computer.executor_id]))) {
      const id = computer.executor_id
      const requestId = requestIds.current.get(id) ?? randomUuid()
      requestIds.current.set(id, requestId)
      try {
        const result = await api.request<{ job: Progress }>(endpoint(id), { method: 'POST', body: { id: requestId }, signal: controller.signal })
        if (controller.signal.aborted) return
        setStatuses(previous => ({ ...previous, [id]: { ...previous[id], job: result.job } }))
        setSelected(previous => { const next = new Set(previous); next.delete(id); return next })
        requestIds.current.delete(id)
      } catch (cause) {
        if (controller.signal.aborted) return
        setErrors(previous => ({ ...previous, [id]: cause instanceof Error ? cause.message : String(cause) }))
      }
    }
    if (!controller.signal.aborted) setSubmitting(false)
  }
  const cancel = async (id: string, progress: Progress) => {
    const controller = abort.current
    if (!controller) return
    try {
      await api.request(`${endpoint(id)}/${encodeURIComponent(progress.id)}`, { method: 'DELETE', signal: controller.signal })
      await refresh(false, controller.signal)
    } catch (cause) {
      if (!controller.signal.aborted) setErrors(previous => ({ ...previous, [id]: cause instanceof Error ? cause.message : String(cause) }))
    }
  }
  const selectedCount = [...selected].filter(id => eligible(statuses[id])).length
  return <Dialog open onOpenChange={open => { if (!open) onClose() }}>
    <DialogContent className="flex min-h-0 max-w-2xl flex-col" data-computer-updates="">
      <DialogHeader className="shrink-0"><DialogTitle>{t('updates.title')}</DialogTitle><DialogDescription>{t('updates.description')}</DialogDescription></DialogHeader>
      <div className="min-h-0 space-y-3 overflow-y-auto overscroll-contain" data-computer-updates-scroll="">
        {loading && <p role="status" className="flex items-center gap-2 text-sm"><LoaderCircle className="size-4 animate-spin" />{t('updates.checking')}</p>}
        {computers.length > 1 && <Button variant="ghost" size="sm" disabled={loading || submitting} onClick={() => setSelected(new Set(computers.filter(computer => eligible(statuses[computer.executor_id])).map(computer => computer.executor_id)))}>{t('updates.selectAvailable')}</Button>}
        {computers.map(computer => {
          const id = computer.executor_id, status = statuses[id], progress = active(status?.info?.progress) ? status!.info!.progress : status?.job
          return <section key={id} className="rounded-lg border p-3 text-sm" data-computer-update={id}>
            <label className="flex min-h-10 items-center gap-3">
              {computers.length > 1 && <input type="checkbox" className="size-4 shrink-0 accent-[var(--primary)]" checked={selected.has(id) && eligible(status)} disabled={!eligible(status) || submitting} onChange={event => setSelected(previous => { const next = new Set(previous); if (event.target.checked) next.add(id); else next.delete(id); return next })} />}
              <span className="min-w-0 flex-1 break-words font-medium">{computer.management.name}</span>
            </label>
            {(status || errors[id]) && <ComputerUpdateStatus status={status} error={errors[id]} />}
            {status && <>
              {active(progress) && <p className="mt-2" role="status">{t(phaseKeys[progress!.phase])}</p>}
              {progress && !active(progress) && <p className="mt-2 text-muted-foreground">{t('updates.lastResult', { result: t(phaseKeys[progress.phase]) })}</p>}
              {progress?.error && <details className="mt-2"><summary className="cursor-pointer text-muted-foreground">{t('updates.diagnostics')}</summary><p className="mt-2 break-words text-destructive">{progress.error}</p></details>}
              {status.availability === 'not_enabled' && status.info?.version !== status.target_version && <details className="mt-2"><summary className="cursor-pointer">{t('updates.enable')}</summary><p className="mt-2 leading-relaxed text-muted-foreground">{t('updates.enableHelp')}</p><pre className="mt-2 overflow-x-auto rounded bg-muted p-2 text-sm" tabIndex={0}>ternilo updates enable --help</pre></details>}
              {progress && ['downloading', 'waiting_idle'].includes(progress.phase) && <Button className="mt-2" variant="outline" size="sm" onClick={() => void cancel(id, progress)}>{t('updates.cancel')}</Button>}
            </>}
          </section>
        })}
      </div>
      <DialogFooter className="shrink-0 flex-wrap">
        <Button variant="outline" onClick={onClose}>{common('close')}</Button>
        <Button variant="outline" disabled={loading || submitting} onClick={() => { if (abort.current) { void refresh(true, abort.current.signal) } }}><RefreshCw />{t('platform.refresh')}</Button>
        <Button disabled={!selectedCount || loading || submitting} onClick={() => void start()}>{submitting ? <LoaderCircle className="animate-spin" /> : <Download />}{t('updates.start', { count: selectedCount })}</Button>
      </DialogFooter>
    </DialogContent>
  </Dialog>
}
