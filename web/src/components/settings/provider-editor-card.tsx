import * as React from 'react'
import { LoaderCircle, Save, X } from 'lucide-react'
import type { HostedWebTools, ProviderModel, ProviderModelDiscoveryRequest, ProviderProfile, ProviderProtocol } from '@/types'
import { HostedToolsEditor } from './provider-hosted-tools'
import { Button } from '@/components/ui/button'
import { Field, FieldDescription, Input, Label, Select } from '@/components/ui/field'
import {
  defaultSettingsDraft,
  modelDraft,
  ProviderModelEditor,
  ProviderSettingsEditor,
  settingsDraft,
  type ModelSettingsDraft,
  type ProviderModelDraft,
} from './provider-model-editor'
import { useTranslate } from '@/i18n/provider'

export interface ProviderDraft {
  id: string
  displayName: string
  baseUrl: string
  protocol: ProviderProtocol
  hostedTools?: HostedWebTools | null
  defaults: ModelSettingsDraft
  models: ProviderModelDraft[]
  timeoutMs: number
  maxAttempts: number
  retryBaseDelayMs: number
}

const defaultProtocolUrls: Partial<Record<ProviderProtocol, string>> = {
  'openai-responses': 'https://api.openai.com/v1',
  'openai-chat-completions': 'https://api.openai.com/v1',
  'google-gemini': 'https://generativelanguage.googleapis.com/v1beta',
  'anthropic-messages': 'https://api.anthropic.com/v1',
}

export function changeProviderProtocol(draft: ProviderDraft, protocol: ProviderProtocol): ProviderDraft {
  const previousDefault = defaultProtocolUrls[draft.protocol]
  const baseUrl = draft.baseUrl.trim().replace(/\/$/, '')
  return {
    ...draft,
    protocol,
    hostedTools: protocol === 'anthropic-messages' ? draft.hostedTools : null,
    baseUrl: (baseUrl === previousDefault || !baseUrl) && defaultProtocolUrls[protocol]
      ? defaultProtocolUrls[protocol]!
      : draft.baseUrl,
  }
}

function initialDraft(provider?: ProviderProfile): ProviderDraft {
  return provider ? {
    id: provider.id,
    displayName: provider.display_name,
    baseUrl: provider.base_url,
    protocol: provider.protocol,
    hostedTools: provider.hosted_tools,
    defaults: settingsDraft(provider.defaults),
    models: provider.models.map(modelDraft),
    timeoutMs: provider.timeout_ms,
    maxAttempts: provider.max_attempts,
    retryBaseDelayMs: provider.retry_base_delay_ms,
  } : {
    id: '',
    displayName: '',
    baseUrl: 'https://api.openai.com/v1',
    protocol: 'openai-responses',
    defaults: defaultSettingsDraft(),
    models: [{ id: '', displayName: '', upstream: {}, overrides: {} }],
    timeoutMs: 600_000,
    maxAttempts: 3,
    retryBaseDelayMs: 250,
  }
}

export function providerModelDiscoveryRequest(
  draft: ProviderDraft,
  apiKey: string,
  providerId?: string,
): ProviderModelDiscoveryRequest {
  return {
    ...(providerId ? { provider_id: providerId } : {}),
    base_url: draft.baseUrl.trim().replace(/\/$/, ''),
    protocol: draft.protocol,
    timeout_ms: draft.timeoutMs,
    api_key: apiKey.trim() || null,
  }
}

