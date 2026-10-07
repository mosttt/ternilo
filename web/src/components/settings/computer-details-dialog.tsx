import * as React from 'react'
import { LoaderCircle, Save } from 'lucide-react'
import { ApiError } from '@/api/client'
import { Button } from '@/components/ui/button'
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from '@/components/ui/dialog'
import { Field, Input, Label, Textarea } from '@/components/ui/field'
import { useTranslate } from '@/i18n/provider'
import type { ComputerDetailsResponse } from '@/types'
import { getComputerDetails, updateComputer } from './platform-admin-api'

export function ComputerDetailsDialog({ tenantId, executorId, scope, onClose, onSaved, formatTime }: {
  tenantId: string
  executorId: string
  scope: 'owned' | 'managed'
  onClose(): void
  onSaved(): Promise<void>
  formatTime(value: number): string
}) {
  const t = useTranslate('settings')
  const common = useTranslate('common')
  const id = React.useId()
  const [response, setResponse] = React.useState<ComputerDetailsResponse | null>(null)
  const [name, setName] = React.useState('')
  const [notes, setNotes] = React.useState('')
  const [loading, setLoading] = React.useState(true)
  const [saving, setSaving] = React.useState(false)
  const [error, setError] = React.useState('')
  const [reload, setReload] = React.useState(0)
  const scrollRef = React.useCallback((scroll: HTMLDivElement | null) => {
    if (!scroll) return
    const observer = new ResizeObserver(() => {
      const focused = document.activeElement
      if (focused instanceof HTMLElement && scroll.contains(focused) && focused.matches('input, textarea')) {
        focused.scrollIntoView({ block: 'nearest', inline: 'nearest' })
      }
    })
    observer.observe(scroll)
    return () => observer.disconnect()
  }, [])
  React.useEffect(() => {
    const controller = new AbortController()
    setLoading(true)
    setError('')
    void getComputerDetails(tenantId, executorId, scope, controller.signal).then(next => {
      if (controller.signal.aborted) return
      setResponse(next)
      setName(next.details.management.name)
      setNotes(next.details.management.notes)
    }).catch(cause => {
      if (!controller.signal.aborted) setError(cause instanceof ApiError && cause.status === 403 ? t('platform.permission') : t('computers.detailsError'))
    }).finally(() => { if (!controller.signal.aborted) setLoading(false) })
    return () => controller.abort()
  }, [executorId, scope, tenantId, t, reload])

  const save = async () => {
    if (!response) return
    setSaving(true)
    setError('')
    try {
      const updated = await updateComputer(tenantId, executorId, scope, { name: name.trim(), notes, expected_revision: response.details.management.revision })
      setResponse(current => current && ({ ...current, details: { ...current.details, management: updated.management } }))
      await onSaved()
      onClose()
    } catch (cause) {
      setError(cause instanceof ApiError && cause.status === 409 ? t(cause.message.startsWith('computer name') ? 'computers.nameConflict' : 'computers.changed') : cause instanceof ApiError && cause.status === 403 ? t('platform.permission') : t('computers.saveError'))
    } finally { setSaving(false) }
  }
  const details = response?.details
  const time = (value: number | null | undefined) => value == null ? t('computers.notReported') : formatTime(value)
  const status = details?.management.suspended_at_ms != null ? t('computers.suspended') : t(response?.connected ? 'computers.online' : 'computers.offline')
  const fields: [string, React.ReactNode][] = details ? [
    [t('computers.id'), executorId],
    [t('computers.owner'), `${details.owner.username} · ${details.owner.user_id}`],
    [t('computers.project'), details.executor.project_id ?? t('computers.allProjects')],
    [t('computers.connection'), status],
    [t('computers.registrationState'), t(`computers.state.${details.executor.state}`)],
    [t('computers.enrollmentTime'), time(details.executor.enrolled_at_ms)],
    [t('computers.lastContact'), time(details.executor.last_seen_at_ms)],
    [t('computers.credentialIssued'), time(details.credential_issued_at_ms)],
    [t('computers.credentialUsed'), time(details.credential_last_used_at_ms)],
    [t('computers.workspaceCount'), details.workspace_count],
    [t('computers.sessionCount'), details.session_count],
  ] : []
  return <Dialog open onOpenChange={open => { if (!open && !saving) onClose() }}>
    <DialogContent className="flex min-h-0 max-w-2xl flex-col" data-computer-details="">
      <DialogHeader className="shrink-0"><DialogTitle>{t('computers.details')}</DialogTitle><DialogDescription>{t('computers.detailsDescription')}</DialogDescription></DialogHeader>
      <div ref={scrollRef} className="grid min-h-0 min-w-0 gap-4 overflow-y-auto overscroll-contain touch-pan-y" data-computer-details-scroll="">
      {loading ? <p role="status" className="flex items-center gap-2 text-sm text-muted-foreground"><LoaderCircle className="size-4 animate-spin" />{t('platform.loading')}</p> : details && <>
        <dl className="grid grid-cols-[minmax(6rem,auto)_minmax(0,1fr)] gap-x-4 gap-y-2 text-sm">
          {fields.map(([label, value]) => <React.Fragment key={label}><dt className="text-muted-foreground">{label}</dt><dd className="min-w-0 break-all">{value}</dd></React.Fragment>)}
        </dl>
        <Field><Label htmlFor={`${id}-name`}>{t('computers.name')}</Label><Input id={`${id}-name`} value={name} maxLength={128} required disabled={saving || details.management.removed_at_ms != null} onChange={event => setName(event.target.value)} /><p className="text-xs text-muted-foreground">{t('computers.nameDescription')}</p></Field>
        <Field><Label htmlFor={`${id}-notes`}>{t('computers.notes')}</Label><Textarea id={`${id}-notes`} value={notes} maxLength={4000} disabled={saving || details.management.removed_at_ms != null} onChange={event => setNotes(event.target.value)} /></Field>
        {details.hello && <details className="text-xs text-muted-foreground"><summary className="cursor-pointer">{t('computers.diagnostics')}</summary><dl className="mt-2 grid grid-cols-[auto_minmax(0,1fr)] gap-x-3 gap-y-1">
          <dt>{t('computers.protocol')}</dt><dd>{details.hello.protocol_version}</dd><dt>{t('computers.instance')}</dt><dd className="break-all">{details.hello.instance_nonce}</dd><dt>{t('computers.catalog')}</dt><dd className="break-all">{details.hello.catalog_revision}</dd><dt>{t('computers.capabilities')}</dt><dd className="break-all">{details.hello.capabilities.join(', ')}</dd>
        </dl></details>}
      </>}
      {error && <p role="alert" className="text-sm text-destructive">{error}</p>}
      </div>
      <DialogFooter className="shrink-0">
        <Button variant="outline" disabled={saving} onClick={onClose}>{common('cancel')}</Button>
        {!loading && error && <Button variant="outline" disabled={saving} onClick={() => setReload(value => value + 1)}>{t('platform.refresh')}</Button>}
        {details && details.management.removed_at_ms == null && <Button disabled={loading || saving || !name.trim()} onClick={() => void save()}>{saving ? <LoaderCircle className="animate-spin" /> : <Save />}{t('computers.save')}</Button>}
      </DialogFooter>
    </DialogContent>
  </Dialog>
}
