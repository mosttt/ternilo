import { ModelConnectionsSettings } from './model-connections-settings'
import { isConnectionProvider } from '@/components/models/model-device-types'
import * as React from 'react'
import { Plus, RefreshCw, Trash2 } from 'lucide-react'
import { api } from '@/api/client'
import { useWorkbench } from '@/state/workbench'
import type { CredentialInventory, ProviderModel, ProviderModelDiscoveryRequest, ProviderProfile } from '@/types'
import { Button } from '@/components/ui/button'
import { cn } from '@/lib/utils'
import { providerIsUsable } from '@/domain/provider-readiness'
import { invalidateProviderInventory, loadProviderInventory, peekProviderInventory } from '@/domain/provider-inventory'
import {
  executionTargetKey,
  executionTargetHeaders,
  executionTargetPath,
  type ExecutionTarget,
} from '@/domain/execution-target'
import { persistModelSelection } from '@/components/workbench/model-picker'
import { providerModel, providerModelDefaults } from './provider-model-editor'
import { ProviderEditorCard, type ProviderDraft } from './provider-editor-card'
import { useTranslate } from '@/i18n/provider'
import { ActionDialog, SectionHeader } from './settings-ui'
import type { ProviderModelValidationCopy } from './provider-model-editor'
import styles from './settings-layout.module.css'

function providerCredentialReference(id: string) {
  const normalized = [...id].map(character => character === '-' ? '_H_' : character === '_' ? '_U_' : character.toUpperCase()).join('')
  return `TERNILO_PROVIDER_${normalized}_API_KEY`
}

interface ProviderValidationCopy {
  model: ProviderModelValidationCopy
  invalidId: string
  missingName: string
  invalidUrl: string
  missingModels: string
  duplicateModels: string
  invalidCloudOutput: string
  invalidRetry: string
}

