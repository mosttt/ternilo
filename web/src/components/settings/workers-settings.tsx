import { copyText } from '@/lib/clipboard'
import * as React from 'react'
import { Check, Copy, LoaderCircle, RefreshCw, Server, ShieldX } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from '@/components/ui/dialog'
import { Field, Input, Label } from '@/components/ui/field'
import { useLocale, useTranslate } from '@/i18n/provider'
import { useWorkbench } from '@/state/workbench'
import { ActionDialog, GroupHeader, SectionHeader } from './settings-ui'
import { createWorker, listWorkers, revokeWorker, workerSetupCommand, type WorkerGrant, type WorkerRecord } from './workers-api'
import styles from './platform-settings.module.css'
import { canManageWorkers, isPlatformStaff } from '@/components/admin/admin-api'
import { ExecutionPanel } from '@/components/admin/execution-panel'

export function WorkersSettings() {
  const { serverIdentity } = useWorkbench()
  const t = useTranslate('admin')
  const settingsT = useTranslate('settings')
  const heading = <SectionHeader title={settingsT('workers.title')} description={settingsT('workers.description')} />
  if (!isPlatformStaff(serverIdentity?.platform_role)) return null
  if (!serverIdentity?.instance.managed_execution_enabled) return <div className="grid gap-6 [&>header]:mb-0">{heading}<section className="rounded-xl border bg-card p-5" data-workers-disabled="">
    <GroupHeader title={t('workers.disabledTitle')} description={t('workers.disabledDescription')} />
    <p className="text-sm leading-relaxed text-muted-foreground">{t('workers.disabledSteps')}</p>
  </section></div>
  const editable = canManageWorkers(serverIdentity.platform_role)
  return <div className="grid gap-6 [&>header]:mb-0">{heading}<ExecutionPanel editable={editable} /><WorkersPanel key={serverIdentity.user.user_id} editable={editable} /></div>
}

