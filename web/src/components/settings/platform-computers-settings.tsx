import { copyText } from '@/lib/clipboard'
import * as React from 'react'
import { Check, Copy, Laptop, LoaderCircle, RefreshCw, ShieldX } from 'lucide-react'
import { ApiError } from '@/api/client'
import { Button } from '@/components/ui/button'
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from '@/components/ui/dialog'
import { Field, Input, Label, Select } from '@/components/ui/field'
import { useLocale, useTranslate } from '@/i18n/provider'
import type { ManagedExecutionTarget, ProjectRecord } from '@/types'
import { ActionDialog, GroupHeader, SectionHeader } from './settings-ui'
import {
  createNodeLaunch,
  createOwnedNodeLaunch,
  listManagedComputers,
  listOwnedComputers,
  listPlatformProjects,
  revokeComputer,
  revokeOwnedComputer,
  type NodeLaunchCommand,
} from './platform-admin-api'
import styles from './platform-settings.module.css'

function errorCopy(
  cause: unknown,
  action: 'load' | 'enrollment' | 'revoke',
  t: ReturnType<typeof useTranslate<'settings'>>,
) {
  if (cause instanceof ApiError && cause.status === 403) return t('platform.permission')
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
  const t = useTranslate('settings')
  const commonT = useTranslate('common')
  const { locale } = useLocale()
  const [computers, setComputers] = React.useState<ManagedExecutionTarget[]>([])
  const [projects, setProjects] = React.useState<ProjectRecord[]>([])
  const [loading, setLoading] = React.useState(true)
  const [error, setError] = React.useState('')
  const [message, setMessage] = React.useState('')
  const [executorId, setExecutorId] = React.useState('')
  const [projectId, setProjectId] = React.useState('')
  const [generating, setGenerating] = React.useState(false)
  const [launch, setLaunch] = React.useState<NodeLaunchCommand | null>(null)
  const [copied, setCopied] = React.useState(false)
  const [copyError, setCopyError] = React.useState('')
  const [revokeTarget, setRevokeTarget] = React.useState<ManagedExecutionTarget | null>(null)
  const [revoking, setRevoking] = React.useState(false)
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
    locale === 'zh' ? 'zh-CN' : 'en',
    { dateStyle: 'medium', timeStyle: 'short' },
  ).format(new Date(value)), [locale])

  const load = React.useCallback(async () => {
    setLoading(true)
    setError('')
    try {
      const [nextComputers, nextProjects] = await Promise.all([
        owned ? listOwnedComputers(tenantId) : listManagedComputers(tenantId),
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
  }, [owned, t, tenantId])

  React.useEffect(() => { void load() }, [load])

  const generate = async () => {
    const normalized = executorId.trim()
    if (!normalized) return
    setGenerating(true)
    setError('')
    setMessage('')
    try {
      const next = await (owned ? createOwnedNodeLaunch : createNodeLaunch)(tenantId, {
        executorId: normalized,
        projectId: projectId || undefined,
      })
      setLaunch(next)
      setExecutorId('')
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
            <Label htmlFor="platform-computer-id">{t('computers.id')}</Label>
            <Input
              id="platform-computer-id"
              autoComplete="off"
              value={executorId}
              placeholder={t('computers.idPlaceholder')}
              onChange={event => setExecutorId(event.target.value)}
            />
          </Field>
          <Field>
            <Label htmlFor="platform-computer-project">{t('computers.project')}</Label>
            <Select id="platform-computer-project" value={projectId} onValueChange={nextValue => setProjectId(nextValue)}>
              <option value="">{t('computers.allProjects')}</option>
              {projects.map(project => <option value={project.project_id} key={project.project_id}>{project.name}</option>)}
            </Select>
          </Field>
          <Button type="submit" disabled={generating || !executorId.trim()}>
            {generating ? <LoaderCircle className={styles.spinner} /> : <Laptop />}
            {generating ? t('computers.generating') : t('computers.generate')}
          </Button>
        </form>
        {!projects.length && !loading ? <p className="mt-3 text-xs text-muted-foreground">{t(owned ? 'computers.memberProjectsEmpty' : 'computers.projectsEmpty')}</p> : null}
      </section>

      {error && (loading || computers.length) ? <p className="mt-4 text-sm text-destructive" role="alert">{error}</p> : null}
      {loading && computers.length ? <p className="mt-4 text-sm text-muted-foreground" role="status">{t('platform.loading')}</p> : null}
      {message ? <p className="mt-4 text-sm text-success" role="status">{message}</p> : null}

      <div className="mt-8 flex items-center justify-between gap-3">
        <h3 className="text-sm font-semibold">{t(owned ? 'computers.myList' : 'computers.list')}</h3>
        <Button type="button" variant="ghost" size="sm" onClick={() => void load()} disabled={loading}>
          <RefreshCw className={loading ? styles.spinner : ''} />{t('platform.refresh')}
        </Button>
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
            return (
              <article className={styles.row} key={computer.executor_id} data-platform-computer={computer.executor_id}>
                <div className={styles.identity}>
                  <strong>{computer.executor_id}</strong>
                  <div className={styles.metadata}>{project?.name ?? computer.project_id ?? t('computers.allProjects')}</div>
                  <div className={styles.metadata}>{t('computers.enrolled', { time: formatTime(computer.enrolled_at_ms) })}</div>
                </div>
                <div>
                  <div className={styles.stateLine}>
                    <span className={styles.status} data-online={String(computer.connected)}>
                      {t(computer.connected ? 'computers.online' : 'computers.offline')}
                    </span>
                    <span className={styles.state}>{t(`computers.state.${computer.state}`)}</span>
                  </div>
                  <div className={styles.metadata}>
                    {computer.last_seen_at_ms
                      ? t('computers.lastSeen', { time: formatTime(computer.last_seen_at_ms) })
                      : t('computers.never')}
                  </div>
                </div>
                <div className={styles.actions}>
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
            <DialogDescription>{t('command.description')}</DialogDescription>
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
        description={t('computers.revokeDescription', { name: revokeTarget?.executor_id ?? '' })}
        cancelLabel={commonT('cancel')}
        confirmLabel={t('computers.revoke')}
        busyLabel={t('computers.revoking')}
        busy={revoking}
        destructive
        onOpenChange={next => { if (!next) setRevokeTarget(null) }}
        onConfirm={() => void revoke()}
      />
    </div>
  )
}
