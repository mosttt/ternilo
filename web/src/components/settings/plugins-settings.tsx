import * as React from 'react'
import { Ban, Box, ChevronDown, PackagePlus, Search, Trash2 } from 'lucide-react'
import { api } from '@/api/client'
import { Button } from '@/components/ui/button'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { Field, FieldDescription, Input, Label } from '@/components/ui/field'
import { Switch } from '@/components/ui/switch'
import { localizePluginMetadata } from '@/i18n/builtin-metadata'
import { useTranslate } from '@/i18n/provider'
import { useWorkbench } from '@/state/workbench'
import type {
  ExtensionInventory,
  ExtensionPackageManifest,
  ExtensionProviderContribution,
  PluginCatalogEntry,
  PluginEntry,
  Profile,
  ProviderProfile,
  SignedExtensionBundle,
} from '@/types'
import { cn } from '@/lib/utils'
import { executionTargetKey, executionTargetPath, type ExecutionTarget } from '@/domain/execution-target'
import { invalidateProviderInventory, loadProviderInventory } from '@/domain/provider-inventory'
import { PluginConfigCard } from './plugin-config-card'
import { configRecord, pluginConfigDefaults, pluginConfigFields } from './plugin-config'
import { ActionDialog, SectionHeader, SettingsTabPanel, SettingsTabs } from './settings-ui'
import styles from './settings-layout.module.css'

const emptyMetadata = (kind: string): PluginCatalogEntry => ({
  kind,
  description: '',
  requires: [],
  provides: [],
  config_schema: { type: 'object', additionalProperties: true },
})

type LifecycleTarget =
  | { kind: 'uninstall'; packageId: string; version: string; label: string }
  | {
      kind: 'extension-revoke'
      packageId: string
      version: string
      label: string
    }
  | { kind: 'publisher'; keyId: string; label: string }

interface ExtensionInstallDraft {
  bundle: SignedExtensionBundle
  fileName: string
  grants: Set<string>
}

interface ProviderMaterializeDraft {
  packageId: string
  version: string
  template: ExtensionProviderContribution
  providerId: string
  apiKeyRef: string
}

function extensionMount(
  entries: PluginEntry[] | undefined,
  packageId: string,
  version: string,
) {
  return entries?.find((entry) => {
    const config = configRecord(entry.config)
    return (
      entry.kind === 'ternilo.extension.package' &&
      config.package_id === packageId &&
      config.version === version
    )
  })
}

function upsertProfileEntry(entries: PluginEntry[], nextEntry: PluginEntry) {
  const position = entries.findIndex((entry) => entry.id === nextEntry.id)
  if (position < 0) return [...entries, nextEntry]
  return entries.map((entry, index) => (index === position ? nextEntry : entry))
}

function hasMissingRequiredSettings(schema: Record<string, unknown>, settings: Record<string, unknown>) {
  return pluginConfigFields(schema).some((field) => field.required && settings[field.key] === undefined)
}

function isExtensionSkillContribution(value: unknown) {
  if (!value || typeof value !== 'object' || Array.isArray(value)) return false
  const skill = value as Record<string, unknown>
  const invocation = skill.invocation
  return (
    typeof skill.name === 'string' && skill.name.trim().length > 0 &&
    typeof skill.description === 'string' && skill.description.trim().length > 0 &&
    typeof skill.content === 'string' && skill.content.trim().length > 0 &&
    (skill.when_to_use === undefined || skill.when_to_use === null || typeof skill.when_to_use === 'string') &&
    (invocation === undefined || (
      Boolean(invocation) &&
      typeof invocation === 'object' &&
      !Array.isArray(invocation) &&
      typeof (invocation as Record<string, unknown>).model_invocable === 'boolean' &&
      typeof (invocation as Record<string, unknown>).user_invocable === 'boolean'
    ))
  )
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return Boolean(value) && typeof value === 'object' && !Array.isArray(value)
}

function isExtensionHookContribution(value: unknown) {
  if (!isRecord(value) || !isRecord(value.matcher)) return false
  const point = value.point
  const matcher = value.matcher
  const pointValid = [
    'session_start',
    'user_prompt_submit',
    'pre_tool_use',
    'post_tool_use',
    'stop',
  ].includes(String(point))
  const matcherValid = matcher.kind === 'all' || (
    matcher.kind === 'tool_names' &&
    (point === 'pre_tool_use' || point === 'post_tool_use') &&
    Array.isArray(matcher.names) &&
    matcher.names.length > 0 &&
    matcher.names.every((name) => typeof name === 'string' && name.trim().length > 0)
  )
  return (
    typeof value.id === 'string' && value.id.trim().length > 0 &&
    typeof value.handler === 'string' && value.handler.trim().length > 0 &&
    pointValid && matcherValid
  )
}

function isExtensionCommandContribution(value: unknown, toolNames: Set<string>) {
  if (!isRecord(value) || !isRecord(value.fixed_arguments)) return false
  const input = value.input
  const inputValid = input === undefined || input === null || (
    isRecord(input) &&
    typeof input.hint === 'string' && input.hint.trim().length > 0 &&
    typeof input.field === 'string' && input.field.trim().length > 0 &&
    (input.images === undefined || input.images === false)
  )
  return (
    typeof value.name === 'string' && value.name.trim().length > 0 &&
    !['feedback', 'plan', 'skill'].includes(value.name) &&
    typeof value.description === 'string' && value.description.trim().length > 0 &&
    typeof value.tool === 'string' && toolNames.has(value.tool) &&
    inputValid
  )
}

function isProviderModelDefaults(value: unknown) {
  if (!isRecord(value)) return false
  return (
    typeof value.context_window === 'number' && value.context_window > 0 &&
    typeof value.max_output_tokens === 'number' && value.max_output_tokens > 0
  )
}

function isProviderModelValues(value: unknown) {
  if (!isRecord(value)) return false
  const reasoning = value.reasoning
  return (value.context_window == null || typeof value.context_window === 'number' && value.context_window > 0)
    && (value.max_output_tokens == null || typeof value.max_output_tokens === 'number' && value.max_output_tokens > 0)
    && (reasoning == null || isRecord(reasoning) && (reasoning.mode === 'disabled' || reasoning.mode === 'enabled' && isRecord(reasoning.configuration)))
}

function isExtensionProviderContribution(value: unknown) {
  if (!isRecord(value) || !isProviderModelDefaults(value.defaults) || !isRecord(value.credential)) return false
  return (
    typeof value.id === 'string' && value.id.trim().length > 0 &&
    typeof value.display_name === 'string' && value.display_name.trim().length > 0 &&
    typeof value.base_url === 'string' && /^https?:\/\//.test(value.base_url) &&
    (value.protocol === 'openai-chat-completions' || value.protocol === 'openai-responses' || value.protocol === 'deepseek-responses' || value.protocol === 'google-gemini' || value.protocol === 'anthropic-messages') &&
    Array.isArray(value.models) && value.models.length > 0 &&
    value.models.every((model) => {
      if (!isRecord(model) || typeof model.id !== 'string' || !isRecord(model.settings)) return false
      return model.settings.mode === 'inherit' || (
        model.settings.mode === 'override' && isProviderModelDefaults(model.settings)
      ) || (model.settings.mode === 'automatic' && isProviderModelValues(model.settings.upstream ?? {}) && isProviderModelValues(model.settings.overrides ?? {}))
    }) &&
    Number.isSafeInteger(value.timeout_ms) && Number(value.timeout_ms) >= 0 &&
    Number.isInteger(value.max_attempts) && Number(value.max_attempts) >= 1 && Number(value.max_attempts) <= 8 &&
    typeof value.retry_base_delay_ms === 'number' && value.retry_base_delay_ms > 0 &&
    typeof value.credential.required === 'boolean' &&
    (value.credential.suggested_ref === undefined || value.credential.suggested_ref === null ||
      typeof value.credential.suggested_ref === 'string')
  )
}

function hasUniqueStrings(values: unknown[], field: string) {
  const items = values.map((value) => isRecord(value) ? value[field] : undefined)
  return items.every((value) => typeof value === 'string') && new Set(items).size === items.length
}