function WorkersPanel({ editable }: { editable: boolean }) {
  const t = useTranslate('settings')
  const commonT = useTranslate('common')
  const { locale } = useLocale()
  const [workers, setWorkers] = React.useState<WorkerRecord[]>([])
  const [workerId, setWorkerId] = React.useState('')
  const [storageId, setStorageId] = React.useState('')
  const [advanced, setAdvanced] = React.useState(false)
  const [loading, setLoading] = React.useState(true)
  const [revision, setRevision] = React.useState(0)
  const [loadError, setLoadError] = React.useState('')
  const [actionError, setActionError] = React.useState('')
  const [busy, setBusy] = React.useState(false)
  const [grant, setGrant] = React.useState<WorkerGrant | null>(null)
  const [copied, setCopied] = React.useState<'setup' | 'serve' | null>(null)
  const [copyError, setCopyError] = React.useState('')
  const [revokeTarget, setRevokeTarget] = React.useState<WorkerRecord | null>(null)

  React.useEffect(() => {
    const controller = new AbortController()
    let active = true
    const load = async () => {
      try {
        const records = await listWorkers(controller.signal)
        if (active) { setWorkers(records); setLoadError('') }
      } catch (cause) {
        if (active) setLoadError(cause instanceof Error ? cause.message : String(cause))
      } finally { if (active) setLoading(false) }
    }
    setLoading(true)
    void load()
    const timer = window.setInterval(() => {
      if (document.visibilityState === 'visible') void load()
    }, 10_000)
    return () => { active = false; controller.abort(); window.clearInterval(timer) }
  }, [revision])

  const generate = async () => {
    if (!editable || !workerId.trim() || busy) return
    setBusy(true)
    setActionError('')
    try {
      setGrant(await createWorker(workerId.trim(), advanced ? storageId.trim() || undefined : undefined))
      setCopied(null)
      setCopyError('')
      setWorkerId('')
      setStorageId('')
      setRevision(value => value + 1)
    } catch (cause) { setActionError(cause instanceof Error ? cause.message : String(cause)) }
    finally { setBusy(false) }
  }

  const revoke = async () => {
    if (!editable || !revokeTarget || busy) return
    setBusy(true)
    setActionError('')
    try {
      await revokeWorker(revokeTarget.worker_id)
      setRevokeTarget(null)
      setRevision(value => value + 1)
    } catch (cause) { setActionError(cause instanceof Error ? cause.message : String(cause)) }
    finally { setBusy(false) }
  }

  const closeGrant = () => { setGrant(null); setCopied(null); setCopyError('') }
  const setupCommand = grant ? workerSetupCommand(window.location.origin, grant.token) : ''
  const copy = async (kind: 'setup' | 'serve') => {
    setCopyError('')
    try {
      await copyText(kind === 'setup' ? setupCommand : 'ternilo-worker serve')
      setCopied(kind)
    } catch { setCopyError(t('command.copyFailed')) }
  }
  const formatTime = (value: number) => new Intl.DateTimeFormat(locale === 'zh' ? 'zh-CN' : 'en', {
    dateStyle: 'medium', timeStyle: 'short',
  }).format(new Date(value))

  return (
    <section className="rounded-xl border bg-card p-5" data-instance-workers="">
      {editable && <form className="grid gap-3" onSubmit={event => { event.preventDefault(); void generate() }}>
        <div className="flex flex-wrap items-end gap-3">
          <Field className="min-w-40 flex-1">
            <Label htmlFor="instance-worker-id">{t('workers.id')}</Label>
            <Input id="instance-worker-id" autoComplete="off" placeholder="worker-01" value={workerId} disabled={busy} onChange={event => setWorkerId(event.target.value)} />
          </Field>
          <Button type="submit" disabled={busy || !workerId.trim()}>
            {busy ? <LoaderCircle className="animate-spin" /> : <Server />}{t('workers.generate')}
          </Button>
        </div>
        <details open={advanced} onToggle={event => setAdvanced(event.currentTarget.open)}>
          <summary className="w-fit cursor-pointer text-xs text-muted-foreground">{t('workers.advanced')}</summary>
          <Field className="mt-3">
            <Label htmlFor="instance-worker-storage">{t('workers.storage')}</Label>
            <Input id="instance-worker-storage" autoComplete="off" placeholder={t('workers.storageDefault')} value={storageId} disabled={busy} onChange={event => setStorageId(event.target.value)} />
            <p className="text-xs leading-relaxed text-muted-foreground">{t('workers.storageDescription')}</p>
          </Field>
        </details>
      </form>}
      {actionError && !revokeTarget && <p className="mt-3 text-sm text-destructive" role="alert">{actionError}</p>}
      <div className="mt-7 flex items-center justify-between gap-3">
        <h4 className="text-sm font-semibold">{t('workers.list')}</h4>
        <Button variant="ghost" size="sm" disabled={loading} onClick={() => setRevision(value => value + 1)}>
          <RefreshCw className={loading ? 'animate-spin' : ''} />{t('platform.refresh')}
        </Button>
      </div>
      {loadError && <p className="mt-3 text-sm text-destructive" role="alert">{loadError}</p>}
      {loading && !workers.length ? <p className="mt-4 text-sm text-muted-foreground" role="status">{t('platform.loading')}</p>
        : !workers.length ? <p className="mt-4 text-sm text-muted-foreground">{t('workers.empty')}</p>
          : <div className={`${styles.list} mt-3`}>
            {workers.map(worker => {
              const state = worker.revoked_at_ms !== null ? 'revoked' : worker.online ? 'online' : worker.registered ? 'offline' : 'pending'
              return <article className={styles.row} key={worker.worker_id} data-worker-id={worker.worker_id}>
                <div className={styles.identity}>
                  <strong>{worker.worker_id}</strong>
                  <details className={`${styles.metadata} mt-1`}>
                    <summary className="w-fit cursor-pointer">{t('workers.details')}</summary>
                    <p className="mt-1">{t('workers.storage')}: <code>{worker.storage_id}</code></p>
                    <p>{t('workers.created', { time: formatTime(worker.created_at_ms) })}</p>
                  </details>
                </div>
                <div>
                  <span className={styles.status} data-online={String(state === 'online')}>{t(state === 'revoked' ? 'workers.state.revoked' : state === 'online' ? 'workers.state.online' : state === 'offline' ? 'workers.state.offline' : 'workers.state.pending')}</span>
                  <p className={styles.metadata}>{worker.last_seen_at_ms === null ? t('workers.never') : t('workers.lastSeen', { time: formatTime(worker.last_seen_at_ms) })}</p>
                </div>
                <div className={styles.actions}>
                  {editable && <Button variant="outline" size="sm" className="text-destructive hover:text-destructive" disabled={state === 'revoked' || busy} onClick={() => { setActionError(''); setRevokeTarget(worker) }}>
                    <ShieldX />{t('workers.revoke')}
                  </Button>}
                </div>
              </article>
            })}
          </div>}

      <Dialog open={grant !== null} onOpenChange={open => { if (!open) closeGrant() }}>
        <DialogContent className="max-w-2xl grid-rows-[auto_minmax(0,1fr)_auto]" data-worker-launch-dialog="">
          <DialogHeader>
            <DialogTitle>{t('workers.commandTitle', { name: grant?.worker_id ?? '' })}</DialogTitle>
            <DialogDescription>{t('workers.commandDescription')}</DialogDescription>
          </DialogHeader>
          {grant && <div className="grid min-h-0 min-w-0 grid-cols-1 gap-4 overflow-y-auto">
            <p className={styles.notice}>{t('workers.commandOnce')}</p>
            <div>
              <div className="mb-2 flex items-center justify-between gap-3">
                <h4 className="text-sm font-medium">{t('workers.setup')}</h4>
                <Button variant="outline" size="sm" onClick={() => void copy('setup')}>{copied === 'setup' ? <Check /> : <Copy />}{t(copied === 'setup' ? 'command.copied' : 'workers.copySetup')}</Button>
              </div>
              <pre className={styles.command} tabIndex={0} aria-label={t('workers.setup')} data-worker-setup-command="">{setupCommand}</pre>
            </div>
            <div>
              <div className="mb-2 flex items-center justify-between gap-3">
                <h4 className="text-sm font-medium">{t('workers.serve')}</h4>
                <Button variant="outline" size="sm" onClick={() => void copy('serve')}>{copied === 'serve' ? <Check /> : <Copy />}{t(copied === 'serve' ? 'command.copied' : 'workers.copyServe')}</Button>
              </div>
              <pre className={styles.command} tabIndex={0} aria-label={t('workers.serve')}>ternilo-worker serve</pre>
            </div>
            {copyError && <p className="text-sm text-destructive" role="alert">{copyError}</p>}
          </div>}
          <DialogFooter><Button onClick={closeGrant}>{t('command.done')}</Button></DialogFooter>
        </DialogContent>
      </Dialog>
      <ActionDialog
        open={revokeTarget !== null}
        title={t('workers.revokeTitle')}
        description={t('workers.revokeDescription', { name: revokeTarget?.worker_id ?? '' })}
        cancelLabel={commonT('cancel')}
        confirmLabel={t('workers.revoke')}
        busy={busy}
        destructive
        error={actionError}
        onOpenChange={open => { if (!open) setRevokeTarget(null) }}
        onConfirm={() => void revoke()}
      />
    </section>
  )
}