export function ProviderEditorCard({
  provider,
  credentialConfigured,
  onCancel,
  onSave,
  onDiscover,
}: {
  provider?: ProviderProfile
  credentialConfigured: boolean
  onCancel(): void
  onSave(draft: ProviderDraft, apiKey: string): Promise<void>
  onDiscover(request: ProviderModelDiscoveryRequest): Promise<ProviderModel[]>
}) {
  const fieldId = React.useId()
  const t = useTranslate('settings')
  const common = useTranslate('common')
  const [draft, setDraft] = React.useState(() => initialDraft(provider))
  const [apiKey, setApiKey] = React.useState('')
  const [busy, setBusy] = React.useState(false)
  const [status, setStatus] = React.useState('')
  const isNew = !provider

  const save = async () => {
    setBusy(true)
    setStatus('')
    try {
      await onSave(draft, apiKey)
      setApiKey('')
    } catch (cause) {
      setStatus(cause instanceof Error ? cause.message : String(cause))
    } finally {
      setBusy(false)
    }
  }

  const discover = () => onDiscover(providerModelDiscoveryRequest(draft, apiKey, provider?.id))

  return (
    <div className="rounded-xl border bg-muted/20 p-4 sm:p-5" data-provider-editor={provider?.id ?? 'new'}>
      <div className="mb-5 flex items-start justify-between gap-4">
        <div>
          <div className="text-sm font-semibold">{isNew ? t('provider.addTitle') : provider.display_name}</div>
          {!isNew && <code className="mt-1 block text-[10px] text-muted-foreground">{provider.id}</code>}
        </div>
        <Button type="button" size="icon-sm" variant="ghost" aria-label={t('provider.closeEditor')} onClick={onCancel}><X /></Button>
      </div>

      {isNew && <div className="mb-5 grid gap-4 sm:grid-cols-2">
        <Field><Label htmlFor={`${fieldId}-provider-id`}>{t('provider.id')}</Label><Input id={`${fieldId}-provider-id`} value={draft.id} placeholder="my-provider" onChange={event => setDraft(current => ({ ...current, id: event.target.value }))} /><FieldDescription>{t('provider.idDescription')}</FieldDescription></Field>
        <Field><Label htmlFor={`${fieldId}-provider-name`}>{t('provider.displayName')}</Label><Input id={`${fieldId}-provider-name`} value={draft.displayName} placeholder="My Provider" onChange={event => setDraft(current => ({ ...current, displayName: event.target.value }))} /></Field>
      </div>}

      <Field>
        <Label htmlFor={`${fieldId}-provider-key`}>{t('provider.apiKey')}</Label>
        <Input id={`${fieldId}-provider-key`} type="password" autoComplete="new-password" spellCheck={false} value={apiKey} onChange={event => setApiKey(event.target.value)} placeholder={credentialConfigured ? t('provider.keyConfiguredPlaceholder') : t('provider.keyEmptyPlaceholder')} />
        <FieldDescription>{credentialConfigured ? t('provider.keyConfiguredDescription') : t('provider.keyEmptyDescription')}</FieldDescription>
      </Field>

      <details className="mt-5 rounded-lg border bg-background/30 p-3" open={isNew}>
        <summary className="cursor-pointer text-sm font-medium">{t('provider.customSettings')}</summary>
        <div className="mt-5 grid gap-4">
          {!isNew && <Field><Label htmlFor={`${fieldId}-provider-name-${provider.id}`}>{t('provider.displayName')}</Label><Input id={`${fieldId}-provider-name-${provider.id}`} value={draft.displayName} onChange={event => setDraft(current => ({ ...current, displayName: event.target.value }))} /></Field>}
          <Field><Label htmlFor={`${fieldId}-provider-url`}>{t('provider.apiAddress')}</Label><Input id={`${fieldId}-provider-url`} value={draft.baseUrl} placeholder="https://api.example.com/v1" onChange={event => setDraft(current => ({ ...current, baseUrl: event.target.value }))} /></Field>
          <Field><Label htmlFor={`${fieldId}-provider-protocol`}>{t('provider.apiProtocol')}</Label><Select className="max-w-sm" id={`${fieldId}-provider-protocol`} value={draft.protocol} onValueChange={nextValue => setDraft(current => changeProviderProtocol(current, nextValue as ProviderProtocol))}><option value="openai-responses">OpenAI Responses</option><option value="deepseek-responses">DeepSeek Responses</option><option value="openai-chat-completions">OpenAI Chat Completions</option><option value="google-gemini">Google Gemini</option><option value="anthropic-messages">Claude · Anthropic Messages</option></Select><FieldDescription>{t(draft.protocol === 'google-gemini' ? 'provider.geminiProtocolDescription' : draft.protocol === 'anthropic-messages' ? 'provider.anthropicProtocolDescription' : draft.protocol === 'deepseek-responses' ? 'provider.deepseekProtocolDescription' : 'provider.protocolDescription')}</FieldDescription></Field>
        </div>

        <section className="mt-5 border-t pt-5" aria-label={t('provider.defaults')}>
          <div className="mb-4"><div className="text-sm font-medium">{t('provider.defaults')}</div><p className="mt-1 text-xs text-muted-foreground">{t('provider.defaultsDescription')}</p></div>
          <ProviderSettingsEditor idPrefix={`${fieldId}-provider-defaults`} name={t('provider.defaults')} value={draft.defaults} onChange={defaults => setDraft(current => ({ ...current, defaults }))} />
        </section>

        <ProviderModelEditor defaults={draft.defaults} models={draft.models} onChange={models => setDraft(current => ({ ...current, models }))} onDiscover={discover} />
        {draft.protocol === 'anthropic-messages' && <HostedToolsEditor value={draft.hostedTools} onChange={hostedTools => setDraft(current => ({ ...current, hostedTools }))} />}

        <details className="mt-5 border-t pt-4"><summary className="cursor-pointer text-xs font-medium text-muted-foreground">{t('provider.requestRetry')}</summary><p className="mt-3 text-xs leading-relaxed text-muted-foreground">{t('provider.timeoutHint')}</p><div className="mt-4 grid gap-4 sm:grid-cols-3"><Field><Label htmlFor={`${fieldId}-provider-timeout`}>{t('provider.timeout')}</Label><Input id={`${fieldId}-provider-timeout`} type="number" min={0} value={draft.timeoutMs} onChange={event => setDraft(current => ({ ...current, timeoutMs: Number(event.target.value) }))} /></Field><><Field><Label htmlFor={`${fieldId}-provider-attempts`}>{t('provider.attempts')}</Label><Input id={`${fieldId}-provider-attempts`} type="number" min={1} max={8} value={draft.maxAttempts} onChange={event => setDraft(current => ({ ...current, maxAttempts: Number(event.target.value) }))} /></Field><Field><Label htmlFor={`${fieldId}-provider-retry-delay`}>{t('provider.retryDelay')}</Label><Input id={`${fieldId}-provider-retry-delay`} type="number" min={1} value={draft.retryBaseDelayMs} onChange={event => setDraft(current => ({ ...current, retryBaseDelayMs: Number(event.target.value) }))} /></Field></></div></details>
      </details>

      {status && <p role="alert" className="mt-4 text-xs text-destructive">{status}</p>}
      <div className="mt-5 flex flex-wrap justify-end gap-2">
        <Button type="button" variant="ghost" disabled={busy} onClick={onCancel}>{common('cancel')}</Button>
        <Button type="button" disabled={busy} onClick={() => void save()}>{busy ? <LoaderCircle className="animate-spin" /> : <Save />}{isNew ? t('models.addProvider') : common('save')}</Button>
      </div>
    </div>
  )
}