function ExtensionRuntimeContributions({
  manifest,
  review = false,
  onMaterialize,
}: {
  manifest: ExtensionPackageManifest
  review?: boolean
  onMaterialize?: (provider: ExtensionProviderContribution) => void
}) {
  const t = useTranslate('settings')
  const { hooks, commands, providers } = manifest.contributions
  return <>
    {hooks.length > 0 ? (
      <section
        className={review ? 'rounded-lg border p-4' : 'mt-3 border-t pt-3'}
        data-extension-hooks={review ? undefined : ''}
        data-extension-hooks-review={review ? '' : undefined}
      >
        <h3 className={review ? 'text-sm font-medium' : 'text-xs text-muted-foreground'}>{t('plugins.hooks')}</h3>
        <div className={review ? 'mt-3 grid gap-2' : 'mt-2 grid gap-2'}>
          {hooks.map((hook) => (
            <div
              className="min-w-0 rounded-md bg-muted/30 px-3 py-2 text-xs"
              key={hook.id}
              data-extension-hook={review ? undefined : hook.id}
              data-extension-hook-review={review ? hook.id : undefined}
            >
              <div className="flex flex-wrap items-baseline gap-x-2 gap-y-1">
                <code className="break-all">{hook.id}</code>
                <span className="text-muted-foreground">· {hook.point} · {hook.handler}</span>
              </div>
              <p className="mt-1 break-words text-muted-foreground">
                {hook.matcher.kind === 'all'
                  ? t('plugins.hookAllTools')
                  : `${t('plugins.hookMatchedTools')}: ${hook.matcher.names.join(' · ')}`}
              </p>
            </div>
          ))}
        </div>
      </section>
    ) : null}
    {commands.length > 0 ? (
      <section
        className={review ? 'rounded-lg border p-4' : 'mt-3 border-t pt-3'}
        data-extension-commands={review ? undefined : ''}
        data-extension-commands-review={review ? '' : undefined}
      >
        <h3 className={review ? 'text-sm font-medium' : 'text-xs text-muted-foreground'}>{t('plugins.commands')}</h3>
        <div className={review ? 'mt-3 grid gap-2' : 'mt-2 grid gap-2'}>
          {commands.map((command) => (
            <div
              className="min-w-0 rounded-md bg-muted/30 px-3 py-2 text-xs"
              key={command.name}
              data-extension-command={review ? undefined : command.name}
              data-extension-command-review={review ? command.name : undefined}
            >
              <div className="flex flex-wrap items-baseline gap-x-2 gap-y-1">
                <code>/{command.name}</code>
                <span className="text-muted-foreground">· {command.tool}</span>
              </div>
              <p className="mt-1 break-words">{command.description}</p>
              {command.input ? (
                <p className="mt-1 break-words text-muted-foreground">
                  {command.input.hint} → {command.input.field}{command.input.images ? ' · images' : ''}
                </p>
              ) : null}
              <pre className="mt-2 max-h-32 overflow-auto whitespace-pre-wrap break-all rounded bg-background/60 p-2 font-mono leading-relaxed" data-extension-command-arguments="">
                <code>{JSON.stringify(command.fixed_arguments, null, 2)}</code>
              </pre>
            </div>
          ))}
        </div>
      </section>
    ) : null}
    {providers.length > 0 ? (
      <section
        className={review ? 'rounded-lg border p-4' : 'mt-3 border-t pt-3'}
        data-extension-providers={review ? undefined : ''}
        data-extension-providers-review={review ? '' : undefined}
      >
        <h3 className={review ? 'text-sm font-medium' : 'text-xs text-muted-foreground'}>{t('plugins.providers')}</h3>
        <div className={review ? 'mt-3 grid gap-2' : 'mt-2 grid gap-2'}>
          {providers.map((provider) => (
            <div
              className="min-w-0 rounded-md bg-muted/30 px-3 py-2 text-xs"
              key={provider.id}
              data-extension-provider={review ? undefined : provider.id}
              data-extension-provider-review={review ? provider.id : undefined}
            >
              <div className="flex min-w-0 flex-wrap items-start gap-2">
                <div className="min-w-0 flex-1">
                  <div className="font-medium">{provider.display_name}</div>
                  <code className="mt-1 block break-all text-muted-foreground">{provider.id} · {provider.protocol}</code>
                </div>
                {onMaterialize ? (
                  <Button type="button" size="sm" variant="outline" onClick={() => onMaterialize(provider)}>
                    <PackagePlus />{t('plugins.addProvider')}
                  </Button>
                ) : null}
              </div>
              <code className="mt-2 block break-all">{provider.base_url}</code>
              <p className="mt-1 break-words text-muted-foreground">
                {provider.models.map((model) => model.id).join(' · ')}
              </p>
              <p className="mt-1 break-words text-muted-foreground">
                context_window={provider.defaults.context_window} · max_output_tokens={provider.defaults.max_output_tokens}
              </p>
              <p className="mt-1 break-words text-muted-foreground">
                timeout_ms={provider.timeout_ms} · max_attempts={provider.max_attempts} · retry_base_delay_ms={provider.retry_base_delay_ms}
              </p>
              <p className="mt-1 break-words text-muted-foreground">
                {t(provider.credential.required ? 'plugins.credentialRequired' : 'plugins.credentialOptional')}
                {provider.credential.suggested_ref ? ` · ${provider.credential.suggested_ref}` : ''}
              </p>
            </div>
          ))}
        </div>
      </section>
    ) : null}
  </>
}

