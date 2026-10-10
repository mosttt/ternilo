import { copyText } from '@/lib/clipboard'
import * as React from 'react'
import { Check, Copy, Info, Laptop, LoaderCircle, Pause, Play, RefreshCw, ShieldX, Trash2 } from 'lucide-react'
import { ApiError } from '@/api/client'
import { Button } from '@/components/ui/button'
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from '@/components/ui/dialog'
import { Field, Input, Label, Select } from '@/components/ui/field'
import { useLocale, useTranslate } from '@/i18n/provider'
import { localeTag } from '@/i18n/runtime'
import type { ManagedExecutionTarget, ProjectRecord } from '@/types'
import { ActionDialog, GroupHeader, SectionHeader } from './settings-ui'
import { ComputerDetailsDialog } from './computer-details-dialog'
import {
  createNodeLaunch,
  createOwnedNodeLaunch,
  listManagedComputers,
  listOwnedComputers,
  listPlatformProjects,
  revokeComputer,
  revokeOwnedComputer,
  setComputerSuspended,
  removeComputerRegistration,
  recoverNodeLaunch,
  type NodeLaunchCommand,
} from './platform-admin-api'
import styles from './platform-settings.module.css'

function errorCopy(
  cause: unknown,
  action: 'load' | 'enrollment' | 'revoke',
  t: ReturnType<typeof useTranslate<'settings'>>,
) {
  if (cause instanceof ApiError && cause.status === 403) return t('platform.permission')
  if (action === 'enrollment' && cause instanceof ApiError && cause.status === 409) return t('computers.nameConflict')
  return t(action === 'load'
    ? 'computers.loadError'
    : action === 'enrollment'
      ? 'computers.enrollmentError'
      : 'computers.revokeError')
}

export function PlatformComputersSettings({
  tenantId,
  scope = 'managed',
}: {
  tenantId: string
  scope?: 'managed' | 'owned'
}) {
  return <ComputerSettingsContents key={`${scope}:${tenantId}`} tenantId={tenantId} scope={scope} />
}