export function validateProviderDraft(draft: ProviderDraft, copy: ProviderValidationCopy) {
  const id = draft.id.trim()
  if (!/^[a-z][a-z0-9_-]{0,63}$/.test(id)) throw new Error(copy.invalidId)
  const displayName = draft.displayName.trim()
  if (!displayName) throw new Error(copy.missingName)
  if (!/^https?:\/\//.test(draft.baseUrl.trim())) throw new Error(copy.invalidUrl)
  if (!draft.models.length) throw new Error(copy.missingModels)
  const defaults = providerModelDefaults(draft.defaults, 'Provider', copy.model)
  const models = draft.models.map((model, index) => providerModel(model, index, copy.model))
  if (new Set(models.map(model => model.id)).size !== models.length) throw new Error(copy.duplicateModels)
  if (defaults.max_output_tokens > 4_294_967_295 || models.some(model => (
    model.settings.mode === 'automatic' && [model.settings.upstream.max_output_tokens, model.settings.overrides.max_output_tokens].some(value => value != null && value > 4_294_967_295)
  ))) throw new Error(copy.invalidCloudOutput)
  if ((!Number.isSafeInteger(draft.timeoutMs) || draft.timeoutMs < 0) || draft.retryBaseDelayMs <= 0 || draft.maxAttempts < 1 || draft.maxAttempts > 8) throw new Error(copy.invalidRetry)
  return { id, displayName, defaults, models }
}

export function ProviderLibrary({ target, scope, title, description, authorable = true, refreshRevision = 0 }: { target: ExecutionTarget; scope: 'local' | 'account' | 'node'; title: string; description: string; authorable?: boolean; refreshRevision?: number }) {
  const { currentSession, updateSession, notify, refresh } = useWorkbench()
  const cloud = scope === 'account'
  const providerAuthoring = authorable && window.__TERNILO_BOOT__?.providerAuthoring !== false
  const t = useTranslate('settings')
  const common = useTranslate('common')
  const modelT = useTranslate('model')
  const [providers, setProviders] = React.useState<ProviderProfile[]>([])
  const [credentials, setCredentials] = React.useState<CredentialInventory | null>(null)
  const [editing, setEditing] = React.useState<string | 'new' | null>(null)
  const [status, setStatus] = React.useState<{ kind: 'success' | 'error'; text: string } | null>(null)
  const [loading, setLoading] = React.useState(true)
  const loadGeneration = React.useRef(0)
  const observedRefresh = React.useRef(refreshRevision)
  const [loadError, setLoadError] = React.useState('')
  const [removeTarget, setRemoveTarget] = React.useState<ProviderProfile | null>(null)
  const [removing, setRemoving] = React.useState(false)
  const [removeError, setRemoveError] = React.useState('')
  const targetKey = executionTargetKey(target)

  const validationCopy = React.useMemo<ProviderValidationCopy>(() => ({
    invalidId: t('provider.errorId'),
    missingName: t('provider.errorName'),
    invalidUrl: t('provider.errorUrl'),
    missingModels: t('provider.errorModels'),
    duplicateModels: t('provider.errorDuplicateModels'),
    invalidCloudOutput: t('provider.errorCloudOutput'),
    invalidRetry: t('provider.errorRetry'),
    model: {
      invalidCapacity: (raw) => t('provider.errorInvalidCapacity', { value: raw }),
      positiveCapacity: (raw) => t('provider.errorPositiveCapacity', { value: raw }),
      missingId: (index) => t('provider.errorMissingModelId', { index }),
      missingEffort: (id) => t('provider.errorMissingEffort', { id }),
      defaultNotEnabled: (id) => t('provider.errorDefaultEffort', { id }),
    },
  }), [t])

  const load = React.useCallback(async (force = false) => {
    const generation = ++loadGeneration.current
    setLoading(true)
    setLoadError('')
    try {
      const inventory = await loadProviderInventory(force, target)
      if (generation !== loadGeneration.current) return
      setProviders([...inventory.providers].sort((a, b) => a.display_name.localeCompare(b.display_name)))
      setCredentials(inventory.credentials)
    } catch (cause) {
      if (generation === loadGeneration.current) setLoadError(cause instanceof Error ? cause.message : String(cause))
    } finally {
      if (generation === loadGeneration.current) setLoading(false)
    }
  }, [targetKey])

  React.useEffect(() => {
    const cached = peekProviderInventory(target)
    setEditing(null)
    setProviders(cached ? [...cached.providers].sort((a, b) => a.display_name.localeCompare(b.display_name)) : [])
    setCredentials(cached?.credentials ?? null)
    void load()
    return () => { loadGeneration.current += 1 }
  }, [load, targetKey])

  const connectionsChanged = React.useCallback(() => load(true), [load])
  const reloadInventory = () => { invalidateProviderInventory(target); void load(true) }
  React.useEffect(() => {
    if (observedRefresh.current === refreshRevision) return
    observedRefresh.current = refreshRevision
    void load(true)
  }, [refreshRevision, load])

  const credentialConfigured = (provider: ProviderProfile) => Boolean(
    provider.source === 'operator'
    || provider.api_key_ref && credentials?.references.some(reference => (
      reference.reference === provider.api_key_ref && reference.configured
    )),
  )

  const keyState = (provider: ProviderProfile) => {
    if (provider.source === 'operator') return { usable: true, label: t('models.operatorManaged') }
    if (!provider.api_key_ref) return { usable: true, label: t('models.keyNotRequired') }
    const present = providerIsUsable(provider, credentials)
    return { usable: present, label: t(present ? 'models.keyConfigured' : 'models.keyMissing') }
  }
  const hasLoaded = credentials !== null
  const displayedProviders = providers.filter(provider => !isConnectionProvider(provider.id))
  const dataState = loading && !hasLoaded
    ? 'loading'
    : loadError && !hasLoaded
      ? 'error'
      : displayedProviders.length
        ? 'ready'
        : 'empty'

  const save = async (draft: ProviderDraft, apiKey: string, original?: ProviderProfile) => {
    const { id, displayName, defaults, models } = validateProviderDraft(draft, validationCopy)
    const current = providers.find(provider => provider.id === id)
    if (!original && current) throw new Error(t('provider.errorDuplicateProvider', { id }))
    const key = apiKey.trim()
    const credentialReference = key ? providerCredentialReference(id) : original?.api_key_ref ?? null
    if (key) {
      await api.request(executionTargetPath('/credentials', target), { headers: executionTargetHeaders(target), method: 'POST', body: { name: credentialReference, value: key } })
      invalidateProviderInventory(target)
    }
    await api.request(executionTargetPath('/providers', target), {
      headers: executionTargetHeaders(target),
      method: 'POST',
      body: {
        id,
        display_name: displayName,
        base_url: draft.baseUrl.trim().replace(/\/$/, ''),
        protocol: draft.protocol,
        api_key_ref: credentialReference,
        defaults,
        models,
        timeout_ms: draft.timeoutMs,
        max_attempts: draft.maxAttempts,
        retry_base_delay_ms: draft.retryBaseDelayMs,
      },
    })
    invalidateProviderInventory(target)
    if (currentSession?.model.provider === 'profile_default' && scope === 'local') {
      await persistModelSelection(
        currentSession.identity.session_id,
        { provider: 'named_provider', provider_id: id, model: models[0].id },
        updateSession,
        (path, options) => api.request(path, options),
      )
    }
    await Promise.all([load(true), refresh()])
    setEditing(null)
    setStatus({ kind: 'success', text: t('models.saved', { name: displayName, keyStatus: key ? t('models.keySaved') : '' }) })
    notify(t('models.savedToast'))
  }

  const discover = (request: ProviderModelDiscoveryRequest) => api.request<ProviderModel[]>(executionTargetPath('/providers/discover', target), { headers: executionTargetHeaders(target), method: 'POST', body: request })

  const remove = async () => {
    const provider = removeTarget
    if (!provider) return
    setRemoving(true)
    setRemoveError('')
    try {
      await api.request(executionTargetPath(`/providers/${encodeURIComponent(provider.id)}`, target), { headers: executionTargetHeaders(target), method: 'DELETE' })
      invalidateProviderInventory(target)
      const managedReference = credentials?.references.find((reference) => (
        reference.reference === provider.api_key_ref
        && reference.reference === providerCredentialReference(provider.id)
        && reference.writable
        && reference.source !== 'environment'
      ))
      if (managedReference) {
        try {
          await api.request(executionTargetPath(`/credentials/${encodeURIComponent(managedReference.reference)}`, target), { headers: executionTargetHeaders(target), method: 'DELETE' })
          invalidateProviderInventory(target)
        } catch (cause) {
          await load(true)
          setRemoveTarget(null)
          setStatus({ kind: 'error', text: t('models.deletedKeyCleanupFailed', {
            name: provider.display_name,
            error: cause instanceof Error ? cause.message : String(cause),
          }) })
          return
        }
      }
      if (editing === provider.id) setEditing(null)
      await load(true)
      setRemoveTarget(null)
      setStatus({ kind: 'success', text: t('models.deleted', { name: provider.display_name }) })
    } catch (cause) {
      setRemoveError(cause instanceof Error ? cause.message : String(cause))
    } finally {
      setRemoving(false)
    }
  }

  return <div className={styles.section} data-models-state={dataState}>
    <SectionHeader
      title={title}
      description={description}
      action={<div className="flex flex-wrap gap-2"><Button variant="outline" disabled={loading} onClick={reloadInventory} aria-label={t('models.refreshProviders')}><RefreshCw className={loading ? 'animate-spin' : undefined} />{t('models.refreshProviders')}</Button>{providerAuthoring && <Button variant="outline" onClick={() => { setStatus(null); setEditing('new') }}><Plus />{t('models.addProvider')}</Button>}</div>}
    />

    {scope === 'local' && providerAuthoring && <ModelConnectionsSettings onChange={connectionsChanged} />}

    <div className={cn('space-y-2', scope === 'local' && providerAuthoring && 'mt-6')} data-provider-inventory="">
      {loading && !hasLoaded ? <div className="rounded-xl border border-dashed p-8 text-center text-sm text-muted-foreground">{common('loading')}</div> : null}
      {!loading && loadError && !hasLoaded ? <div className="rounded-xl border border-destructive/40 p-6 text-center"><p className="text-sm text-destructive" role="alert">{loadError}</p><Button className="mt-4" size="sm" variant="outline" onClick={() => void load()}>{common('retry')}</Button></div> : null}
      {loading && hasLoaded ? <p className="rounded-lg border px-3 py-2 text-xs text-muted-foreground" role="status">{common('loading')}</p> : null}
      {loadError && hasLoaded ? <div className="rounded-lg border border-destructive/40 px-3 py-2 text-xs text-destructive" role="alert"><span>{loadError}</span><Button className="ml-3" size="xs" variant="outline" onClick={() => void load()}>{common('retry')}</Button></div> : null}
      {hasLoaded ? <>
      {displayedProviders.map(provider => <React.Fragment key={provider.id}>
        {(() => {
          const writable = providerAuthoring && provider.source !== 'operator'
          const credential = keyState(provider)
          return <>
        <div className={cn('rounded-xl border bg-card', editing === provider.id && 'border-primary/40')}>
          <div className="flex min-w-0 items-center gap-3 p-4">
            <span
              className={cn('size-2.5 shrink-0 rounded-full border', credential.usable ? 'border-success bg-success' : 'border-muted-foreground/40')}
              role="img"
              aria-label={credential.label}
              title={credential.label}
            />
            <div className="min-w-0 flex-1"><div className="flex flex-wrap items-center gap-2"><span className="truncate text-sm font-medium">{provider.display_name}</span><span className="rounded border px-1.5 py-0.5 text-[10px] text-muted-foreground">{t(provider.source === 'operator' ? 'models.operator' : 'models.custom')}</span></div><div className="mt-1 truncate font-mono text-[10px] text-muted-foreground">{provider.id} · {provider.protocol}</div></div>
            {writable && <Button size="sm" variant="outline" onClick={() => { setStatus(null); setEditing(editing === provider.id ? null : provider.id) }}>{editing === provider.id ? common('collapse') : common('edit')}</Button>}
            {writable && <Button size="sm" variant="ghost" className="text-destructive hover:text-destructive" aria-label={t('models.deleteAria', { name: provider.display_name })} onClick={() => { setRemoveError(''); setRemoveTarget(provider) }}><Trash2 /><span className="max-sm:hidden">{common('delete')}</span></Button>}
          </div>
          <div className="flex flex-wrap gap-2 border-t px-4 py-3" data-provider-models="">{provider.models.map(model => <span key={model.id} className="max-w-full break-all rounded-md bg-muted px-2 py-1 text-xs" title={model.id}>{model.display_name || model.id}</span>)}</div>
        </div>
        {editing === provider.id && <ProviderEditorCard key={provider.id} provider={provider} credentialConfigured={credentialConfigured(provider)} onCancel={() => setEditing(null)} onSave={(draft, key) => save(draft, key, provider)} onDiscover={discover} />}
          </>
        })()}
      </React.Fragment>)}
      {!displayedProviders.length && editing !== 'new' && <div className="rounded-xl border border-dashed p-8 text-center"><p className="text-sm font-medium">{cloud ? modelT('cloud.noByok') : t('models.empty')}</p><p className="mt-2 text-xs text-muted-foreground">{cloud ? modelT('cloud.settingsDescription') : t('models.emptyDescription')}</p>{providerAuthoring && <Button className="mt-4" onClick={() => setEditing('new')}><Plus />{t('models.addProvider')}</Button>}</div>}
      {editing === 'new' && <ProviderEditorCard key="new" credentialConfigured={false} onCancel={() => setEditing(null)} onSave={(draft, key) => save(draft, key)} onDiscover={discover} />}
      </> : null}
    </div>

    {status && <p className={cn('mt-4 text-xs', status.kind === 'error' ? 'text-destructive' : 'text-muted-foreground')} role="status">{status.text}</p>}

    <ActionDialog
      open={removeTarget !== null}
      title={t('models.deleteTitle')}
      description={removeTarget ? t('models.deleteDescription', { name: removeTarget.display_name }) : undefined}
      cancelLabel={common('cancel')}
      confirmLabel={common('delete')}
      busyLabel={t('models.deleting')}
      busy={removing}
      destructive
      error={removeError}
      onOpenChange={(open) => { if (!open) setRemoveTarget(null) }}
      onConfirm={() => void remove()}
    />
  </div>
}
