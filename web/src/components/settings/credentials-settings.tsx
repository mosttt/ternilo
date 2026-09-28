import * as React from 'react'
import { ExternalLink, FileKey2, KeyRound, LogOut, ShieldCheck, Trash2 } from 'lucide-react'
import { api } from '@/api/client'
import { Button } from '@/components/ui/button'
import { Field, Input, Label, Select } from '@/components/ui/field'
import { useTranslate } from '@/i18n/provider'
import { useWorkbench } from '@/state/workbench'
import { invalidateProviderInventory, loadProviderInventory } from '@/domain/provider-inventory'
import {
  executionTargetKey,
  executionTargetPath,
  type ExecutionTarget,
} from '@/domain/execution-target'
import type { CredentialInventory } from '@/types'
import { randomUuid } from '@/lib/random-id'
import { ActionDialog, GroupHeader, SectionHeader } from './settings-ui'
import styles from './settings-layout.module.css'

const authorizationSurfaceId = `web-${randomUuid()}`

interface AuthorizationKey {
  space: string
  key: string
}

interface AuthorizationSnapshot {
  entries: Array<{
    key: AuthorizationKey
    label: string
    methods: Array<{ id: string; label: string }>
    in_flight: boolean
    configured: boolean
    writable: boolean
  }>
  attempts: Array<{
    attempt_id: string
    key: AuthorizationKey
    method: string
    status: string
    error?: string | null
  }>
  notices: Array<{
    id: string
    key: AuthorizationKey
    notice: { message: string; url?: string | null; code?: string | null }
  }>
  prompts: Array<{
    id: string
    key: AuthorizationKey
    prompt: {
      kind: string
      message: string
      placeholder?: string
      options?: Array<{ id: string; label: string; description?: string }>
    }
  }>
}

function sameKey(left: AuthorizationKey, right: AuthorizationKey) {
  return left.space === right.space && left.key === right.key
}

function flowKey(key: AuthorizationKey) {
  return `${key.space}:${key.key}`
}