function ComputerSettingsContents({ tenantId, scope }: { tenantId: string; scope: 'managed' | 'owned' }) {
  const t = useTranslate('settings')
  const commonT = useTranslate('common')
  const { locale } = useLocale()
  const [computers, setComputers] = React.useState<ManagedExecutionTarget[]>([])
  const [projects, setProjects] = React.useState<ProjectRecord[]>([])
  const [loading, setLoading] = React.useState(true)
  const [error, setError] = React.useState('')
  const [message, setMessage] = React.useState('')
  const [computerName, setComputerName] = React.useState('')
  const [includeRemoved, setIncludeRemoved] = React.useState(false)
  const [recoveryTarget, setRecoveryTarget] = React.useState<ManagedExecutionTarget | null>(null)
  const [recoveryName, setRecoveryName] = React.useState('')
  const [recovering, setRecovering] = React.useState(false)
  const [recoveryError, setRecoveryError] = React.useState('')
  const [projectId, setProjectId] = React.useState('')
  const [generating, setGenerating] = React.useState(false)
  const [launch, setLaunch] = React.useState<NodeLaunchCommand | null>(null)
  const [copied, setCopied] = React.useState(false)
  const [copyError, setCopyError] = React.useState('')
  const [revokeTarget, setRevokeTarget] = React.useState<ManagedExecutionTarget | null>(null)
  const [revoking, setRevoking] = React.useState(false)
  const [detailsTarget, setDetailsTarget] = React.useState<string | null>(null)
  const [lifecycleTarget, setLifecycleTarget] = React.useState<{ computer: ManagedExecutionTarget; action: 'suspend' | 'resume' | 'remove' } | null>(null)
  const [changing, setChanging] = React.useState(false)
  const owned = scope === 'owned'
  const Heading = owned ? SectionHeader : GroupHeader
  const listState = loading && !computers.length
    ? 'loading'
    : error && !computers.length
      ? 'error'
      : computers.length
        ? 'ready'
        : 'empty'

  const formatTime = React.useCallback((value: number) => new Intl.DateTimeFormat(
    localeTag(locale),
    { dateStyle: 'medium', timeStyle: 'short' },
  ).format(new Date(value)), [locale])

  const load = React.useCallback(async () => {
    setLoading(true)
    setError('')
    try {
      const [nextComputers, nextProjects] = await Promise.all([
        owned ? listOwnedComputers(tenantId, includeRemoved) : listManagedComputers(tenantId, includeRemoved),
        listPlatformProjects(),
      ])
      setComputers(nextComputers)
      setProjects(nextProjects)
      setProjectId(current => nextProjects.some(project => project.project_id === current) ? current : '')
    } catch (cause) {
      setError(errorCopy(cause, 'load', t))
    } finally {
      setLoading(false)
    }
  }, [owned, t, tenantId, includeRemoved])

  React.useEffect(() => { void load() }, [load])

  const generate = async () => {
    const normalized = computerName.trim()
    if (!normalized) return
    setGenerating(true)
    setError('')
    setMessage('')
    try {
      const next = await (owned ? createOwnedNodeLaunch : createNodeLaunch)(tenantId, {
        name: normalized,
        projectId: projectId || undefined,
      })
      setLaunch(next)
      setComputerName('')
      await load()
    } catch (cause) {
      setError(errorCopy(cause, 'enrollment', t))
    } finally {
      setGenerating(false)
    }
  }

  const revoke = async () => {
    if (!revokeTarget) return
    setRevoking(true)
    setError('')
    setMessage('')
    try {
      await (owned ? revokeOwnedComputer : revokeComputer)(tenantId, revokeTarget.executor_id)
      setRevokeTarget(null)
      await load()
      setMessage(t('computers.revoked'))
    } catch (cause) {
      setError(errorCopy(cause, 'revoke', t))
    } finally {
      setRevoking(false)
    }
  }

  const closeLaunch = () => {
    setLaunch(null)
    setCopied(false)
    setCopyError('')
  }

  const recover = async () => {
    if (!recoveryTarget || !recoveryName.trim()) return
    setRecovering(true)
    setRecoveryError('')
    try {
      const next = await recoverNodeLaunch(tenantId,recoveryTarget,scope,recoveryName.trim())
      setRecoveryTarget(null)
      setLaunch(next)
      await load()
    } catch (cause) {
      setRecoveryError(cause instanceof ApiError && cause.status === 409 ? t(cause.message.startsWith('computer name') ? 'computers.nameConflict' : 'computers.changed') : errorCopy(cause,'enrollment',t))
    } finally { setRecovering(false) }
  }

  const changeLifecycle = async () => {
    if (!lifecycleTarget) return
    setChanging(true)
    setError('')
    setMessage('')
    const { computer, action } = lifecycleTarget
    try {
      if (action === 'remove') await removeComputerRegistration(tenantId, computer.executor_id, scope, computer.management.revision)
      else await setComputerSuspended(tenantId, computer.executor_id, scope, { suspended: action === 'suspend', expected_revision: computer.management.revision })
      setLifecycleTarget(null)
      await load()
      setMessage(t(action === 'remove' ? 'computers.removed' : action === 'suspend' ? 'computers.paused' : 'computers.resumed'))
    } catch (cause) {
      setLifecycleTarget(null)
      setError(cause instanceof ApiError && cause.status === 409 ? t('computers.changed') : cause instanceof ApiError && cause.status === 403 ? t('platform.permission') : t('computers.changeError'))
    } finally { setChanging(false) }
  }


  return (
    <div
      data-platform-computers={owned ? undefined : ''}
      data-my-computers={owned ? '' : undefined}
      data-platform-list-state={listState}
    >
      <Heading
        title={t(owned ? 'computers.myTitle' : 'platform.computers')}
        description={t(owned ? 'computers.myDescription' : 'computers.description')}
      />
      <section className="rounded-xl border bg-card p-4">
        <h3 className="text-sm font-semibold">{t('computers.register')}</h3>
        <p className="mt-1 text-xs leading-relaxed text-muted-foreground">{t(owned ? 'computers.myRegisterDescription' : 'computers.registerDescription')}</p>
        <form
          className={`${styles.formGrid} mt-4`}
          onSubmit={event => {
            event.preventDefault()
            void generate()
          }}
        >
          <Field>
            <Label htmlFor="platform-computer-name">{t('computers.name')}</Label>
            <Input
              id="platform-computer-name"
              autoComplete="off"
              value={computerName}
              required
              maxLength={128}
              placeholder={t('computers.namePlaceholder')}
              onChange={event => setComputerName(event.target.value)}
            />
          </Field>
          <Field>
            <Label htmlFor="platform-computer-project">{t('computers.project')}</Label>
            <Select id="platform-computer-project" value={projectId} onValueChange={nextValue => setProjectId(nextValue)}>
              <option value="">{t('computers.allProjects')}</option>
              {projects.map(project => <option value={project.project_id} key={project.project_id}>{project.name}</option>)}
            </Select>
          </Field>
          <Button type="submit" disabled={generating || !computerName.trim()}>
            {generating ? <LoaderCircle className={styles.spinner} /> : <Laptop />}
            {generating ? t('computers.generating') : t('computers.generate')}
          </Button>
        </form>
        {!projects.length && !loading ? <p className="mt-3 text-xs text-muted-foreground">{t(owned ? 'computers.memberProjectsEmpty' : 'computers.projectsEmpty')}</p> : null}
      </section>

      {error && (loading || computers.length) ? <p className="mt-4 text-sm text-destructive" role="alert">{error}</p> : null}
      {loading && computers.length ? <p className="mt-4 text-sm text-muted-foreground" role="status">{t('platform.loading')}</p> : null}
      {message ? <p className="mt-4 text-sm text-success" role="status">{message}</p> : null}

      <div className="mt-8 flex flex-wrap items-center justify-between gap-3">
        <h3 className="text-sm font-semibold">{t(owned ? 'computers.myList' : 'computers.list')}</h3>
        <div className="flex flex-wrap justify-end gap-1"><Button type="button" variant="ghost" size="sm" aria-pressed={includeRemoved} onClick={() => setIncludeRemoved(value => !value)}>{t(includeRemoved ? 'computers.hideRemoved' : 'computers.showRemoved')}</Button><Button type="button" variant="ghost" size="sm" onClick={() => void load()} disabled={loading}>
          <RefreshCw className={loading ? styles.spinner : ''} />{t('platform.refresh')}
        </Button></div>
      </div>

      {loading && !computers.length ? (
        <div className={`${styles.statePanel} mt-3`} role="status">
          <div><LoaderCircle className={styles.spinner} />{t('platform.loading')}</div>
        </div>
      ) : error && !computers.length ? (
        <div className={`${styles.statePanel} mt-3`} role="alert">
          <div><span>{error}</span><Button type="button" variant="outline" onClick={() => void load()}>{t('platform.retry')}</Button></div>
        </div>
      ) : computers.length ? (
        <div className={`${styles.list} mt-3`}>
          {computers.map(computer => {
            const project = projects.find(item => item.project_id === computer.project_id)
            const suspended = computer.management.suspended_at_ms != null
            const removed = computer.management.removed_at_ms != null
            return (
              <article className={`${styles.row} ${styles.computerRow}`} key={computer.executor_id} data-platform-computer={computer.executor_id}>
                <div className={styles.identity}>
                  <strong>{computer.management.name}</strong>
                  {computer.management.notes && <div className={`${styles.metadata} max-w-96 truncate`} title={computer.management.notes}>{computer.management.notes}</div>}
                  <div className={styles.metadata}>{project?.name ?? computer.project_id ?? t('computers.allProjects')}</div>
                  <div className={styles.metadata}>{t('computers.enrolled', { time: formatTime(computer.enrolled_at_ms) })}</div>
                </div>
                <div>
                  <div className={styles.stateLine}>
                    <span className={styles.status} data-online={String(computer.connected)}>
                      {t(suspended ? 'computers.suspended' : computer.connected ? 'computers.online' : 'computers.offline')}
                    </span>
                    <span className={styles.state}>{removed ? t('computers.removedState') : t(`computers.state.${computer.state}`)}</span>
                  </div>
                  <div className={styles.metadata}>
                    {computer.last_seen_at_ms
                      ? t('computers.lastSeen', { time: formatTime(computer.last_seen_at_ms) })
                      : t('computers.never')}
                  </div>
                </div>
                <div className={styles.actions}>
                  <Button type="button" variant="outline" size="sm" onClick={() => setDetailsTarget(computer.executor_id)}><Info />{t('computers.details')}</Button>
                  {computer.state === 'revoked' ? <Button type="button" variant="outline" size="sm" onClick={() => { setRecoveryTarget(computer); setRecoveryName(computer.management.name); setRecoveryError('') }}><RefreshCw />{t('computers.recover')}</Button> : <Button type="button" variant="outline" size="sm" onClick={() => setLifecycleTarget({ computer, action: suspended ? 'resume' : 'suspend' })}>{suspended ? <Play /> : <Pause />}{t(suspended ? 'computers.resume' : 'computers.suspend')}</Button>}
                  <Button
                    type="button"
                    variant="outline"
                    size="sm"
                    className="text-destructive hover:text-destructive"
                    disabled={computer.state === 'revoked'}
                    onClick={() => setRevokeTarget(computer)}
                  >
                    <ShieldX />{t('computers.revoke')}
                  </Button>
                  {!removed && <Button type="button" variant="outline" size="sm" className="text-destructive hover:text-destructive" onClick={() => setLifecycleTarget({ computer, action: 'remove' })}><Trash2 />{t('computers.remove')}</Button>}
                </div>
              </article>
            )
          })}
        </div>
      ) : <div className={`${styles.statePanel} mt-3`}>{t('computers.empty')}</div>}

      <Dialog open={Boolean(launch)} onOpenChange={next => { if (!next) closeLaunch() }}>
        <DialogContent className="max-w-2xl" data-node-launch-dialog="">
          <DialogHeader>
            <DialogTitle>{t('command.title')}</DialogTitle>
            <DialogDescription>{t(launch?.recovery ? 'command.recoveryDescription' : 'command.description')}</DialogDescription>
          </DialogHeader>
          <p className={styles.notice}>{t('command.warning')}</p>
          {launch ? (
            <pre className={styles.command} tabIndex={0} aria-label={t('command.label')} data-node-launch-command="">{launch.command}</pre>
          ) : null}
          {copyError ? <p className="text-sm text-destructive" role="alert">{copyError}</p> : null}
          <DialogFooter>
            <Button
              type="button"
              variant="outline"
              onClick={() => {
                if (!launch) return
                setCopyError('')
                void copyText(launch.command)
                  .then(() => setCopied(true))
                  .catch(() => setCopyError(t('command.copyFailed')))
              }}
            >
              {copied ? <Check /> : <Copy />}{copied ? t('command.copied') : t('command.copy')}
            </Button>
            <Button type="button" onClick={closeLaunch}>{t('command.done')}</Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      <ActionDialog
        open={Boolean(revokeTarget)}
        title={t('computers.revokeTitle')}
        description={t('computers.revokeDescription', { name: revokeTarget?.management.name ?? '' })}
        cancelLabel={commonT('cancel')}
        confirmLabel={t('computers.revoke')}
        busyLabel={t('computers.revoking')}
        busy={revoking}
        destructive
        onOpenChange={next => { if (!next) setRevokeTarget(null) }}
        onConfirm={() => void revoke()}
      />
      {detailsTarget && <ComputerDetailsDialog key={`${tenantId}:${scope}:${detailsTarget}`} tenantId={tenantId} executorId={detailsTarget} scope={scope} formatTime={formatTime} onClose={() => setDetailsTarget(null)} onSaved={load} />}
      <Dialog open={Boolean(recoveryTarget)} onOpenChange={open => { if (!open && !recovering) setRecoveryTarget(null) }}>
        <DialogContent className="max-w-lg" data-computer-recovery="">
          <DialogHeader><DialogTitle>{t('computers.recoverTitle')}</DialogTitle><DialogDescription>{t('computers.recoverDescription', { name: recoveryTarget?.management.name ?? '' })}</DialogDescription></DialogHeader>
          <Field><Label htmlFor="computer-recovery-name">{t('computers.name')}</Label><Input id="computer-recovery-name" required maxLength={128} value={recoveryName} disabled={recovering} onChange={event => setRecoveryName(event.target.value)} /><p className="text-xs text-muted-foreground">{t('computers.nameDescription')}</p></Field>
          {recoveryError && <p role="alert" className="text-sm text-destructive">{recoveryError}</p>}
          <DialogFooter><Button variant="outline" disabled={recovering} onClick={() => setRecoveryTarget(null)}>{commonT('cancel')}</Button><Button disabled={recovering || !recoveryName.trim()} onClick={() => void recover()}>{recovering ? <LoaderCircle className="animate-spin" /> : <RefreshCw />}{t('computers.recoverGenerate')}</Button></DialogFooter>
        </DialogContent>
      </Dialog>
      <ActionDialog
        open={Boolean(lifecycleTarget)}
        title={t(lifecycleTarget?.action === 'remove' ? 'computers.removeTitle' : lifecycleTarget?.action === 'resume' ? 'computers.resumeTitle' : 'computers.suspendTitle')}
        description={t(lifecycleTarget?.action === 'remove' ? 'computers.removeDescription' : lifecycleTarget?.action === 'resume' ? 'computers.resumeDescription' : 'computers.suspendDescription', { name: lifecycleTarget?.computer.management.name ?? '' })}
        cancelLabel={commonT('cancel')}
        confirmLabel={t(lifecycleTarget?.action === 'remove' ? 'computers.remove' : lifecycleTarget?.action === 'resume' ? 'computers.resume' : 'computers.suspend')}
        busyLabel={t('computers.changing')}
        busy={changing}
        destructive={lifecycleTarget?.action === 'remove'}
        onOpenChange={next => { if (!next) setLifecycleTarget(null) }}
        onConfirm={() => void changeLifecycle()}
      />
    </div>
  )
}
