import * as React from 'react'
import { Plus } from 'lucide-react'
import { api } from '@/api/client'
import { Button } from '@/components/ui/button'
import { Field, Input, Label, Select } from '@/components/ui/field'
import { ActionDialog } from '@/components/settings/settings-ui'
import { useTranslate } from '@/i18n/provider'
import { modelAdminPath, modelResource, type ModelProvider, type ModelPublication } from './model-service-api'
import { ModelDirectory, ModelModal, ModelPickerList, errorMessage, useModelPage } from './model-service-ui'
import css from './model-service.module.css'
import { ProviderEditor } from './model-providers'

export function ModelPublications({ editable = true }: { editable?: boolean }) {
  const t = useTranslate('modelService')
  const directory = useModelPage<ModelPublication>(`${modelAdminPath}/publications`, 'models')
  const [editor, setEditor] = React.useState<ModelPublication | 'new' | null>(null)
  const [target, setTarget] = React.useState<ModelPublication | null>(null)
  const [busy, setBusy] = React.useState(false)
  const [error, setError] = React.useState('')
  const disable = async () => {
    if (!target || busy) return
    setBusy(true); setError('')
    try { await api.request<void>(modelResource(`${modelAdminPath}/publications`, target.model_id), { method: 'DELETE' }); setTarget(null); directory.reload() }
    catch (cause) { setError(errorMessage(cause)) }
    finally { setBusy(false) }
  }
  return <>
    <ModelDirectory state={directory} label={t('modelsTab')} empty={t('emptyPublications')} actions={editable ? <Button onClick={() => setEditor('new')}><Plus />{t('publish')}</Button> : null}>
      <div className={css.list}>{directory.items.map(model => <article key={model.model_id} className={css.row} data-published-model={model.model_id}>
        <div className={css.identity}><strong>{model.display_name}</strong><span className={css.badge}>{t(!model.enabled ? 'disabled' : model.provider_enabled ? 'available' : 'providerUnavailable')}</span><code>{model.model_id}</code><p>{model.provider_id} / {model.upstream_model}</p><p>{t('modelFacts', { context: model.defaults.context_window.toLocaleString(), output: model.defaults.max_output_tokens.toLocaleString() })}</p></div>
        {editable && <div className={css.rowActions}><Button variant="outline" onClick={() => setEditor(model)}>{t('edit')}</Button><Button variant="ghost" disabled={!model.enabled} onClick={() => { setTarget(model); setError('') }}>{t('disable')}</Button></div>}
      </article>)}</div>
    </ModelDirectory>
    {editable && editor && <PublicationEditor key={editor === 'new' ? 'new' : editor.model_id} initial={editor === 'new' ? null : editor} onClose={() => setEditor(null)} onSaved={() => { setEditor(null); directory.reload() }} />}
    <ActionDialog open={target !== null} title={t('disableTitle')} description={t('disableDescription', { name: target?.display_name ?? '' })} cancelLabel={t('cancel')} confirmLabel={t('disable')} busy={busy} error={error} destructive onOpenChange={open => { if (!open) setTarget(null) }} onConfirm={() => void disable()} />
  </>
}

export function PublicationEditor({ initial, onClose, onSaved }: { initial: ModelPublication | null; onClose(): void; onSaved(model: ModelPublication): void }) {
  const t = useTranslate('modelService')
  const [modelId, setModelId] = React.useState(initial?.model_id ?? '')
  const [name, setName] = React.useState(initial?.display_name ?? '')
  const [provider, setProvider] = React.useState<ModelProvider | null>(null)
  const [providerId, setProviderId] = React.useState(initial?.provider_id ?? '')
  const [upstreamModel, setUpstreamModel] = React.useState(initial?.upstream_model ?? '')
  const [enabled, setEnabled] = React.useState(initial?.enabled ?? true)
  const [busy, setBusy] = React.useState(false)
  const [error, setError] = React.useState('')
  const [addingProvider, setAddingProvider] = React.useState(false)
  const [providerRevision, refreshProviders] = React.useReducer(value => value + 1, 0)
  React.useEffect(() => {
    if (!initial) return
    const controller = new AbortController()
    void api.request<ModelProvider>(modelResource(`${modelAdminPath}/providers`, initial.provider_id), { signal: controller.signal })
      .then(value => { if (!controller.signal.aborted) setProvider(value) }).catch(cause => { if (!controller.signal.aborted) setError(errorMessage(cause)) })
    return () => controller.abort()
  }, [initial])
  const save = async () => {
    if (busy) return
    setBusy(true); setError('')
    try {
      const model = await api.request<ModelPublication>(initial ? modelResource(`${modelAdminPath}/publications`, initial.model_id) : `${modelAdminPath}/publications`, { method: initial ? 'PUT' : 'POST', body: { model_id: modelId.trim(), display_name: name.trim(), provider_id: providerId, upstream_model: upstreamModel, enabled } })
      onSaved(model)
    } catch (cause) { setError(errorMessage(cause)) }
    finally { setBusy(false) }
  }
  return <ModelModal title={t(initial ? 'publicationEdit' : 'publish')} description={t('publicationDescription')} busy={busy} onClose={onClose}>
    <form className={css.form} onSubmit={event => { event.preventDefault(); void save() }}>
      <div className={css.fields}><Field><Label htmlFor="publication-id">{t('modelId')}</Label><Input id="publication-id" value={modelId} disabled={busy || Boolean(initial)} required onChange={event => setModelId(event.target.value)} /></Field><Field><Label htmlFor="publication-name">{t('displayName')}</Label><Input id="publication-name" value={name} disabled={busy} required maxLength={120} onChange={event => setName(event.target.value)} /></Field></div>
      <div className={css.inlineAction}><p className={css.hint}>{t('providerPrerequisite')}</p><Button type="button" variant="outline" disabled={busy} onClick={() => setAddingProvider(true)}><Plus />{t('providerAdd')}</Button></div>
      <ModelPickerList<ModelProvider> key={providerRevision} path={`${modelAdminPath}/providers`} field="providers" label={t('chooseProvider')} selected={[providerId]} disabled={busy} id={value => value.profile.id} name={value => <>{value.profile.display_name}<small>{value.profile.id}</small></>} onPick={value => { setProvider(value); setProviderId(value.profile.id); setUpstreamModel(value.profile.models[0]?.id ?? '') }} />
      {provider && <Field><Label htmlFor="publication-upstream-model">{t('upstreamModel')}</Label><Select id="publication-upstream-model" value={upstreamModel} disabled={busy} required onValueChange={nextValue => setUpstreamModel(nextValue)}><option value="">{t('chooseModel')}</option>{provider.profile.models.map(model => <option key={model.id} value={model.id}>{model.display_name || model.id}</option>)}</Select><p className={css.hint}>{provider.profile.display_name}</p></Field>}
      <label className={css.check}><input type="checkbox" checked={enabled} disabled={busy} onChange={event => setEnabled(event.target.checked)} />{t('enabled')}</label>
      {error && <p role="alert" className="text-sm text-destructive">{error}</p>}
      <div className={css.formFooter}><Button type="button" variant="outline" disabled={busy} onClick={onClose}>{t('cancel')}</Button><Button type="submit" disabled={busy || !providerId || !upstreamModel}>{t(busy ? 'saving' : 'save')}</Button></div>
    </form>
    {addingProvider && <ProviderEditor initial={null} onClose={() => setAddingProvider(false)} onSaved={value => { setProvider(value); setProviderId(value.profile.id); setUpstreamModel(value.profile.models[0]?.id ?? ''); refreshProviders(); setAddingProvider(false) }} />}
  </ModelModal>
}
