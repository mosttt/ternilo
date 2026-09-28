import * as React from 'react'
import { KeyRound, Plus } from 'lucide-react'
import { navigate } from '@/app/navigation'
import { Button } from '@/components/ui/button'
import { Field, Input, Label } from '@/components/ui/field'
import { ActionDialog } from '@/components/settings/settings-ui'
import { useTranslate } from '@/i18n/provider'
import { createModelKey, modelAccessPath, revokeModelKey, type ModelEntitlement, type ModelKey } from './model-service-api'
import { CopyValue, ModelDirectory, ModelModal, ModelPickerList, QuotaFacts, errorMessage, futureTimestamp, localDateInput, useModelDate, useModelPage, useModelProtocol } from './model-service-ui'
import css from './model-service.module.css'

export function AvailableModels() {
  const t = useTranslate('modelService')
  const protocol = useModelProtocol()
  const directory = useModelPage<ModelEntitlement>(`${modelAccessPath}/catalog`, 'entitlements')
  const [grant, setGrant] = React.useState<ModelEntitlement | null>(null)
  return <>
    <ModelDirectory state={directory} label={t('source')} empty={t('noEntitlements')}>
      <div className={css.list}>{directory.items.map(entitlement => <article className={css.row} key={entitlement.grant.grant_id} data-model-entitlement={entitlement.grant.grant_id}>
        <div className={css.identity}><strong>{entitlement.grant.name}</strong><QuotaFacts quota={entitlement.grant.quota} /><p>{t(entitlement.grant.subject.kind === 'group' ? 'groupBudget' : 'userBudget')}</p>
          <div className="mt-4 grid gap-3">{entitlement.models.map(model => <div key={model.model_id}><strong>{model.display_name}</strong><code>{model.model_id}</code><p>{protocol(model.protocol)}</p><p>{t('modelFacts', { context: model.defaults.context_window.toLocaleString(), output: model.defaults.max_output_tokens.toLocaleString() })}</p></div>)}</div>
        </div>
        <Button variant="outline" disabled={!entitlement.models.length} onClick={() => setGrant(entitlement)}><KeyRound />{t('keyCreate')}</Button>
      </article>)}</div>
    </ModelDirectory>
    {grant && <KeyEditor initial={grant} onClose={() => setGrant(null)} onCreated={directory.reload} />}
  </>
}

export function ModelKeys() {
  const t = useTranslate('modelService')
  const date = useModelDate()
  const directory = useModelPage<ModelKey>(`${modelAccessPath}/keys`, 'keys')
  const [creating, setCreating] = React.useState(false)
  const [target, setTarget] = React.useState<ModelKey | null>(null)
  const [busy, setBusy] = React.useState(false)
  const [error, setError] = React.useState('')
  const revoke = async () => {
    if (!target || busy) return
    setBusy(true); setError('')
    try { await revokeModelKey(target.key_id); setTarget(null); directory.reload() }
    catch (cause) { setError(errorMessage(cause)) }
    finally { setBusy(false) }
  }
  return <div className={css.page} data-model-keys="">
    <div className={css.accessIntro}><p className={css.hint}>{t('keyDescription')}</p><p className={css.hint}>{t('keyDirectoryHint')}</p><p className={css.notice}>{t('keyAccessNotice')}</p></div>
    <ModelDirectory state={directory} label={t('keysTab')} empty={t('keyEmpty')} actions={<Button onClick={() => setCreating(true)}><Plus />{t('keyCreate')}</Button>}>
      <div className={css.list}>{directory.items.map(key => <article className={css.row} key={key.key_id} data-model-key={key.key_id}>
        <div className={css.identity}><strong>{key.name}</strong><span className={css.badge}>{t(key.revoked_at_ms !== null ? 'revoked' : key.expires_at_ms !== null && key.expires_at_ms <= Date.now() ? 'expired' : 'keyUnrevoked')}</span><code>{key.token_prefix}…</code><p>{t('source')}: {key.grant_name}</p><p>{t('keyScope')}: {key.model_ids.join(' · ')}</p><p>{t('monthlyTokens')}: {key.monthly_tokens ?? t('inheritLimit')} · {t('concurrentRequests')}: {key.max_concurrent_requests ?? t('inheritLimit')}</p><p>{key.expires_at_ms === null ? t('noExpiry') : t('expires', { date: date(key.expires_at_ms) })}</p><p>{key.last_used_at_ms === null ? t('neverUsed') : t('keyLastUsed', { date: date(key.last_used_at_ms) })}</p></div>
        <Button variant="outline" disabled={key.revoked_at_ms !== null} onClick={() => { setTarget(key); setError('') }}>{t('revoke')}</Button>
      </article>)}</div>
    </ModelDirectory>
    {creating && <KeyEditor initial={null} onClose={() => setCreating(false)} onCreated={directory.reload} />}
    <ActionDialog open={target !== null} title={t('keyRevokeTitle')} description={t('keyRevokeDescription', { name: target?.name ?? '' })} cancelLabel={t('cancel')} confirmLabel={t('revoke')} busy={busy} error={error} destructive onOpenChange={open => { if (!open) setTarget(null) }} onConfirm={() => void revoke()} />
  </div>
}

function suggestedExpiry(entitlement: ModelEntitlement | null) {
  return localDateInput(Math.min(Date.now() + 30 * 24 * 60 * 60 * 1_000, entitlement?.grant.expires_at_ms ?? Number.POSITIVE_INFINITY))
}

