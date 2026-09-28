import * as React from 'react'
import { Plus } from 'lucide-react'
import { api } from '@/api/client'
import { Button } from '@/components/ui/button'
import { ProviderEditorCard, type ProviderDraft } from '@/components/settings/provider-editor-card'
import { validateProviderDraft } from '@/components/settings/models-settings'
import { ActionDialog } from '@/components/settings/settings-ui'
import { useTranslate } from '@/i18n/provider'
import type { ProviderModel, ProviderProfile } from '@/types'
import { modelAdminPath, modelResource, type ModelProvider } from './model-service-api'
import { ModelDirectory, ModelModal, errorMessage, useModelPage, useModelProtocol } from './model-service-ui'
import css from './model-service.module.css'

export function ModelProviders({ editable = true }: { editable?: boolean }) {
  const t = useTranslate('modelService')
  const protocol = useModelProtocol()
  const directory = useModelPage<ModelProvider>(`${modelAdminPath}/providers`, 'providers')
  const [editor, setEditor] = React.useState<ModelProvider | 'new' | null>(null)
  const [target, setTarget] = React.useState<ModelProvider | null>(null)
  const [busy, setBusy] = React.useState(false)
  const [error, setError] = React.useState('')
  const disable = async () => {
    if (!target || busy) return
    setBusy(true); setError('')
    try { await api.request<void>(modelResource(`${modelAdminPath}/providers`, target.profile.id), { method: 'DELETE' }); setTarget(null); directory.reload() }
    catch (cause) { setError(errorMessage(cause)) }
    finally { setBusy(false) }
  }
  return <div className={css.page}>
    <p className={css.hint}>{t('providerDescription')}</p>
    <ModelDirectory state={directory} label={t('upstream')} empty={t('emptyProviders')} actions={editable ? <Button onClick={() => setEditor('new')}><Plus />{t('providerAdd')}</Button> : null}>
      <div className={css.list}>{directory.items.map(provider => <article className={css.row} key={provider.profile.id} data-model-provider={provider.profile.id}>
        <div className={css.identity}><strong>{provider.profile.display_name}</strong><span className={css.badge}>{t(provider.enabled ? 'enabled' : 'disabled')}</span><code>{provider.profile.base_url}</code><p>{protocol(provider.profile.protocol)} · {t(provider.has_api_key ? 'keySaved' : 'keyNotSet')}</p></div>
        {editable && <div className={css.rowActions}><Button variant="outline" onClick={() => setEditor(provider)}>{t('edit')}</Button><Button variant="ghost" disabled={!provider.enabled} onClick={() => { setTarget(provider); setError('') }}>{t('disable')}</Button></div>}
      </article>)}</div>
    </ModelDirectory>
    {editable && editor && <ProviderEditor key={editor === 'new' ? 'new' : editor.profile.id} initial={editor === 'new' ? null : editor} onClose={() => setEditor(null)} onSaved={() => { setEditor(null); directory.reload() }} />}
    <ActionDialog open={target !== null} title={t('disableTitle')} description={t('disableDescription', { name: target?.profile.display_name ?? '' })} cancelLabel={t('cancel')} confirmLabel={t('disable')} busy={busy} error={error} destructive onOpenChange={open => { if (!open) setTarget(null) }} onConfirm={() => void disable()} />
  </div>
}

export function ProviderEditor({ initial, onClose, onSaved }: { initial: ModelProvider | null; onClose(): void; onSaved(provider: ModelProvider): void }) {
  const t = useTranslate('modelService')
  const settings = useTranslate('settings')
  const [enabled, setEnabled] = React.useState(initial?.enabled ?? true)
  const [clearKey, setClearKey] = React.useState(false)
  const [busy, setBusy] = React.useState(false)
  const save = async (draft: ProviderDraft, apiKey: string) => {
    setBusy(true)
    try {
      const validated = validateProviderDraft(draft, {
        invalidId: settings('provider.errorId'), missingName: settings('provider.errorName'), invalidUrl: settings('provider.errorUrl'),
        missingModels: settings('provider.errorModels'), duplicateModels: settings('provider.errorDuplicateModels'), invalidCloudOutput: settings('provider.errorCloudOutput'), invalidRetry: settings('provider.errorRetry'),
        model: { invalidCapacity: value => settings('provider.errorInvalidCapacity', { value }), positiveCapacity: value => settings('provider.errorPositiveCapacity', { value }), missingId: index => settings('provider.errorMissingModelId', { index }), missingEffort: id => settings('provider.errorMissingEffort', { id }), defaultNotEnabled: id => settings('provider.errorDefaultEffort', { id }) },
      })
      const profile: ProviderProfile = { id: validated.id, display_name: validated.displayName, base_url: draft.baseUrl.trim().replace(/\/$/, ''), protocol: draft.protocol,
        api_key_ref: null, defaults: validated.defaults, models: validated.models, timeout_ms: draft.timeoutMs, max_attempts: draft.maxAttempts, retry_base_delay_ms: draft.retryBaseDelayMs }
      const provider = await api.request<ModelProvider>(initial ? modelResource(`${modelAdminPath}/providers`, initial.profile.id) : `${modelAdminPath}/providers`, {
        method: initial ? 'PUT' : 'POST', body: { profile, enabled, ...(apiKey.trim() ? { api_key: apiKey.trim() } : {}), clear_api_key: clearKey && !apiKey.trim() },
      })
      onSaved(provider)
    } finally { setBusy(false) }
  }
  return <ModelModal title={t(initial ? 'providerEdit' : 'providerAdd')} description={t('providerDescription')} busy={busy} onClose={onClose}>
    <label className={css.check}><input type="checkbox" checked={enabled} disabled={busy} onChange={event => setEnabled(event.target.checked)} />{t('enabled')}</label>
    {initial?.has_api_key && <div><label className={css.check}><input type="checkbox" checked={clearKey} disabled={busy} onChange={event => setClearKey(event.target.checked)} />{t('clearKey')}</label><p className={css.hint}>{t('clearKeyDescription')}</p></div>}
    <p className={css.notice}>{t('managedRetryNote')}</p>
    <ProviderEditorCard provider={initial?.profile} credentialConfigured={initial?.has_api_key ?? false} onCancel={() => { if (!busy) onClose() }} onSave={save}
      onDiscover={request => api.request<ProviderModel[]>(`${modelAdminPath}/providers/discover`, { method: 'POST', body: request })} />
  </ModelModal>
}