export function PluginsSettings({ onChanged }: { onChanged(): Promise<void> }) {
  const { currentSession, currentWorkspace, catalog, updateSession, notify } = useWorkbench()
  const t = useTranslate('settings')
  const builtins = useTranslate('builtins')
  const common = useTranslate('common')
  const [activeTab, setActiveTab] = React.useState('configuration')
  const [profile, setProfile] = React.useState<Profile | null>(null)
  const [inventory, setInventory] = React.useState<ExtensionInventory | null>(null)
  const [loadingProfile, setLoadingProfile] = React.useState(false)
  const [profileError, setProfileError] = React.useState('')
  const [loadingInventory, setLoadingInventory] = React.useState(true)
  const [inventoryError, setInventoryError] = React.useState('')
  const [catalogQuery, setCatalogQuery] = React.useState('')
  const [expandedKind, setExpandedKind] = React.useState<string | null>(null)
  const [status, setStatus] = React.useState('')
  const [lifecycleTarget, setLifecycleTarget] = React.useState<LifecycleTarget | null>(null)
  const [lifecycleBusy, setLifecycleBusy] = React.useState(false)
  const [lifecycleError, setLifecycleError] = React.useState('')
  const [extensionMountBusy, setExtensionMountBusy] = React.useState<string | null>(null)
  const [extensionConfigureRequest, setExtensionConfigureRequest] = React.useState<{
    key: string
    revision: number
  } | null>(null)
  const [extensionInstall, setExtensionInstall] = React.useState<ExtensionInstallDraft | null>(null)
  const [installingExtension, setInstallingExtension] = React.useState(false)
  const [extensionInstallError, setExtensionInstallError] = React.useState('')
  const [providerMaterialize, setProviderMaterialize] = React.useState<ProviderMaterializeDraft | null>(null)
  const [providerMaterializeBusy, setProviderMaterializeBusy] = React.useState(false)
  const [providerMaterializeError, setProviderMaterializeError] = React.useState('')
  const target = React.useMemo<ExecutionTarget>(
    () => ({
      sessionId: currentSession?.identity.session_id,
      workspaceId: currentWorkspace?.workspace_id,
    }),
    [currentSession?.identity.session_id, currentWorkspace?.workspace_id],
  )
  const targetKey = executionTargetKey(target)

  const loadProfile = React.useCallback(async () => {
    if (!currentSession) {
      setProfile(null)
      return
    }
    setLoadingProfile(true)
    setProfileError('')
    try {
      setProfile(
        await api.request<Profile>(`/sessions/${encodeURIComponent(currentSession.identity.session_id)}/plugins`),
      )
    } catch (cause) {
      setProfileError(cause instanceof Error ? cause.message : String(cause))
    } finally {
      setLoadingProfile(false)
    }
  }, [currentSession, notify])

  const loadInventory = React.useCallback(async () => {
    setLoadingInventory(true)
    setInventoryError('')
    try {
      setInventory(await api.request<ExtensionInventory>(executionTargetPath('/extensions', target)))
    } catch (cause) {
      setInventoryError(cause instanceof Error ? cause.message : String(cause))
    } finally {
      setLoadingInventory(false)
    }
  }, [targetKey])

  React.useEffect(() => {
    void loadProfile()
  }, [loadProfile])
  React.useEffect(() => {
    setInventory(null)
    void loadInventory()
  }, [loadInventory, targetKey])

  const writeOverrides = async (overrides: PluginEntry[]) => {
    if (!currentSession) throw new Error(t('plugins.noSession'))
    await updateSession(currentSession.identity.session_id, {
      profile_plugins: overrides,
    })
    await Promise.all([loadProfile(), onChanged()])
  }

  const saveEntry = async (entry: PluginEntry) => {
    if (!currentSession) throw new Error(t('plugins.noSession'))
    await writeOverrides([...currentSession.profile_plugins.filter((item) => item.id !== entry.id), entry])
    notify(t('plugins.saved'))
  }

  const resetEntry = async (id: string) => {
    if (!currentSession) throw new Error(t('plugins.noSession'))
    await writeOverrides(currentSession.profile_plugins.filter((item) => item.id !== id))
    notify(t('plugins.resetDone'))
  }

  const toggleEntry = async (entry: PluginEntry, enabled: boolean) => {
    if (!currentSession) throw new Error(t('plugins.noSession'))
    const next = { ...entry, enabled }
    await writeOverrides([...currentSession.profile_plugins.filter((item) => item.id !== entry.id), next])
    notify(enabled ? t('plugins.enabled') : t('plugins.disabled'))
  }

  const setExtensionState = async (packageId: string, version: string, enabled: boolean) => {
    setStatus('')
    try {
      await api.request(
        executionTargetPath(`/extensions/${encodeURIComponent(packageId)}/${encodeURIComponent(version)}`, target),
        { method: 'PUT', body: { enabled } },
      )
      await Promise.all([loadInventory(), loadProfile(), onChanged()])
      notify(enabled ? t('plugins.extensionEnabled') : t('plugins.extensionDisabled'))
    } catch (cause) {
      setStatus(cause instanceof Error ? cause.message : String(cause))
    }
  }

  const setExtensionMounted = async (
    packageId: string,
    version: string,
    configSchema: Record<string, unknown>,
    mounted: boolean,
    effectiveMount?: PluginEntry,
    sessionOverride?: PluginEntry,
    inherited = false,
  ) => {
    if (!currentSession) {
      setStatus(t('plugins.noSessionMount'))
      return
    }
    const key = `${packageId}@${version}`
    const rowId = `extension:${key}`
    setExtensionMountBusy(key)
    setStatus('')
    try {
      let next: PluginEntry[]
      if (mounted) {
        const nextMount = sessionOverride ?? effectiveMount ?? {
          id: rowId,
          kind: 'ternilo.extension.package',
          enabled: true,
          config: { package_id: packageId, version, settings: pluginConfigDefaults(configSchema) },
        }
        next = upsertProfileEntry(currentSession.profile_plugins, { ...nextMount, enabled: true })
      } else if (inherited && effectiveMount) {
        next = upsertProfileEntry(currentSession.profile_plugins, {
          ...(sessionOverride ?? effectiveMount),
          enabled: false,
        })
      } else {
        const mountId = sessionOverride?.id ?? effectiveMount?.id ?? rowId
        next = currentSession.profile_plugins.filter((entry) => entry.id !== mountId)
      }
      await writeOverrides(next)
      notify(t(mounted ? 'plugins.mounted' : 'plugins.unmounted'))
    } catch (cause) {
      setStatus(cause instanceof Error ? cause.message : String(cause))
    } finally {
      setExtensionMountBusy(null)
    }
  }

  const saveExtensionSettings = async (
    mount: PluginEntry,
    sessionOverride: PluginEntry | undefined,
    settings: unknown,
    mountAfterSave = false,
  ) => {
    if (!currentSession) throw new Error(t('plugins.noSessionMount'))
    const source = sessionOverride ?? mount
    const mountConfig = configRecord(source.config)
    const nextMount = {
      ...source,
      enabled: mountAfterSave ? true : source.enabled,
      config: {
        ...mountConfig,
        package_id: mountConfig.package_id,
        version: mountConfig.version,
        settings: configRecord(settings),
      },
    }
    await writeOverrides(upsertProfileEntry(currentSession.profile_plugins, nextMount))
    setExtensionConfigureRequest(null)
    notify(t('plugins.extensionSettingsSaved'))
  }

  const importJson = async (endpoint: string, file: File | undefined, bodyTransform?: (value: unknown) => unknown) => {
    if (!file) return
    setStatus('')
    try {
      const parsed = JSON.parse(await file.text())
      await api.request(executionTargetPath(endpoint, target), {
        method: 'POST',
        body: bodyTransform ? bodyTransform(parsed) : parsed,
      })
      await loadInventory()
      notify(t('plugins.imported'))
    } catch (cause) {
      setStatus(cause instanceof Error ? cause.message : String(cause))
    }
  }

  const reviewExtensionBundle = async (file: File | undefined) => {
    if (!file) return
    setStatus('')
    setExtensionInstallError('')
    try {
      const bundle = JSON.parse(await file.text()) as SignedExtensionBundle
      const runtimeKind = bundle?.manifest?.runtime?.kind
      const payloadKind = bundle?.payload?.kind
      const tools = bundle?.manifest?.contributions?.tools
      const promptSections = bundle?.manifest?.contributions?.prompt_sections
      const skills = bundle?.manifest?.contributions?.skills
      const hooks = bundle?.manifest?.contributions?.hooks
      const commands = bundle?.manifest?.contributions?.commands
      const providers = bundle?.manifest?.contributions?.providers
      const toolNames = new Set(
        Array.isArray(tools)
          ? tools.map((tool) => tool?.spec?.name).filter((name): name is string => typeof name === 'string')
          : [],
      )
      if (
        bundle?.manifest?.schema_version !== 1 ||
        !bundle?.manifest?.package_id ||
        !bundle.manifest.version ||
        !bundle.manifest.payload_sha256 ||
        !Array.isArray(bundle.manifest.requested_capabilities) ||
        !Array.isArray(tools) ||
        !Array.isArray(promptSections) ||
        !Array.isArray(skills) ||
        !Array.isArray(hooks) ||
        !Array.isArray(commands) ||
        !Array.isArray(providers) ||
        (tools.length === 0 && promptSections.length === 0 && skills.length === 0 &&
          hooks.length === 0 && commands.length === 0 && providers.length === 0) ||
        !skills.every(isExtensionSkillContribution) ||
        !hooks.every(isExtensionHookContribution) ||
        !commands.every((command) => isExtensionCommandContribution(command, toolNames)) ||
        !providers.every(isExtensionProviderContribution) ||
        !hasUniqueStrings(hooks, 'id') ||
        !hasUniqueStrings(commands, 'name') ||
        !hasUniqueStrings(providers, 'id') ||
        (runtimeKind !== 'rhai' && runtimeKind !== 'wasm-component') ||
        (payloadKind !== 'utf8' && payloadKind !== 'base64') ||
        typeof bundle.payload.content !== 'string' ||
        (runtimeKind === 'rhai' && payloadKind !== 'utf8') ||
        (runtimeKind === 'wasm-component' && payloadKind !== 'base64')
      ) {
        throw new Error(t('plugins.bundleInvalid'))
      }
      setExtensionInstall({
        bundle,
        fileName: file.name,
        grants: new Set(bundle.manifest.requested_capabilities),
      })
    } catch (cause) {
      setStatus(cause instanceof Error ? cause.message : String(cause))
    }
  }

  const installExtensionBundle = async () => {
    if (!extensionInstall) return
    setInstallingExtension(true)
    setExtensionInstallError('')
    try {
      await api.request(executionTargetPath('/extensions', target), {
        method: 'POST',
        body: {
          bundle: extensionInstall.bundle,
          granted_capabilities: [...extensionInstall.grants],
        },
      })
      await loadInventory()
      notify(t('plugins.imported'))
      setExtensionInstall(null)
    } catch (cause) {
      setExtensionInstallError(cause instanceof Error ? cause.message : String(cause))
    } finally {
      setInstallingExtension(false)
    }
  }

  const materializeExtensionProvider = async () => {
    if (!providerMaterialize) return
    setProviderMaterializeBusy(true)
    setProviderMaterializeError('')
    try {
      await api.request<ProviderProfile>(executionTargetPath('/providers/from-extension', target), {
        method: 'POST',
        body: {
          package_id: providerMaterialize.packageId,
          version: providerMaterialize.version,
          template: providerMaterialize.template.id,
          provider_id: providerMaterialize.providerId.trim(),
          api_key_ref: providerMaterialize.apiKeyRef.trim() || undefined,
        },
      })
      invalidateProviderInventory(target)
      await loadProviderInventory(true, target)
      notify(t('plugins.providerCreated'))
      setProviderMaterialize(null)
    } catch (cause) {
      setProviderMaterializeError(cause instanceof Error ? cause.message : String(cause))
    } finally {
      setProviderMaterializeBusy(false)
    }
  }

  const applyLifecycleAction = async () => {
    if (!lifecycleTarget) return
    setLifecycleBusy(true)
    setLifecycleError('')
    try {
      const endpoint =
        lifecycleTarget.kind === 'publisher'
          ? `/extensions/publishers/${encodeURIComponent(lifecycleTarget.keyId)}/revoke`
          : `/extensions/${encodeURIComponent(lifecycleTarget.packageId)}/${encodeURIComponent(lifecycleTarget.version)}${lifecycleTarget.kind === 'extension-revoke' ? '/revoke' : ''}`
      await api.request(executionTargetPath(endpoint, target), {
        method: lifecycleTarget.kind === 'uninstall' ? 'DELETE' : 'POST',
      })
      await Promise.all([loadInventory(), loadProfile(), onChanged()])
      notify(
        t(
          lifecycleTarget.kind === 'uninstall'
            ? 'plugins.extensionUninstalled'
            : lifecycleTarget.kind === 'extension-revoke'
              ? 'plugins.extensionRevoked'
              : 'plugins.publisherRevoked',
        ),
      )
      setLifecycleTarget(null)
    } catch (cause) {
      setLifecycleError(cause instanceof Error ? cause.message : String(cause))
    } finally {
      setLifecycleBusy(false)
    }
  }

  const normalizedQuery = catalogQuery.trim().toLocaleLowerCase()
  const localizedCatalog = (catalog?.plugins ?? []).map((plugin) => localizePluginMetadata(plugin, builtins))
  const filteredCatalog = localizedCatalog.filter(
    (plugin) =>
      !normalizedQuery ||
      `${plugin.kind} ${plugin.description} ${plugin.requires.join(' ')} ${plugin.provides.join(' ')}`
        .toLocaleLowerCase()
        .includes(normalizedQuery),
  )

  const tabs = [
    { id: 'configuration', label: t('plugins.configurationTab') },
    { id: 'catalog', label: t('plugins.catalogTab') },
    { id: 'extensions', label: t('plugins.extensionsTab') },
  ]

  return (
    <div className={styles.section}>
      <SectionHeader title={t('nav.plugins')} description={t('plugins.description')} />
      <SettingsTabs label={t('plugins.views')} tabs={tabs} active={activeTab} onChange={setActiveTab} />

      <SettingsTabPanel id="configuration" active={activeTab}>
        <div className="mb-4 rounded-xl border bg-muted/20 px-4 py-3" data-plugin-scope="session">
          <strong className="text-sm">{t('plugins.sessionScopeTitle')}</strong>
          <p className="mt-1 text-xs leading-relaxed text-muted-foreground">{t('plugins.sessionScopeDescription')}</p>
        </div>
        {profileError && profile ? (
          <p className="mb-4 text-sm text-destructive" role="alert">
            {profileError}
          </p>
        ) : null}
        {!currentSession ? (
          <div className="rounded-xl border border-dashed p-8 text-center text-sm text-muted-foreground">
            {t('plugins.noSession')}
          </div>
        ) : loadingProfile && !profile ? (
          <div className="rounded-xl border border-dashed p-8 text-center text-sm text-muted-foreground">
            {t('plugins.loading')}
          </div>
        ) : profileError && !profile ? (
          <div className="rounded-xl border border-destructive/40 p-6 text-center">
            <p className="text-sm text-destructive" role="alert">
              {profileError}
            </p>
            <Button className="mt-4" size="sm" variant="outline" onClick={() => void loadProfile()}>
              {common('retry')}
            </Button>
          </div>
        ) : profile?.plugins.length ? (
          <div className="space-y-3">
            {profile.plugins.map((entry) => {
              const metadata = localizedCatalog.find((item) => item.kind === entry.kind) ?? emptyMetadata(entry.kind)
              return (
                <PluginConfigCard
                  key={entry.id}
                  entry={entry}
                  metadata={metadata}
                  hostToolCallLimit={catalog?.host_limits?.max_tool_calls}
                  overridden={currentSession.profile_plugins.some((item) => item.id === entry.id)}
                  onToggle={(enabled) => toggleEntry(entry, enabled)}
                  onSave={saveEntry}
                  onReset={() => resetEntry(entry.id)}
                />
              )
            })}
          </div>
        ) : (
          <div className="rounded-xl border border-dashed p-8 text-center text-sm text-muted-foreground">
            {t('plugins.empty')}
          </div>
        )}
      </SettingsTabPanel>

      <SettingsTabPanel id="catalog" active={activeTab}>
        <label className={styles.search}>
          <Search aria-hidden="true" />
          <span className="sr-only">{t('plugins.search')}</span>
          <input
            type="search"
            value={catalogQuery}
            placeholder={t('plugins.search')}
            onChange={(event) => setCatalogQuery(event.target.value)}
          />
        </label>
        <div className="mb-3 mt-5 flex items-center justify-between gap-3">
          <h3 className="text-sm font-semibold">{t('plugins.available')}</h3>
          <span className="text-xs text-muted-foreground" data-plugin-count={filteredCatalog.length}>
            {filteredCatalog.length}
          </span>
        </div>
        {!catalog ? (
          <div className="rounded-xl border border-dashed p-8 text-center text-sm text-muted-foreground">
            {common('loading')}
          </div>
        ) : filteredCatalog.length ? (
          <div className="space-y-2">
            {filteredCatalog.map((plugin) => {
              const entries = profile?.plugins.filter((entry) => entry.kind === plugin.kind) ?? []
              const state = entries.some((entry) => entry.enabled)
                ? t('plugins.enabledTag')
                : entries.length
                  ? t('plugins.disabledTag')
                  : t('plugins.availableTag')
              const open = expandedKind === plugin.kind
              return (
                <article
                  className="overflow-hidden rounded-xl border bg-card"
                  key={plugin.kind}
                  data-plugin-kind={plugin.kind}
                >
                  <button
                    type="button"
                    className="flex w-full items-center gap-3 p-4 text-left"
                    aria-expanded={open}
                    onClick={() => setExpandedKind(open ? null : plugin.kind)}
                  >
                    <Box className="size-4 text-muted-foreground" />
                    <span className="min-w-0 flex-1">
                      <code className="text-xs">{plugin.kind}</code>
                      <span className="mt-1 block text-xs text-muted-foreground">
                        {plugin.description || t('plugins.noDescription')}
                      </span>
                    </span>
                    <span className="rounded bg-muted px-1.5 py-0.5 text-[10px] text-muted-foreground">{state}</span>
                    <ChevronDown className={cn('size-4 transition-transform', open && 'rotate-180')} />
                  </button>
                  {open ? (
                    <dl className="grid gap-4 border-t bg-muted/15 p-4 text-xs sm:grid-cols-2">
                      <div>
                        <dt className="text-muted-foreground">{t('plugins.requiresLabel')}</dt>
                        <dd className="mt-1 break-words font-mono">{plugin.requires.join(' · ') || '—'}</dd>
                      </div>
                      <div>
                        <dt className="text-muted-foreground">{t('plugins.providesLabel')}</dt>
                        <dd className="mt-1 break-words font-mono">{plugin.provides.join(' · ') || '—'}</dd>
                      </div>
                      <div className="sm:col-span-2">
                        <dt className="text-muted-foreground">{t('plugins.rowsUsing')}</dt>
                        <dd className="mt-1">
                          {entries.map((entry) => entry.id).join(' · ') || t('plugins.notInSession')}
                        </dd>
                      </div>
                    </dl>
                  ) : null}
                </article>
              )
            })}
          </div>
        ) : (
          <div className="rounded-xl border border-dashed p-8 text-center text-sm text-muted-foreground">
            {normalizedQuery ? t('plugins.noSearchResults') : t('plugins.noCatalog')}
          </div>
        )}
      </SettingsTabPanel>

      <SettingsTabPanel id="extensions" active={activeTab}>
        <div className="mb-4 rounded-xl border bg-muted/20 px-4 py-3" data-plugin-scope="target">
          <strong className="text-sm">{t('plugins.targetScopeTitle')}</strong>
          <p className="mt-1 text-xs leading-relaxed text-muted-foreground">{t('plugins.targetScopeDescription')}</p>
        </div>
        <p className="mb-5 text-xs leading-relaxed text-muted-foreground">{t('plugins.extensionsDescription')}</p>
        {inventoryError && inventory ? (
          <p className="mb-4 text-sm text-destructive" role="alert">
            {inventoryError}
          </p>
        ) : null}
        {loadingInventory && !inventory ? (
          <div className="rounded-xl border border-dashed p-8 text-center text-sm text-muted-foreground">
            {common('loading')}
          </div>
        ) : inventoryError && !inventory ? (
          <div className="rounded-xl border border-destructive/40 p-6 text-center">
            <p className="text-sm text-destructive" role="alert">
              {inventoryError}
            </p>
            <Button className="mt-4" size="sm" variant="outline" onClick={() => void loadInventory()}>
              {common('retry')}
            </Button>
          </div>
        ) : (
          <div className="space-y-5">
            {inventory?.publishers.length ? (
              <section>
                <h3 className="mb-2 text-sm font-semibold">{t('plugins.trustedPublishers')}</h3>
                <div className="space-y-2">
                  {inventory.publishers.map((publisher) => (
                    <div
                      className="flex min-h-12 items-center gap-3 rounded-xl border bg-card px-4 py-2"
                      key={publisher.trust.key_id}
                    >
                      <div className="min-w-0 flex-1">
                        <code className="break-all text-xs">{publisher.trust.key_id}</code>
                        <p className="mt-1 break-words text-xs text-muted-foreground">
                          {publisher.trust.allowed_sources.join(' · ') || '—'}
                        </p>
                      </div>
                      {publisher.revoked ? (
                        <span className="text-xs text-destructive">{t('plugins.revoked')}</span>
                      ) : (
                        <Button
                          type="button"
                          variant="ghost"
                          size="icon-sm"
                          aria-label={t('plugins.revokePublisher', {
                            name: publisher.trust.key_id,
                          })}
                          onClick={() =>
                            setLifecycleTarget({
                              kind: 'publisher',
                              keyId: publisher.trust.key_id,
                              label: publisher.trust.key_id,
                            })
                          }
                        >
                          <Ban />
                        </Button>
                      )}
                    </div>
                  ))}
                </div>
              </section>
            ) : null}
            <section>
              <h3 className="mb-2 text-sm font-semibold">{t('plugins.installedExtensions')}</h3>
              <div className="space-y-2">
                {inventory?.extensions.length ? (
                  inventory.extensions.map((extension) => {
                    const packageKey = `${extension.manifest.package_id}@${extension.manifest.version}`
                    const rowId = `extension:${packageKey}`
                    const effectiveMount = extensionMount(
                      profile?.plugins,
                      extension.manifest.package_id,
                      extension.manifest.version,
                    )
                    const sessionOverride = extensionMount(
                      currentSession?.profile_plugins,
                      extension.manifest.package_id,
                      extension.manifest.version,
                    )
                    const presetMount = extensionMount(
                      currentSession?.preset_plugins,
                      extension.manifest.package_id,
                      extension.manifest.version,
                    )
                    const mounted = effectiveMount?.enabled === true
                    const settingsMount = sessionOverride ?? effectiveMount ?? {
                      id: rowId,
                      kind: 'ternilo.extension.package',
                      enabled: false,
                      config: {
                        package_id: extension.manifest.package_id,
                        version: extension.manifest.version,
                        settings: pluginConfigDefaults(extension.manifest.config_schema),
                      },
                    }
                    const extensionSettings = configRecord(configRecord(settingsMount.config).settings)
                    const hasSettings =
                      pluginConfigFields(extension.manifest.config_schema).length > 0 ||
                      Boolean(Object.keys(extensionSettings).length)
                    const canConfigureUnmounted = Boolean(
                      currentSession && extension.enabled && !extension.revoked && hasSettings,
                    )
                    return (
                      <article
                        className="min-w-0 rounded-xl border bg-card p-4"
                        key={packageKey}
                        data-extension-package={packageKey}
                      >
                        <div className="flex flex-wrap items-center gap-3">
                          <PackagePlus className="size-4 text-muted-foreground" />
                          <div className="min-w-0 flex-1">
                            <div className="text-sm font-medium">
                              {extension.manifest.package_id}{' '}
                              <span className="text-xs text-muted-foreground">{extension.manifest.version}</span>
                            </div>
                          </div>
                          {extension.revoked ? (
                            <span className="text-xs text-destructive">{t('plugins.revoked')}</span>
                          ) : null}
                          <label className="flex min-h-10 items-center gap-2 rounded-md px-2 text-xs text-muted-foreground">
                            <span>{t('plugins.sessionTool')}</span>
                            <Switch
                              data-extension-mount=""
                              disabled={
                                !currentSession ||
                                (!mounted && (!extension.enabled || extension.revoked)) ||
                                extensionMountBusy === packageKey
                              }
                              checked={mounted}
                              aria-label={t(mounted ? 'plugins.unmountExtension' : 'plugins.mountExtension', {
                                name: extension.manifest.package_id,
                              })}
                              onCheckedChange={(checked) =>
                                checked &&
                                hasMissingRequiredSettings(extension.manifest.config_schema, extensionSettings)
                                  ? setExtensionConfigureRequest((current) => ({
                                      key: packageKey,
                                      revision: (current?.revision ?? 0) + 1,
                                    }))
                                  : void setExtensionMounted(
                                      extension.manifest.package_id,
                                      extension.manifest.version,
                                      extension.manifest.config_schema,
                                      checked,
                                      effectiveMount,
                                      sessionOverride,
                                      Boolean(presetMount || (effectiveMount && !sessionOverride)),
                                    )
                              }
                            />
                          </label>
                          <Switch
                            data-extension-enabled=""
                            disabled={extension.revoked}
                            checked={extension.enabled && !extension.revoked}
                            aria-label={t(extension.enabled ? 'plugins.disableExtension' : 'plugins.enableExtension', {
                              name: extension.manifest.package_id,
                            })}
                            onCheckedChange={(checked) =>
                              void setExtensionState(extension.manifest.package_id, extension.manifest.version, checked)
                            }
                          />
                          {!extension.revoked ? (
                            <Button
                              type="button"
                              variant="ghost"
                              size="icon-sm"
                              aria-label={t('plugins.revokeExtension', {
                                name: extension.manifest.package_id,
                              })}
                              onClick={() =>
                                setLifecycleTarget({
                                  kind: 'extension-revoke',
                                  packageId: extension.manifest.package_id,
                                  version: extension.manifest.version,
                                  label: `${extension.manifest.package_id} ${extension.manifest.version}`,
                                })
                              }
                            >
                              <Ban />
                            </Button>
                          ) : null}
                          {!extension.revoked ? (
                            <Button
                              type="button"
                              variant="ghost"
                              size="icon-sm"
                              aria-label={t('plugins.uninstallExtension', {
                                name: extension.manifest.package_id,
                              })}
                              onClick={() =>
                                setLifecycleTarget({
                                  kind: 'uninstall',
                                  packageId: extension.manifest.package_id,
                                  version: extension.manifest.version,
                                  label: `${extension.manifest.package_id} ${extension.manifest.version}`,
                                })
                              }
                            >
                              <Trash2 />
                            </Button>
                          ) : null}
                        </div>
                        <dl className="mt-3 grid gap-3 border-t pt-3 text-xs sm:grid-cols-2">
                          <div>
                            <dt className="text-muted-foreground">{t('plugins.runtime')}</dt>
                            <dd className="mt-1 font-mono" data-extension-runtime="">
                              {extension.manifest.runtime.kind}
                            </dd>
                          </div>
                          <div>
                            <dt className="text-muted-foreground">{t('plugins.payloadDigest')}</dt>
                            <dd className="mt-1 break-all font-mono">{extension.manifest.payload_sha256}</dd>
                          </div>
                          <div className="sm:col-span-2">
                            <dt className="text-muted-foreground">{t('plugins.capabilities')}</dt>
                            <dd className="mt-1 break-words font-mono">
                              {extension.granted_capabilities.join(' · ') || t('plugins.noHostCapability')}
                            </dd>
                          </div>
                        </dl>
                        {extension.manifest.contributions.tools.length > 0 ? (
                          <div className="mt-3 border-t pt-3">
                            <h4 className="text-xs text-muted-foreground">{t('plugins.tools')}</h4>
                            <div className="mt-2 grid gap-2">
                              {extension.manifest.contributions.tools.map((tool) => (
                                <div
                                  className="rounded-md bg-muted/30 px-3 py-2 text-xs"
                                  key={`${tool.handler}:${tool.spec.name}`}
                                  data-extension-tool={tool.spec.name}
                                >
                                  <div>
                                    <code>{tool.spec.name}</code>{' '}
                                    <span className="text-muted-foreground">
                                      · {tool.effect} · {tool.handler}
                                    </span>
                                  </div>
                                  {tool.spec.description ? (
                                    <p className="mt-1 text-muted-foreground">{tool.spec.description}</p>
                                  ) : null}
                                  {tool.presentation ? (
                                    <p className="mt-1 text-muted-foreground" data-extension-presentation="">
                                      {t('plugins.presentation')}: {tool.presentation.title} ·{' '}
                                      {tool.presentation.icon_kind} · {tool.presentation.result.kind}
                                    </p>
                                  ) : null}
                                </div>
                              ))}
                            </div>
                          </div>
                        ) : null}
                        {extension.manifest.contributions.prompt_sections.length > 0 ? (
                          <div className="mt-3 border-t pt-3" data-extension-prompt-sections="">
                            <h4 className="text-xs text-muted-foreground">{t('plugins.promptSections')}</h4>
                            <div className="mt-2 grid gap-2">
                              {extension.manifest.contributions.prompt_sections.map((section) => (
                                <div
                                  className="min-w-0 rounded-md bg-muted/30 px-3 py-2 text-xs"
                                  key={section.id}
                                  data-extension-prompt-section={section.id}
                                >
                                  <div className="flex flex-wrap items-baseline gap-x-2 gap-y-1">
                                    <code className="break-all">{section.id}</code>
                                    <span className="text-muted-foreground">
                                      {t('plugins.promptOrder', { order: section.order })}
                                    </span>
                                  </div>
                                  <pre className="mt-2 max-h-48 overflow-auto whitespace-pre-wrap break-words rounded bg-background/60 p-2 font-mono leading-relaxed">
                                    <code>{section.content}</code>
                                  </pre>
                                </div>
                              ))}
                            </div>
                          </div>
                        ) : null}
                        {extension.manifest.contributions.skills.length > 0 ? (
                          <div className="mt-3 min-w-0 border-t pt-3" data-extension-skills="">
                            <h4 className="text-xs text-muted-foreground">{t('plugins.skills')}</h4>
                            <div className="mt-2 grid min-w-0 gap-2">
                              {extension.manifest.contributions.skills.map((skill) => (
                                <div
                                  className="min-w-0 rounded-md bg-muted/30 px-3 py-2 text-xs"
                                  key={skill.name}
                                  data-extension-skill={skill.name}
                                >
                                  <code className="break-all">{skill.name}</code>
                                  <p className="mt-1 break-words">{skill.description}</p>
                                  {skill.when_to_use ? (
                                    <p className="mt-1 break-words text-muted-foreground">
                                      {t('plugins.skillWhenToUse')}: {skill.when_to_use}
                                    </p>
                                  ) : null}
                                  <div className="mt-2 flex flex-wrap gap-x-3 gap-y-1 text-muted-foreground">
                                    <span>
                                      {t('plugins.skillModelInvocation')}:{' '}
                                      {t(
                                        (skill.invocation?.model_invocable ?? true)
                                          ? 'plugins.skillInvocable'
                                          : 'plugins.skillNotInvocable',
                                      )}
                                    </span>
                                    <span>
                                      {t('plugins.skillUserInvocation')}:{' '}
                                      {t(
                                        (skill.invocation?.user_invocable ?? true)
                                          ? 'plugins.skillInvocable'
                                          : 'plugins.skillNotInvocable',
                                      )}
                                    </span>
                                  </div>
                                  <pre
                                    className="mt-2 max-h-48 w-full min-w-0 max-w-full overflow-auto whitespace-pre-wrap break-all rounded bg-background/60 p-2 font-mono leading-relaxed"
                                    data-extension-skill-content=""
                                  >
                                    <code className="break-all">{skill.content}</code>
                                  </pre>
                                </div>
                              ))}
                            </div>
                          </div>
                        ) : null}
                        <ExtensionRuntimeContributions
                          manifest={extension.manifest}
                          onMaterialize={
                            extension.enabled && !extension.revoked
                              ? (template) => {
                                  setProviderMaterializeError('')
                                  setProviderMaterialize({
                                    packageId: extension.manifest.package_id,
                                    version: extension.manifest.version,
                                    template,
                                    providerId: template.id,
                                    apiKeyRef: template.credential.suggested_ref ?? '',
                                  })
                                }
                              : undefined
                          }
                        />
                        {mounted || canConfigureUnmounted ? (
                          <div className="mt-3 border-t pt-3" data-extension-settings={packageKey}>
                            {!mounted ? (
                              <p className="mb-3 text-xs text-muted-foreground">
                                {t('plugins.extensionSettingsMountFirst')}
                              </p>
                            ) : null}
                            <PluginConfigCard
                              entry={{
                                id: `${rowId}:settings`,
                                kind: 'ternilo.extension.package',
                                enabled: true,
                                config: extensionSettings,
                              }}
                              metadata={{
                                kind: 'ternilo.extension.package',
                                description:
                                  extension.manifest.description ?? t('plugins.extensionSettingsDescription'),
                                requires: [],
                                provides: [],
                                config_schema: extension.manifest.config_schema,
                              }}
                              overridden={Boolean(sessionOverride)}
                              title={t('plugins.extensionSettings', {
                                name: extension.manifest.package_id,
                              })}
                              showToggle={false}
                              openRequest={
                                extensionConfigureRequest?.key === packageKey
                                  ? extensionConfigureRequest.revision
                                  : undefined
                              }
                              onSave={(entry) =>
                                saveExtensionSettings(settingsMount, sessionOverride, entry.config, !mounted)
                              }
                            />
                          </div>
                        ) : hasSettings ? (
                          <p
                            className="mt-3 rounded-lg border border-dashed px-3 py-2 text-xs text-muted-foreground"
                            aria-disabled="true"
                            data-extension-settings-disabled={packageKey}
                          >
                            {t(
                              currentSession
                                ? 'plugins.extensionSettingsMountFirst'
                                : 'plugins.extensionSettingsNoSession',
                            )}
                          </p>
                        ) : null}
                      </article>
                    )
                  })
                ) : (
                  <div className="rounded-xl border border-dashed p-6 text-center text-sm text-muted-foreground">
                    {t('plugins.noExtensions')}
                  </div>
                )}
              </div>
            </section>
          </div>
        )}
        {!loadingInventory && !inventoryError ? (
          <details className="mt-5 rounded-xl border bg-muted/20 p-4">
            <summary className="cursor-pointer text-sm font-medium">{t('plugins.import')}</summary>
            <div className="mt-4 grid gap-4 sm:grid-cols-2">
              <Field>
                <Label htmlFor="plugin-publisher-file">{t('plugins.publisherTrust')}</Label>
                <Input
                  id="plugin-publisher-file"
                  type="file"
                  accept="application/json,.json"
                  onChange={(event) => {
                    void importJson('/extensions/publishers', event.target.files?.[0])
                    event.currentTarget.value = ''
                  }}
                />
                <FieldDescription>{t('plugins.publisherTrustDescription')}</FieldDescription>
              </Field>
              <Field>
                <Label htmlFor="plugin-bundle-file">{t('plugins.bundle')}</Label>
                <Input
                  id="plugin-bundle-file"
                  type="file"
                  accept="application/json,.json"
                  onChange={(event) => {
                    void reviewExtensionBundle(event.target.files?.[0])
                    event.currentTarget.value = ''
                  }}
                />
                <FieldDescription>{t('plugins.bundleDescription')}</FieldDescription>
              </Field>
            </div>
          </details>
        ) : null}
        {status ? (
          <p className="mt-4 text-xs text-destructive" role="alert">
            {status}
          </p>
        ) : null}
      </SettingsTabPanel>
      <ActionDialog
        open={lifecycleTarget !== null}
        title={t(
          lifecycleTarget?.kind === 'publisher'
            ? 'plugins.revokePublisherTitle'
            : lifecycleTarget?.kind === 'extension-revoke'
              ? 'plugins.revokeExtensionTitle'
              : 'plugins.uninstallExtensionTitle',
        )}
        description={
          lifecycleTarget
            ? t(
                lifecycleTarget.kind === 'publisher'
                  ? 'plugins.revokePublisherDescription'
                  : lifecycleTarget.kind === 'extension-revoke'
                    ? 'plugins.revokeExtensionDescription'
                    : 'plugins.uninstallExtensionDescription',
                { name: lifecycleTarget.label },
              )
            : undefined
        }
        cancelLabel={common('cancel')}
        confirmLabel={t(lifecycleTarget?.kind === 'uninstall' ? 'plugins.uninstall' : 'plugins.revoke')}
        busyLabel={t(lifecycleTarget?.kind === 'uninstall' ? 'plugins.uninstalling' : 'plugins.revoking')}
        busy={lifecycleBusy}
        destructive
        error={lifecycleError}
        onOpenChange={(next) => {
          if (!next) {
            setLifecycleTarget(null)
            setLifecycleError('')
          }
        }}
        onConfirm={() => void applyLifecycleAction()}
      />
      <Dialog
        open={providerMaterialize !== null}
        onOpenChange={(open) => {
          if (!open && !providerMaterializeBusy) {
            setProviderMaterialize(null)
            setProviderMaterializeError('')
          }
        }}
      >
        <DialogContent className="max-w-lg grid-rows-[auto_minmax(0,1fr)_auto]" data-extension-provider-dialog="">
          <DialogHeader>
            <DialogTitle>{t('plugins.addProviderTitle')}</DialogTitle>
            <DialogDescription>
              {providerMaterialize
                ? t('plugins.addProviderDescription', {
                    name: providerMaterialize.template.display_name,
                  })
                : undefined}
            </DialogDescription>
          </DialogHeader>
          {providerMaterialize ? (
            <form
              id="extension-provider-materialize-form"
              className="grid min-h-0 gap-4 overflow-y-auto px-1"
              onSubmit={(event) => {
                event.preventDefault()
                void materializeExtensionProvider()
              }}
            >
              <div className="rounded-lg border bg-muted/20 p-3 text-xs">
                <div className="font-medium">{providerMaterialize.template.display_name}</div>
                <code className="mt-1 block break-all text-muted-foreground">
                  {providerMaterialize.packageId}@{providerMaterialize.version} · {providerMaterialize.template.id}
                </code>
                <code className="mt-2 block break-all">{providerMaterialize.template.base_url}</code>
              </div>
              <Field>
                <Label htmlFor="extension-provider-id">{t('plugins.providerId')}</Label>
                <Input
                  id="extension-provider-id"
                  required
                  autoComplete="off"
                  value={providerMaterialize.providerId}
                  onChange={(event) =>
                    setProviderMaterialize((current) =>
                      current ? { ...current, providerId: event.target.value } : current,
                    )
                  }
                />
                <FieldDescription>{t('plugins.providerIdDescription')}</FieldDescription>
              </Field>
              <Field>
                <Label htmlFor="extension-provider-credential-ref">
                  {t('plugins.credentialReference')}
                </Label>
                <Input
                  id="extension-provider-credential-ref"
                  required={providerMaterialize.template.credential.required}
                  autoComplete="off"
                  value={providerMaterialize.apiKeyRef}
                  onChange={(event) =>
                    setProviderMaterialize((current) =>
                      current ? { ...current, apiKeyRef: event.target.value } : current,
                    )
                  }
                />
                <FieldDescription>{t('plugins.credentialReferenceDescription')}</FieldDescription>
              </Field>
              {providerMaterializeError ? (
                <p className="text-sm text-destructive" role="alert">
                  {providerMaterializeError}
                </p>
              ) : null}
            </form>
          ) : null}
          <DialogFooter>
            <Button
              type="button"
              variant="outline"
              disabled={providerMaterializeBusy}
              onClick={() => setProviderMaterialize(null)}
            >
              {common('cancel')}
            </Button>
            <Button
              type="submit"
              form="extension-provider-materialize-form"
              disabled={providerMaterializeBusy}
            >
              {providerMaterializeBusy ? t('plugins.creatingProvider') : t('plugins.createProvider')}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
      <Dialog
        open={extensionInstall !== null}
        onOpenChange={(open) => {
          if (!open && !installingExtension) setExtensionInstall(null)
        }}
      >
        <DialogContent className="min-w-0 max-w-xl grid-rows-[auto_minmax(0,1fr)_auto]" data-extension-install-review="">
          <DialogHeader>
            <DialogTitle>{t('plugins.installReviewTitle')}</DialogTitle>
            <DialogDescription>
              {extensionInstall
                ? t('plugins.installReviewDescription', {
                    name: extensionInstall.bundle.manifest.package_id,
                  })
                : undefined}
            </DialogDescription>
          </DialogHeader>
          {extensionInstall ? (
            <div className="grid min-h-0 min-w-0 gap-4 overflow-y-auto text-sm">
              <dl className="grid gap-3 rounded-lg border bg-muted/20 p-4 sm:grid-cols-2">
                <div>
                  <dt className="text-xs text-muted-foreground">{t('plugins.package')}</dt>
                  <dd className="mt-1 font-mono">
                    {extensionInstall.bundle.manifest.package_id} · {extensionInstall.bundle.manifest.version}
                  </dd>
                </div>
                <div>
                  <dt className="text-xs text-muted-foreground">{t('plugins.runtime')}</dt>
                  <dd className="mt-1 font-mono">{extensionInstall.bundle.manifest.runtime.kind}</dd>
                </div>
                <div>
                  <dt className="text-xs text-muted-foreground">{t('plugins.publisher')}</dt>
                  <dd className="mt-1 break-all font-mono">{extensionInstall.bundle.manifest.publisher_key_id}</dd>
                </div>
                <div>
                  <dt className="text-xs text-muted-foreground">{t('plugins.source')}</dt>
                  <dd className="mt-1 break-all font-mono">{extensionInstall.bundle.manifest.source}</dd>
                </div>
                <div className="sm:col-span-2">
                  <dt className="text-xs text-muted-foreground">{t('plugins.payloadDigest')}</dt>
                  <dd className="mt-1 break-all font-mono text-xs">
                    {extensionInstall.bundle.manifest.payload_sha256}
                  </dd>
                </div>
              </dl>
              {extensionInstall.bundle.manifest.contributions.tools.length > 0 ? (
                <section className="rounded-lg border p-4">
                  <h3 className="text-sm font-medium">{t('plugins.tools')}</h3>
                  <div className="mt-3 grid gap-2">
                    {extensionInstall.bundle.manifest.contributions.tools.map((tool) => (
                      <div className="rounded-md bg-muted/30 px-3 py-2 text-xs" key={`${tool.handler}:${tool.spec.name}`}>
                        <div>
                          <code>{tool.spec.name}</code>{' '}
                          <span className="text-muted-foreground">
                            · {tool.effect} · {tool.handler}
                          </span>
                        </div>
                        <p className="mt-1 text-muted-foreground">{tool.spec.description}</p>
                        {tool.presentation ? (
                          <p className="mt-1 text-muted-foreground" data-extension-presentation-review="">
                            {t('plugins.presentation')}: {tool.presentation.title} · {tool.presentation.icon_kind} ·{' '}
                            {tool.presentation.result.kind} ·{' '}
                            {t('plugins.presentationFields', {
                              count: tool.presentation.input_summary?.length ?? 0,
                            })}
                          </p>
                        ) : null}
                      </div>
                    ))}
                  </div>
                </section>
              ) : null}
              {extensionInstall.bundle.manifest.contributions.prompt_sections.length > 0 ? (
                <section className="rounded-lg border p-4" data-extension-prompt-sections-review="">
                  <h3 className="text-sm font-medium">{t('plugins.promptSections')}</h3>
                  <div className="mt-3 grid gap-2">
                    {extensionInstall.bundle.manifest.contributions.prompt_sections.map((section) => (
                      <div
                        className="min-w-0 rounded-md bg-muted/30 px-3 py-2 text-xs"
                        key={section.id}
                        data-extension-prompt-section-review={section.id}
                      >
                        <div className="flex flex-wrap items-baseline gap-x-2 gap-y-1">
                          <code className="break-all">{section.id}</code>
                          <span className="text-muted-foreground">
                            {t('plugins.promptOrder', { order: section.order })}
                          </span>
                        </div>
                        <pre className="mt-2 max-h-48 overflow-auto whitespace-pre-wrap break-words rounded bg-background/60 p-2 font-mono leading-relaxed">
                          <code>{section.content}</code>
                        </pre>
                      </div>
                    ))}
                  </div>
                </section>
              ) : null}
              {extensionInstall.bundle.manifest.contributions.skills.length > 0 ? (
                <section className="min-w-0 rounded-lg border p-4" data-extension-skills-review="">
                  <h3 className="text-sm font-medium">{t('plugins.skills')}</h3>
                  <div className="mt-3 grid min-w-0 gap-2">
                    {extensionInstall.bundle.manifest.contributions.skills.map((skill) => (
                      <div
                        className="min-w-0 rounded-md bg-muted/30 px-3 py-2 text-xs"
                        key={skill.name}
                        data-extension-skill-review={skill.name}
                      >
                        <code className="break-all">{skill.name}</code>
                        <p className="mt-1 break-words">{skill.description}</p>
                        {skill.when_to_use ? (
                          <p className="mt-1 break-words text-muted-foreground">
                            {t('plugins.skillWhenToUse')}: {skill.when_to_use}
                          </p>
                        ) : null}
                        <div className="mt-2 flex flex-wrap gap-x-3 gap-y-1 text-muted-foreground">
                          <span>
                            {t('plugins.skillModelInvocation')}:{' '}
                            {t(
                              (skill.invocation?.model_invocable ?? true)
                                ? 'plugins.skillInvocable'
                                : 'plugins.skillNotInvocable',
                            )}
                          </span>
                          <span>
                            {t('plugins.skillUserInvocation')}:{' '}
                            {t(
                              (skill.invocation?.user_invocable ?? true)
                                ? 'plugins.skillInvocable'
                                : 'plugins.skillNotInvocable',
                            )}
                          </span>
                        </div>
                        <pre
                          className="mt-2 max-h-48 w-full min-w-0 max-w-full overflow-auto whitespace-pre-wrap break-all rounded bg-background/60 p-2 font-mono leading-relaxed"
                          data-extension-skill-content=""
                        >
                          <code className="break-all">{skill.content}</code>
                        </pre>
                      </div>
                    ))}
                  </div>
                </section>
              ) : null}
              <ExtensionRuntimeContributions manifest={extensionInstall.bundle.manifest} review />
              {extensionInstall.bundle.payload.kind === 'utf8' ? (
                <details className="rounded-lg border p-4" data-extension-source-review="">
                  <summary className="cursor-pointer text-sm font-medium">{t('plugins.reviewSource')}</summary>
                  <pre className="mt-3 max-h-72 overflow-auto whitespace-pre-wrap break-words rounded-md bg-muted p-3 font-mono text-xs">
                    <code>{extensionInstall.bundle.payload.content}</code>
                  </pre>
                </details>
              ) : (
                <div className="rounded-lg border p-4 text-sm" data-extension-binary-summary="">
                  <strong>{t('plugins.binaryPayload')}</strong>
                  <p className="mt-1 text-xs text-muted-foreground">
                    {t('plugins.binaryPayloadDescription', {
                      count: Math.max(
                        0,
                        Math.floor((extensionInstall.bundle.payload.content.length * 3) / 4) -
                          (extensionInstall.bundle.payload.content.endsWith('==')
                            ? 2
                            : extensionInstall.bundle.payload.content.endsWith('=')
                              ? 1
                              : 0),
                      ),
                    })}
                  </p>
                </div>
              )}
              <fieldset className="rounded-lg border p-4">
                <legend className="px-1 text-sm font-medium">{t('plugins.capabilities')}</legend>
                <p className="mb-3 text-xs leading-relaxed text-muted-foreground">
                  {t('plugins.capabilitiesDescription')}
                </p>
                {extensionInstall.bundle.manifest.requested_capabilities.length ? (
                  <div className="grid gap-2">
                    {extensionInstall.bundle.manifest.requested_capabilities.map((capability) => (
                      <label
                        className="flex min-h-10 items-center gap-3 rounded-md px-2 hover:bg-muted"
                        key={capability}
                      >
                        <input
                          type="checkbox"
                          checked={extensionInstall.grants.has(capability)}
                          onChange={(event) =>
                            setExtensionInstall((current) => {
                              if (!current) return current
                              const grants = new Set(current.grants)
                              if (event.target.checked) grants.add(capability)
                              else grants.delete(capability)
                              return { ...current, grants }
                            })
                          }
                        />
                        <span>
                          <span className="block font-mono text-xs">{capability}</span>
                          <span className="text-xs text-muted-foreground">
                            {t(
                              capability === 'workspace_read'
                                ? 'plugins.capabilityWorkspaceRead'
                                : capability === 'log'
                                  ? 'plugins.capabilityLog'
                                  : 'plugins.capabilityDeclared',
                            )}
                          </span>
                        </span>
                      </label>
                    ))}
                  </div>
                ) : (
                  <p className="text-xs text-muted-foreground">{t('plugins.noHostCapability')}</p>
                )}
              </fieldset>
              {extensionInstallError ? (
                <p className="text-sm text-destructive" role="alert">
                  {extensionInstallError}
                </p>
              ) : null}
            </div>
          ) : null}
          <DialogFooter>
            <Button variant="outline" disabled={installingExtension} onClick={() => setExtensionInstall(null)}>
              {common('cancel')}
            </Button>
            <Button disabled={installingExtension} onClick={() => void installExtensionBundle()}>
              {installingExtension ? t('plugins.installing') : t('plugins.install')}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  )
}