export function KeyEditor({ initial, onClose, onCreated }: { initial: ModelEntitlement | null; onClose(): void; onCreated(): void }) {
  const t = useTranslate('modelService')
  const protocol = useModelProtocol()
  const [name, setName] = React.useState('')
  const [entitlement, setEntitlement] = React.useState(initial)
  const [models, setModels] = React.useState<string[]>(initial?.models.map(model => model.model_id) ?? [])
  const [monthlyTokens, setMonthlyTokens] = React.useState('')
  const [concurrent, setConcurrent] = React.useState('')
  const [expiry, setExpiry] = React.useState(() => suggestedExpiry(initial))
  const [busy, setBusy] = React.useState(false)
  const [error, setError] = React.useState('')
  // The plaintext key exists only while this dialog is mounted.
  const [created, setCreated] = React.useState<{ key: ModelKey; token: string } | null>(null)
  const save = async () => {
    if (busy || !entitlement) return
    setBusy(true); setError('')
    try {
      if (!models.length) throw new Error(t('modelsRequired'))
      const result = await createModelKey({ name: name.trim(), grant_id: entitlement.grant.grant_id, model_ids: models, monthly_tokens: monthlyTokens ? Number(monthlyTokens) : null, max_concurrent_requests: concurrent ? Number(concurrent) : null, expires_at_ms: futureTimestamp(expiry, t('invalidExpiry')) })
      setCreated(result); onCreated()
    } catch (cause) { setError(errorMessage(cause)) }
    finally { setBusy(false) }
  }
  return <ModelModal title={t(created ? 'keyCreated' : 'keyCreate')} description={t(created ? 'keyOnce' : 'keyDescription')} busy={busy} onClose={onClose}>
    {created ? <div className={css.page} data-model-key-created="">
      <CopyValue label={t('platformAddress')} value={new URL('/v1', window.location.origin).toString()} />
      <CopyValue label={t('apiKey')} value={created.token} secret />
      <div className={css.page} data-model-key-models="">{entitlement?.models.filter(model => created.key.model_ids.includes(model.model_id)).map(model => <CopyValue key={model.model_id} label={`${model.display_name} · ${protocol(model.protocol)}`} value={model.model_id} />)}</div>
      <p className={css.hint}>{t('mixedProtocols')}</p>
      <p className={css.notice}>{t('keyInstructions')}</p>
      <p className={css.hint}>{t('keyManagementHint')}</p>
      <div className={css.formFooter}><Button variant="outline" onClick={onClose}>{t('close')}</Button><Button onClick={() => { onClose(); navigate('/models?tab=access&access=keys') }}>{t('keyManage')}</Button></div>
    </div> : <form className={css.form} onSubmit={event => { event.preventDefault(); void save() }}>
      <Field><Label htmlFor="model-key-name">{t('keyName')}</Label><Input id="model-key-name" value={name} placeholder={t('keyNamePlaceholder')} required maxLength={120} disabled={busy} onChange={event => setName(event.target.value)} /></Field>
      {!initial && <ModelPickerList<ModelEntitlement> path={`${modelAccessPath}/catalog`} field="entitlements" label={t('keyGrant')} selected={entitlement ? [entitlement.grant.grant_id] : []} disabled={busy} id={item => item.grant.grant_id} name={item => <>{item.grant.name}<small>{item.models.map(model => model.model_id).join(' · ')}</small></>} onPick={item => { setEntitlement(item); setModels(item.models.map(model => model.model_id)); setExpiry(suggestedExpiry(item)) }} />}
      {entitlement && <>
        <div className={css.notice}><strong>{entitlement.grant.name}</strong><QuotaFacts quota={entitlement.grant.quota} /><p>{t(entitlement.grant.subject.kind === 'group' ? 'groupBudget' : 'userBudget')}</p></div>
        <fieldset className={css.picker}><legend className="px-2 text-sm font-medium">{t('selectedModels')}</legend><div className={css.pickerRows}>{entitlement.models.map(model => <label className={css.check} key={model.model_id}><input type="checkbox" checked={models.includes(model.model_id)} disabled={busy} onChange={event => setModels(current => event.target.checked ? [...current, model.model_id] : current.filter(id => id !== model.model_id))} /><span>{model.display_name}<code className="ml-2 text-xs text-muted-foreground">{model.model_id}</code><small className="block text-xs text-muted-foreground">{protocol(model.protocol)}</small></span></label>)}</div></fieldset>
      </>}
      <div className={css.fields}><Field><Label htmlFor="model-key-tokens">{t('keyMonthlyTokens')}</Label><Input id="model-key-tokens" type="number" min={1} max={entitlement?.grant.quota.limit_tokens} step={1} value={monthlyTokens} disabled={busy} onChange={event => setMonthlyTokens(event.target.value)} /></Field><Field><Label htmlFor="model-key-concurrency">{t('keyConcurrentRequests')}</Label><Input id="model-key-concurrency" type="number" min={1} max={entitlement?.grant.quota.max_concurrent_requests} step={1} value={concurrent} disabled={busy} onChange={event => setConcurrent(event.target.value)} /></Field></div>
      <p className={css.hint}>{t('keyLimitDescription')}</p>
      <Field><Label htmlFor="model-key-expiry">{t('expiry')}</Label><Input id="model-key-expiry" type="datetime-local" value={expiry} disabled={busy} onChange={event => setExpiry(event.target.value)} /></Field>
      {error && <p role="alert" className="text-sm text-destructive">{error}</p>}
      <div className={css.formFooter}><Button type="button" variant="outline" disabled={busy} onClick={onClose}>{t('cancel')}</Button><Button type="submit" disabled={busy || !entitlement || !models.length}>{t(busy ? 'saving' : 'keyCreate')}</Button></div>
    </form>}
  </ModelModal>
}
