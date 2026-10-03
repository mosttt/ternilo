import * as React from 'react'
import { ArrowLeft, Check, ChevronRight, ChevronsUpDown, CircleGauge, Sparkles } from 'lucide-react'
import { api } from '@/api/client'
import { usePathname } from '@/app/navigation'
import { supportedReasoningEfforts } from '@/domain/model-reasoning'
import { currentCloudModel, invalidateCloudModelInventory, loadCloudModelInventory, modelSelectionKey, peekCloudModelInventory, subscribeCloudModelInventory, type CloudModelCurrent } from '@/domain/cloud-model-inventory'
import { PlatformModelItems } from './platform-model-items'
import { ProviderModelItems } from './provider-model-items'
import { ComputerModelItems } from './computer-model-items'
import { isAccountConnectionProvider, isConnectionProvider } from '@/components/models/model-device-types'
import { useAccountProviders } from './use-account-providers'
import { executionTargetHeaders, executionTargetKey, executionTargetPath, type ExecutionTarget } from '@/domain/execution-target'
import { resolvedProviderModel, type ResolvedProviderModel } from '@/domain/provider-model'
import { invalidateProviderInventory, loadProviderInventory, peekProviderInventory, subscribeProviderInventory } from '@/domain/provider-inventory'
import {
  modelSelectionIsUsable,
  profileModelIsUsable,
  usableProviderModels,
} from '@/domain/provider-readiness'
import { useWorkbench } from '@/state/workbench'
import type { CredentialInventory, ModelSelection, Profile, ProviderProfile } from '@/types'
import { Button } from '@/components/ui/button'
import {
  DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuLabel,
  DropdownMenuPortal, DropdownMenuSeparator, DropdownMenuSub, DropdownMenuSubContent,
  DropdownMenuSubTrigger, DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import { useTranslate } from '@/i18n/provider'
import type { ReasoningEffort } from '@/types'
import css from './model-picker.module.css'

function subscribeMenuViewport(listener: () => void) {
  window.addEventListener('resize', listener)
  return () => window.removeEventListener('resize', listener)
}

export function modelLabel(selection: ModelSelection, providers: ProviderProfile[], defaultLabel: string, current?: CloudModelCurrent | null) {
  if (selection.provider === 'profile_default') return defaultLabel
  if (selection.provider === 'platform_model') return `${current?.model?.display_name || selection.model_id}${current?.source_name ? ` · ${current.source_name}` : ''}`
  if (selection.provider === 'open_ai_compatible') return selection.model || 'OpenAI-compatible'
  const provider = providers.find(item => item.id === selection.provider_id)
  const model = provider?.models.find(item => item.id === selection.model)
  return current?.model?.display_name || model?.display_name || selection.model || provider?.display_name || selection.provider_id
}

function selectedProviderModel(selection: ModelSelection, providers: ProviderProfile[]) {
  if (selection.provider !== 'named_provider') return null
  const provider = providers.find(item => item.id === selection.provider_id)
  const model = provider?.models.find(item => item.id === selection.model)
  const resolved = resolvedProviderModel(provider, selection.model)
  return provider && model && resolved ? { provider, model, resolved } : null
}

function effectiveEffort(selection: ModelSelection, model: Pick<ResolvedProviderModel, 'reasoning'> | null | undefined) {
  if (selection.provider !== 'named_provider' && selection.provider !== 'platform_model' && selection.provider !== 'account_provider' && selection.provider !== 'computer_provider') return undefined
  return selection.reasoning_effort ?? model?.reasoning?.default_effort ?? undefined
}

export function withReasoningEffort(selection: ModelSelection, effort?: ReasoningEffort): ModelSelection {
  if (selection.provider !== 'named_provider' && selection.provider !== 'platform_model' && selection.provider !== 'account_provider' && selection.provider !== 'computer_provider') return selection
  const { reasoning_effort: _previous, ...model } = selection
  return effort === undefined ? model : { ...model, reasoning_effort: effort }
}

export type ModelReadinessStatus = 'loading' | 'error' | 'empty' | 'ready'

export interface ModelReadiness {
  status: ModelReadinessStatus
  canSubmit: boolean
  error: string
  retry(): void
}

export async function persistModelSelection(
  sessionId: string,
  model: ModelSelection,
  updateSession: (id: string, update: Record<string, unknown>) => Promise<unknown>,
  request: (path: string, options: { method: string; body: unknown }) => Promise<unknown>,
) {
  await updateSession(sessionId, { model })
  await request(`/default-model?session_id=${encodeURIComponent(sessionId)}`, {
    method: 'PUT',
    body: model,
  })
}

export function ModelPicker({
  compact = false,
  disabled = false,
  rememberDefault = true,
  effectiveProfile = null,
  onConfigureModels,
  onReadinessChange,
  open,
  onOpenChange,
  defaultTenantId,
}: {
  compact?: boolean
  disabled?: boolean
  rememberDefault?: boolean
  effectiveProfile?: Profile | null
  onConfigureModels?(): void
  onReadinessChange?(readiness: ModelReadiness): void
  open?: boolean
  onOpenChange?(open: boolean): void
  defaultTenantId?: string
}) {
  const { platform, currentTenantId, currentSession: workbenchSession, currentWorkspace: workbenchWorkspace, catalog, updateSession, notify, serverIdentity } = useWorkbench()
  const currentSession = defaultTenantId ? null : workbenchSession
  const currentWorkspace = defaultTenantId ? null : workbenchWorkspace
  const t = useTranslate('model')
  const cloud = Boolean(platform && (currentSession?.placement ?? currentWorkspace?.placement) !== 'local_node')
  const edge = Boolean(platform && !cloud)
  const accounts = useAccountProviders(Boolean(platform && serverIdentity?.personal_tenant_id), serverIdentity?.personal_tenant_id)
  const [selecting, setSelecting] = React.useState(false)
  const [localMenuOpen, setLocalMenuOpen] = React.useState(false)
  const pathname = usePathname()
  const menuPath = React.useRef(pathname)
  const menuOpen = (open ?? localMenuOpen) && menuPath.current === pathname
  const setMenuOpen = (next: boolean) => { setLocalMenuOpen(next); onOpenChange?.(next) }
  const narrowMenu = React.useSyncExternalStore(subscribeMenuViewport, () => window.innerWidth <= 760)
  const [mobilePanel, setMobilePanel] = React.useState<'models' | 'reasoning' | null>(null)
  React.useEffect(() => {
    if (menuPath.current === pathname) return
    menuPath.current = pathname
    setLocalMenuOpen(false)
    setMobilePanel(null)
    onOpenChange?.(false)
  }, [pathname, onOpenChange])
  const target = React.useMemo<ExecutionTarget>(() => ({
    tenantId: defaultTenantId ?? (platform ? currentTenantId : undefined),
    sessionId: currentSession?.identity.session_id,
    workspaceId: currentWorkspace?.workspace_id,
    placement: currentSession?.placement ?? currentWorkspace?.placement,
  }), [defaultTenantId, platform, currentTenantId, currentSession?.identity.session_id, currentWorkspace?.workspace_id, currentSession?.placement, currentWorkspace?.placement])
  const targetKey = executionTargetKey(target)
  const [cachedProviders, setProviders] = React.useState<ProviderProfile[]>(() => peekProviderInventory(target)?.providers ?? [])
  const [cachedCredentials, setCredentials] = React.useState<CredentialInventory | null>(() => peekProviderInventory(target)?.credentials ?? null)
  const [providerStatus, setStatus] = React.useState<Exclude<ModelReadinessStatus, 'empty'>>(() => (
    peekProviderInventory(target) ? 'ready' : 'loading'
  ))
  const [providerError, setLoadError] = React.useState('')
  const loadGeneration = React.useRef(0)
  const previousTarget = React.useRef({ key: targetKey, status: currentWorkspace?.status })
  const [loadedCloud, setCloudInventory] = React.useState(() => ({ key: targetKey, inventory: peekCloudModelInventory(target) }))
  const cloudInventory = loadedCloud.key === targetKey ? loadedCloud.inventory : null
  const selection = currentSession?.model ?? cloudInventory?.current?.selection ?? { provider: 'profile_default' as const }
  const serverBacked = cloud || Boolean(platform && (selection.provider === 'account_provider' || selection.provider === 'platform_model' || selection.provider === 'computer_provider'))
  const sharedAccount = selection.provider === 'account_provider' && platform && selection.owner_user_id !== serverIdentity?.user.user_id
  const selectionKey = modelSelectionKey(selection)
  const [cloudStatus, setCloudStatus] = React.useState<'loading' | 'ready' | 'error'>('loading')
  const [cloudError, setCloudError] = React.useState('')
  const providers = cloud ? cloudInventory?.providers ?? [] : cachedProviders
  const credentials = cloud ? cloudInventory?.credentials ?? null : cachedCredentials
  const status = cloud ? cloudStatus : providerStatus
  const loadError = cloud ? cloudError : providerError
  const selectedProviders = sharedAccount || selection.provider === 'computer_provider' ? [] : selection.provider === 'account_provider' ? accounts.inventory?.providers ?? [] : providers
  const cloudGeneration = React.useRef(0)
  const loadCloud = React.useCallback(async (force = false) => {
    if (!platform) return
    const generation = ++cloudGeneration.current
    if (!peekCloudModelInventory(target)) setCloudStatus('loading')
    setCloudError('')
    try {
      const inventory = await loadCloudModelInventory(force, target)
      if (generation !== cloudGeneration.current) return
      setCloudInventory({ key: targetKey, inventory }); setCloudStatus('ready')
    } catch (cause) {
      if (generation !== cloudGeneration.current) return
      setCloudError(cause instanceof Error ? cause.message : String(cause)); setCloudStatus('error')
    }
  }, [platform, targetKey])
  React.useEffect(() => {
    if (!platform) { setCloudInventory({ key: targetKey, inventory: null }); return }
    const unsubscribe = subscribeCloudModelInventory(() => {
      const inventory = peekCloudModelInventory(target)
      if (inventory) setCloudInventory({ key: targetKey, inventory })
      else void loadCloud()
    }, target)
    void loadCloud(true)
    return () => { cloudGeneration.current += 1; unsubscribe() }
  }, [platform, targetKey, selectionKey, loadCloud])
  const cloudCurrent = currentCloudModel(cloudInventory, selection)


  const load = React.useCallback(async (force = false) => {
    if (cloud) return
    const generation = ++loadGeneration.current
    const existing = peekProviderInventory(target)
    if (!existing) setStatus('loading')
    setLoadError('')
    try {
      const inventory = await loadProviderInventory(force, target)
      if (generation !== loadGeneration.current) return
      setProviders(inventory.providers)
      setCredentials(inventory.credentials)
      setStatus('ready')
    } catch (cause) {
      if (generation !== loadGeneration.current) return
      const message = cause instanceof Error ? cause.message : String(cause)
      setLoadError(message)
      setStatus(existing ? 'ready' : 'error')
    }
  }, [targetKey, cloud])

  React.useEffect(() => {
    if (cloud) return
    const existing = peekProviderInventory(target)
    setProviders(existing?.providers ?? [])
    setCredentials(existing?.credentials ?? null)
    setStatus(existing ? 'ready' : 'loading')
    const unsubscribe = subscribeProviderInventory(inventory => {
      if (!inventory) {
        setProviders([])
        setCredentials(null)
        setStatus('loading')
        return
      }
      setProviders(inventory.providers)
      setCredentials(inventory.credentials)
      setLoadError('')
      setStatus('ready')
    }, target)
    void load()
    return () => {
      loadGeneration.current += 1
      unsubscribe()
    }
  }, [load, targetKey, cloud])

  React.useEffect(() => {
    const previous = previousTarget.current
    previousTarget.current = { key: targetKey, status: currentWorkspace?.status }
    if (previous.key === targetKey && previous.status === 'offline' && currentWorkspace?.status === 'online') {
      invalidateProviderInventory(target)
      void load(true)
    }
  }, [currentWorkspace?.status, load, targetKey])

  const selected = selectedProviderModel(selection, providers)
  // Another client may select a model added after this catalog was cached.
  const missingModelKey = selection.provider === 'named_provider' && !selected
    ? JSON.stringify([targetKey, selection.provider_id, selection.model])
    : null
  const checkedMissingModel = React.useRef<string | null>(null)
  React.useEffect(() => {
    if (!missingModelKey) {
      checkedMissingModel.current = null
      return
    }
    if (cloud || status !== 'ready' || checkedMissingModel.current === missingModelKey) return
    checkedMissingModel.current = missingModelKey
    void load(true)
  }, [load, missingModelKey, status, cloud])
  const selectedDefaults = serverBacked ? cloudCurrent?.model?.defaults : selected?.resolved
  const reasoning = serverBacked ? cloudCurrent?.selectable_reasoning : selectedDefaults?.reasoning
  const currentEffort = effectiveEffort(selection, selectedDefaults)
  const profileAvailable = profileModelIsUsable(effectiveProfile, catalog, credentials)
  const selectedLabel = cloud && selection.provider === 'profile_default' ? t('cloud.choose') : selection.provider === 'profile_default' && effectiveProfile && !profileAvailable
    ? t('provider.configure')
    : modelLabel(selection, selectedProviders, t('default'), cloudCurrent)
  const triggerLabel = `${selectedLabel}${selection.provider === 'computer_provider' ? ` · ${cloudCurrent?.source_name || t('source.computer')}` : selection.provider === 'account_provider' ? ` · ${t(sharedAccount ? 'source.sharedAccount' : 'source.account')}` : edge && selection.provider === 'platform_model' ? ` · ${t('cloud.platform')}` : edge ? ` · ${t('source.node')}` : cloud && selection.provider === 'named_provider' ? ` · ${t('cloud.byok')}` : ''}${currentEffort ? ` · ${currentEffort}` : ''}`
  const usableModels = usableProviderModels(providers, credentials)
  const providerCatalogEmpty = status === 'ready' && usableModels === 0
  const profilePending = selection.provider === 'profile_default' && (!effectiveProfile || !catalog)
  const readinessStatus: ModelReadinessStatus = selecting ? 'loading' : serverBacked
    ? cloudStatus !== 'ready' ? cloudStatus : selection.provider === 'profile_default' ? 'empty' : 'ready'
    : selection.provider === 'open_ai_compatible' || profileAvailable
    ? 'ready'
    : profilePending ? 'loading'
    : providerCatalogEmpty ? 'empty' : status
  const canSubmit = readinessStatus === 'ready' && (serverBacked ? cloudCurrent?.available === true : modelSelectionIsUsable(selection, providers, credentials, profileAvailable))
  const readinessError = serverBacked ? cloudError || (cloudCurrent && !cloudCurrent.available ? t('cloud.unavailable') : '') : loadError
  const readiness = React.useMemo<ModelReadiness>(() => ({
    status: readinessStatus,
    canSubmit,
    error: readinessError,
    retry: () => { if (platform) void loadCloud(true); void load(true); void accounts.load(true) },
  }), [canSubmit, serverBacked, load, loadCloud, accounts.load, readinessError, readinessStatus])

  React.useEffect(() => { onReadinessChange?.(readiness) }, [onReadinessChange, readiness])

  if (!currentSession && !cloud) return null

  const select = async (model: ModelSelection) => {
    if (disabled || selecting) return
    setSelecting(true)
    try {
      if (currentSession) {
        await updateSession(currentSession.identity.session_id, { model })
        if (platform) invalidateCloudModelInventory(target)
        if (!rememberDefault || model.provider === 'account_provider' || model.provider === 'computer_provider' || (edge && model.provider === 'platform_model')) {
          notify(t('updated'))
          return
        }
      }
      try {
        await api.request(executionTargetPath('/default-model', target), { headers: executionTargetHeaders(target), method: 'PUT', body: model })
        if (platform) invalidateCloudModelInventory(target)
        notify(t(currentSession ? 'updated' : 'default.updated'))
      } catch (cause) {
        const message = cause instanceof Error ? cause.message : String(cause)
        notify(currentSession ? t('defaultSaveFailed', { message }) : message, 'error')
      }
    } catch (cause) {
      notify(cause instanceof Error ? cause.message : String(cause), 'error')
    } finally {
      setSelecting(false)
    }
  }

  const changeMenuOpen = (open: boolean) => {
    setMenuOpen(open)
    setMobilePanel(null)
    if (open) { void load(true); void accounts.load(true); if (serverBacked) void loadCloud(true) }
  }
  const touchTrigger = React.useRef(false)
  const configureModels = () => { setMenuOpen(false); onConfigureModels?.() }
  const modelItems = <>
              {platform && cloudCurrent?.owner_user_id && cloudCurrent.owner_user_id !== serverIdentity?.user.user_id && cloudCurrent.model && <>
                <DropdownMenuLabel>{t('source.sessionDelegation')}</DropdownMenuLabel>
                <DropdownMenuItem data-model-source="delegated" disabled={!cloudCurrent.available} onSelect={() => void select(selection)}>
                  <div className="min-w-0 flex-1"><div>{cloudCurrent.model.display_name}</div><div className="text-xs text-muted-foreground">{cloudCurrent.source_name} · {cloudCurrent.model.model_id}</div></div><Check />
                </DropdownMenuItem>
              </>}
              {!cloud && <DropdownMenuItem disabled={!profileAvailable} onSelect={() => void select({ provider: 'profile_default' })}>
                <CircleGauge /><div className="min-w-0 flex-1"><div>{t('inherit')}</div><div className="text-xs text-muted-foreground">{t('inherit.description')}</div></div>
                {selection.provider === 'profile_default' && <Check />}
              </DropdownMenuItem>}
              {platform && serverIdentity && <>
                <ProviderModelItems providers={accounts.inventory?.providers ?? []} credentials={accounts.inventory?.credentials ?? null} selection={selection} accountOwner={serverIdentity.user.user_id} label={t('source.account')} onSelect={model => void select(model)} />
                {accounts.error && <div className="px-3 py-2 text-xs text-destructive" role="alert">{accounts.error}<Button variant="ghost" size="xs" onClick={() => void accounts.load(true)}>{t('provider.retry')}</Button></div>}
              </>}
              {!cloud && <ProviderModelItems providers={platform ? providers : providers.filter(provider => !isConnectionProvider(provider.id))} credentials={credentials} selection={selection} label={t('source.node')} onSelect={model => void select(model)} />}
              {!platform && providers.some(provider => isConnectionProvider(provider.id) && !isAccountConnectionProvider(provider.id)) && <ProviderModelItems providers={providers.filter(provider => isConnectionProvider(provider.id) && !isAccountConnectionProvider(provider.id))} credentials={credentials} selection={selection} label={t('source.connectedPlatform')} onSelect={model => void select(model)} />}
              {!platform && providers.some(provider => isAccountConnectionProvider(provider.id)) && <ProviderModelItems providers={providers.filter(provider => isAccountConnectionProvider(provider.id))} credentials={credentials} selection={selection} label={t('source.connectedAccount')} onSelect={model => void select(model)} />}
              {edge && currentTenantId && <ComputerModelItems key={`computer:${targetKey}`} tenantId={currentTenantId} executionComputerId={currentWorkspace?.node_id ?? undefined} selection={selection} onSelect={model => void select(model)} />}
              {platform && <PlatformModelItems key={`platform:${targetKey}`} target={target} selection={selection} onSelect={model => void select(model)} />}
  </>
  const reasoningItems = reasoning ? <>
              <DropdownMenuItem onSelect={() => void select(withReasoningEffort(selection))}>
                <div className="min-w-0 flex-1"><div>{t('effort.modelDefault')} · {reasoning.default_effort}</div><div className="text-xs text-muted-foreground">{t('effort.modelDefaultDescription')}</div></div>
                {(selection.provider === 'named_provider' || selection.provider === 'platform_model' || selection.provider === 'account_provider' || selection.provider === 'computer_provider') && selection.reasoning_effort === undefined && currentEffort === reasoning.default_effort && <Check />}
              </DropdownMenuItem>
              <DropdownMenuSeparator />
              {supportedReasoningEfforts(reasoning).map(effort => (
                <DropdownMenuItem key={effort} onSelect={() => void select(withReasoningEffort(selection, effort))}>
                  <code className="min-w-0 flex-1 text-sm">{effort}</code>
                  {(selection.provider === 'named_provider' || selection.provider === 'platform_model' || selection.provider === 'account_provider' || selection.provider === 'computer_provider') && selection.reasoning_effort === effort && <Check />}
                </DropdownMenuItem>
              ))}
  </> : null

  return (
    <>
    <DropdownMenu key={pathname} open={menuOpen} onOpenChange={changeMenuOpen}>
      <DropdownMenuTrigger asChild
        onPointerDown={event => {
          // Open after touch release so the same tap cannot hit new menu content.
          touchTrigger.current = event.pointerType === 'touch'
          if (touchTrigger.current) event.preventDefault()
        }}
        onPointerCancel={() => { touchTrigger.current = false }}
        onKeyDown={() => { touchTrigger.current = false }}
        onClick={() => {
          if (!touchTrigger.current) return
          touchTrigger.current = false
          changeMenuOpen(!menuOpen)
        }}>
        <Button type="button" data-model-picker="" disabled={disabled || selecting} size={compact ? 'xs' : 'sm'} variant="ghost" className={css.trigger} title={triggerLabel}>
          <Sparkles className="size-3.5" /><span className="truncate">{triggerLabel}</span><ChevronsUpDown className="size-3" />
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="start" side="top" collisionPadding={12} className="w-72 max-w-[calc(100vw-24px)] max-h-[min(440px,60dvh)] overflow-y-auto">
        {narrowMenu && mobilePanel ? <>
          <DropdownMenuItem onSelect={event => { event.preventDefault(); setMobilePanel(null) }}><ArrowLeft />{t('menu.back')}</DropdownMenuItem>
          <DropdownMenuSeparator />
          <DropdownMenuLabel>{t(mobilePanel === 'models' ? 'model' : 'effort')}</DropdownMenuLabel>
          {mobilePanel === 'models' ? modelItems : reasoningItems}
        </> : <>
        <DropdownMenuLabel>{t(currentSession ? 'session.title' : 'default.title')}</DropdownMenuLabel>
        {sharedAccount && selection.provider === 'account_provider' && <div className="px-2 pb-2 text-xs text-muted-foreground">{t('source.sharedAccount')}<span className="mt-1 block break-all font-mono">{selection.owner_user_id}</span></div>}
        {narrowMenu ? <DropdownMenuItem onSelect={event => { event.preventDefault(); setMobilePanel('models') }}><span>{t('model')}</span><span className="ml-auto max-w-40 truncate text-xs text-muted-foreground">{modelLabel(selection, selectedProviders, t('default'), cloudCurrent)}</span><ChevronRight /></DropdownMenuItem> : <DropdownMenuSub>
          <DropdownMenuSubTrigger><span>{t('model')}</span><span className="ml-auto max-w-40 truncate text-xs text-muted-foreground">{modelLabel(selection, selectedProviders, t('default'), cloudCurrent)}</span></DropdownMenuSubTrigger>
          <DropdownMenuPortal>
            <DropdownMenuSubContent className="max-h-[min(480px,70dvh)] w-80 max-w-[calc(100vw-24px)] overflow-y-auto">
              {modelItems}
            </DropdownMenuSubContent>
          </DropdownMenuPortal>
        </DropdownMenuSub>}
        {reasoning && (narrowMenu ? <DropdownMenuItem onSelect={event => { event.preventDefault(); setMobilePanel('reasoning') }}><span>{t('effort')}</span><span className="ml-auto font-mono text-xs text-muted-foreground">{currentEffort ?? t('effort.notSelected')}</span><ChevronRight /></DropdownMenuItem> : <DropdownMenuSub>
          <DropdownMenuSubTrigger><span>{t('effort')}</span><span className="ml-auto font-mono text-xs text-muted-foreground">{currentEffort ?? t('effort.notSelected')}</span></DropdownMenuSubTrigger>
          <DropdownMenuPortal>
            <DropdownMenuSubContent className="w-80 max-w-[calc(100vw-24px)]">
              {reasoningItems}
            </DropdownMenuSubContent>
          </DropdownMenuPortal>
        </DropdownMenuSub>)}
        {!reasoning && selection.provider !== 'profile_default' && <div className="px-2 py-2 text-xs text-muted-foreground" data-reasoning-unavailable=""><span className="block font-medium">{t('effort')}</span><p className="mt-1 leading-relaxed">{t(selection.provider === 'platform_model' ? 'effort.platformUnavailable' : 'effort.unavailable')}</p></div>}
        {status === 'loading' && (!cloud || !cloudInventory) && <div className="px-3 py-5 text-center text-xs text-muted-foreground" role="status">{t('provider.loading')}</div>}
        {status === 'error' && <div className="grid gap-2 px-3 py-4 text-center text-xs" role="alert"><span className="break-words text-destructive">{t('provider.loadFailed', { message: loadError })}</span><Button type="button" size="sm" variant="outline" onClick={() => void load(true)}>{t('provider.retry')}</Button></div>}
        {!serverBacked && providerCatalogEmpty && !(accounts.inventory?.providers.length) && <div className="grid gap-2 px-3 py-4 text-center text-xs text-muted-foreground"><span>{providers.length ? t('provider.noUsable') : t('provider.empty')}</span>{!platform && onConfigureModels && <Button type="button" size="sm" variant="outline" onClick={configureModels}>{t('provider.configure')}</Button>}</div>}
        {platform && ((cloudCurrent && !cloudCurrent.available) || cloudStatus === 'error') && <div className="grid gap-2 px-3 py-3 text-xs text-muted-foreground">
          {cloudCurrent && !cloudCurrent.available && <span role="alert">{t('cloud.unavailable')}</span>}
          {cloudStatus === 'error' && <span role="alert">{cloudError}</span>}
        </div>}
        </>}
      </DropdownMenuContent>
    </DropdownMenu>

    </>
  )
}