export function CredentialsSettings() {
  const { currentSession, currentWorkspace, remote, platform, logout, notify } = useWorkbench()
  const t = useTranslate('settings')
  const common = useTranslate('common')
  const [inventory, setInventory] = React.useState<CredentialInventory | null>(null)
  const [authorizations, setAuthorizations] = React.useState<AuthorizationSnapshot | null>(null)
  const [name, setName] = React.useState('')
  const [value, setValue] = React.useState('')
  const [loading, setLoading] = React.useState(true)
  const [loadError, setLoadError] = React.useState('')
  const [busy, setBusy] = React.useState(false)
  const [authorizationBusy, setAuthorizationBusy] = React.useState<string | null>(null)
  const [authorizationErrors, setAuthorizationErrors] = React.useState<Record<string, string>>({})
  const [removeTarget, setRemoveTarget] = React.useState<string | null>(null)
  const [removeError, setRemoveError] = React.useState('')
  const promptRefs = React.useRef(new Map<string, HTMLInputElement>())
  const promptChoices = React.useRef(new Map<string, string>())
  const loadedRef = React.useRef(false)
  const target = React.useMemo<ExecutionTarget>(() => ({
    sessionId: currentSession?.identity.session_id,
    workspaceId: currentWorkspace?.workspace_id,
  }), [currentSession?.identity.session_id, currentWorkspace?.workspace_id])
  const targetKey = executionTargetKey(target)

  const load = React.useCallback(async (): Promise<AuthorizationSnapshot | null> => {
    if (!loadedRef.current) setLoading(true)
    try {
      const [credentials, authorization] = await Promise.all([
        api.request<CredentialInventory>(executionTargetPath('/credentials', target)),
        api.request<AuthorizationSnapshot>(executionTargetPath('/authorizations', target, { surface_id: authorizationSurfaceId })),
      ])
      setInventory(credentials)
      setAuthorizations(authorization)
      setLoadError('')
      loadedRef.current = true
      return authorization
    } catch (cause) {
      setLoadError(cause instanceof Error ? cause.message : String(cause))
      return null
    } finally {
      setLoading(false)
    }
  }, [targetKey])

  React.useEffect(() => {
    loadedRef.current = false
    setInventory(null)
    setAuthorizations(null)
    void load()
  }, [load, targetKey])
  const authorizationInFlight = authorizations?.entries.some((entry) => entry.in_flight) ?? false
  React.useEffect(() => {
    if (!authorizationInFlight) return
    const timer = window.setInterval(() => void load().then(snapshot => {
      if (!snapshot || snapshot.entries.some(entry => entry.in_flight)) return
      invalidateProviderInventory(target)
      void loadProviderInventory(true, target).catch(cause => {
        setLoadError(cause instanceof Error ? cause.message : String(cause))
      })
    }), 1_500)
    return () => window.clearInterval(timer)
  }, [authorizationInFlight, load, targetKey])

  const refreshAfterCredentialChange = async () => {
    invalidateProviderInventory(target)
    await Promise.all([load(), loadProviderInventory(true, target)])
  }

  const save = async (event: React.FormEvent) => {
    event.preventDefault()
    if (!name.trim() || !value) return
    setBusy(true)
    try {
      await api.request(executionTargetPath('/credentials', target), { method: 'POST', body: { name: name.trim(), value } })
      setValue('')
      setName('')
      await refreshAfterCredentialChange()
      notify(t('credentials.saved'))
    } catch (cause) {
      notify(cause instanceof Error ? cause.message : String(cause), 'error')
    } finally {
      setBusy(false)
    }
  }

  const remove = async () => {
    if (!removeTarget) return
    setBusy(true)
    setRemoveError('')
    try {
      await api.request(executionTargetPath(`/credentials/${encodeURIComponent(removeTarget)}`, target), { method: 'DELETE' })
      setRemoveTarget(null)
      await refreshAfterCredentialChange()
    } catch (cause) {
      setRemoveError(cause instanceof Error ? cause.message : String(cause))
    } finally {
      setBusy(false)
    }
  }

  const begin = async (entry: AuthorizationSnapshot['entries'][number], method: string) => {
    const id = flowKey(entry.key)
    setAuthorizationBusy(`${id}:begin`)
    setAuthorizationErrors(current => ({ ...current, [id]: '' }))
    try {
      await api.request(executionTargetPath('/authorizations/begin', target), {
        method: 'POST',
        body: { key: entry.key, method, surface_id: authorizationSurfaceId },
      })
      await refreshAfterCredentialChange()
    } catch (cause) {
      const message = cause instanceof Error ? cause.message : String(cause)
      setAuthorizationErrors(current => ({ ...current, [id]: message }))
      notify(message, 'error')
    } finally {
      setAuthorizationBusy(null)
    }
  }

  const cancel = async (key: AuthorizationKey) => {
    const id = flowKey(key)
    setAuthorizationBusy(`${id}:cancel`)
    setAuthorizationErrors(current => ({ ...current, [id]: '' }))
    try {
      await api.request(executionTargetPath('/authorizations/cancel', target), { method: 'POST', body: key })
      await refreshAfterCredentialChange()
    } catch (cause) {
      const message = cause instanceof Error ? cause.message : String(cause)
      setAuthorizationErrors(current => ({ ...current, [id]: message }))
      notify(message, 'error')
    } finally {
      setAuthorizationBusy(null)
    }
  }

  const answer = async (promptId: string, key: AuthorizationKey) => {
    const id = flowKey(key)
    const value = promptChoices.current.get(promptId) ?? promptRefs.current.get(promptId)?.value ?? authorizations?.prompts.find(item => item.id === promptId)?.prompt.options?.[0]?.id ?? ''
    setAuthorizationBusy(`${id}:answer`)
    setAuthorizationErrors(current => ({ ...current, [id]: '' }))
    try {
      await api.request(executionTargetPath('/authorization-prompts/answer', target), {
        method: 'POST',
        body: { prompt_id: promptId, surface_id: authorizationSurfaceId, value },
      })
      promptRefs.current.delete(promptId)
      promptChoices.current.delete(promptId)
      await refreshAfterCredentialChange()
    } catch (cause) {
      const message = cause instanceof Error ? cause.message : String(cause)
      setAuthorizationErrors(current => ({ ...current, [id]: message }))
      notify(message, 'error')
    } finally {
      setAuthorizationBusy(null)
    }
  }

  return (
    <div className={styles.section}>
      <SectionHeader title={t('nav.credentials')} description={t('credentials.description')} />
      {loadError ? (
        <div className="mb-5 rounded-xl border border-destructive/40 p-4">
          <p className="text-sm text-destructive" role="alert">{loadError}</p>
          <Button type="button" className="mt-3" size="sm" variant="outline" disabled={loading} onClick={() => void load()}>
            {loading ? common('loading') : common('retry')}
          </Button>
        </div>
      ) : null}
      {remote && !platform ? <section>
        <GroupHeader title={t('credentials.remoteAccess')} description={t('credentials.remoteAccessDescription')} />
        <div className="flex flex-col items-start gap-3 rounded-xl border bg-card p-4 sm:flex-row sm:items-center sm:justify-between">
          <p className="text-sm leading-relaxed text-muted-foreground">{t('credentials.signOutDescription')}</p>
          <Button type="button" className="shrink-0" variant="outline" onClick={logout} data-settings-sign-out="">
            <LogOut />{t('credentials.signOut')}
          </Button>
        </div>
      </section> : null}
      <section>
        <GroupHeader title={t('credentials.pluginLogin')} description={t('credentials.pluginLoginDescription')} />
        <div className="space-y-2">
          {loading && !authorizations ? (
            <div className="rounded-xl border border-dashed p-6 text-center text-sm text-muted-foreground">{common('loading')}</div>
          ) : authorizations?.entries.length ? authorizations.entries.map((entry) => {
            const id = flowKey(entry.key)
            const notices = authorizations.notices.filter((item) => sameKey(item.key, entry.key))
            const prompts = authorizations.prompts.filter((item) => sameKey(item.key, entry.key))
            const latestAttempt = authorizations.attempts.filter((item) => sameKey(item.key, entry.key)).at(-1)
            const starting = authorizationBusy === `${id}:begin`
            const state = starting
              ? 'starting'
              : latestAttempt?.status ?? (entry.configured ? 'authorized' : 'ready')
            const stateLabel = {
              ready: t('credentials.state.ready'),
              starting: t('credentials.state.starting'),
              running: t('credentials.state.running'),
              authorized: t('credentials.state.authorized'),
              cancelled: t('credentials.state.cancelled'),
              failed: t('credentials.state.failed'),
            }[state]
            const failure = authorizationErrors[id] || (latestAttempt?.status === 'failed' ? latestAttempt.error : '')
            const actionBusy = authorizationBusy?.startsWith(`${id}:`) ?? false
            return (
              <div className="rounded-xl border bg-card p-4" key={id} data-authorization-flow={id} data-authorization-state={state}>
                <div className="flex flex-wrap items-center gap-3">
                  <ShieldCheck className="size-4 text-muted-foreground" />
                  <div className="min-w-0 flex-1">
                    <div className="text-sm font-medium">{entry.label}</div>
                    <div className="text-xs text-muted-foreground" role="status" aria-live="polite">
                      {stateLabel}
                    </div>
                  </div>
                  {entry.in_flight ? (
                    <Button size="sm" variant="outline" disabled={actionBusy} onClick={() => void cancel(entry.key)}>{common('cancel')}</Button>
                  ) : entry.methods.map((method) => (
                    <Button
                      size="sm"
                      variant="outline"
                      disabled={!entry.writable || actionBusy}
                      key={method.id}
                      onClick={() => void begin(entry, method.id)}
                    >
                      {entry.configured ? t('credentials.loginAgain') : latestAttempt?.status === 'failed' ? t('credentials.retry') : method.label}
                    </Button>
                  ))}
                </div>
                {failure ? <p className="mt-3 text-xs text-destructive" role="alert">{failure}</p> : null}
                {notices.map((notice) => (
                  <div className="mt-3 rounded-lg bg-muted p-3 text-xs" key={notice.id}>
                    <p>{notice.notice.message}</p>
                    {notice.notice.code ? <code className="mt-2 block text-sm">{notice.notice.code}</code> : null}
                    {notice.notice.url ? (
                      <a className="mt-2 inline-flex items-center gap-1 text-primary underline" target="_blank" rel="noreferrer" href={notice.notice.url}>
                        {t('credentials.openAuthorization')}<ExternalLink className="size-3" />
                      </a>
                    ) : null}
                  </div>
                ))}
                {prompts.map((item) => (
                  <div className="mt-3 flex flex-col items-stretch gap-2 rounded-lg bg-muted p-3 sm:flex-row sm:items-end" key={item.id}>
                    <Field className="flex-1">
                      <Label htmlFor={`authorization-prompt-${item.id}`}>{item.prompt.message}</Label>
                      {item.prompt.kind === 'select' ? (
                        <Select id={`authorization-prompt-${item.id}`} defaultValue={item.prompt.options?.[0]?.id} onValueChange={value => promptChoices.current.set(item.id, value)}>
                          {item.prompt.options?.map((option) => <option value={option.id} key={option.id}>{option.label}</option>)}
                        </Select>
                      ) : (
                        <Input
                          id={`authorization-prompt-${item.id}`}
                          ref={(element) => { if (element) promptRefs.current.set(item.id, element) }}
                          type={item.prompt.kind === 'secret' ? 'password' : 'text'}
                          placeholder={item.prompt.placeholder}
                        />
                      )}
                    </Field>
                    <Button disabled={actionBusy} onClick={() => void answer(item.id, entry.key)}>{common('submit')}</Button>
                  </div>
                ))}
              </div>
            )
          }) : (
            <div className="rounded-xl border border-dashed p-6 text-center text-sm text-muted-foreground">{t('credentials.emptyLogin')}</div>
          )}
        </div>
      </section>

      <section className={styles.group}>
        <GroupHeader title={t('credentials.local')} description={t('credentials.localDescription')} />
        <form className="grid gap-3 rounded-xl border bg-card p-4 sm:grid-cols-[1fr_1.4fr_auto]" onSubmit={(event) => void save(event)}>
          <Field>
            <Label htmlFor="credential-name">{t('credentials.name')}</Label>
            <Input id="credential-name" value={name} onChange={(event) => setName(event.target.value)} placeholder="MY_SERVICE_TOKEN" />
          </Field>
          <Field>
            <Label htmlFor="credential-value">{t('credentials.value')}</Label>
            <Input id="credential-value" value={value} onChange={(event) => setValue(event.target.value)} type="password" autoComplete="new-password" />
          </Field>
          <div className="flex items-end"><Button type="submit" disabled={busy || !name.trim() || !value}><KeyRound />{common('save')}</Button></div>
        </form>
        <div className="mt-3 overflow-hidden rounded-xl border bg-card">
          {loading && !inventory ? <div className="p-6 text-center text-sm text-muted-foreground">{common('loading')}</div> : inventory?.references.length ? inventory.references.map((reference) => (
            <div className="flex items-center gap-3 border-b p-3 last:border-b-0" key={reference.reference}>
              <FileKey2 className="size-4 text-muted-foreground" />
              <code className="min-w-0 flex-1 truncate text-xs">{reference.reference}</code>
              <span className="text-xs text-muted-foreground">
                {reference.reference.startsWith('TERNILO_PROVIDER_')
                  ? t('credentials.providerManaged')
                  : reference.source === 'environment' ? t('credentials.environment') : t('credentials.localManaged')}
              </span>
              {reference.writable && !reference.reference.startsWith('TERNILO_PROVIDER_') ? (
                <Button
                  size="icon-xs"
                  variant="ghost"
                  aria-label={`${t('credentials.delete')}: ${reference.reference}`}
                  onClick={() => { setRemoveError(''); setRemoveTarget(reference.reference) }}
                >
                  <Trash2 />
                </Button>
              ) : null}
            </div>
          )) : <div className="p-6 text-center text-sm text-muted-foreground">{t('credentials.empty')}</div>}
        </div>
      </section>

      <ActionDialog
        open={removeTarget !== null}
        title={t('credentials.deleteTitle')}
        description={removeTarget ? t('credentials.deleteDescription', { name: removeTarget }) : undefined}
        cancelLabel={common('cancel')}
        confirmLabel={common('delete')}
        busyLabel={t('credentials.deleting')}
        busy={busy}
        destructive
        error={removeError}
        onOpenChange={(open) => { if (!open) setRemoveTarget(null) }}
        onConfirm={() => void remove()}
      />
    </div>
  )
}
