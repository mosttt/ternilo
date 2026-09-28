import * as React from 'react'
import { Plus } from 'lucide-react'
import { api } from '@/api/client'
import { Button } from '@/components/ui/button'
import { Field, Input, Label, Select } from '@/components/ui/field'
import { ActionDialog } from '@/components/settings/settings-ui'
import { useTranslate } from '@/i18n/provider'
import { modelAdminPath, modelResource, type ModelGrant, type ModelGroup, type ModelPublication, type ModelUser } from './model-service-api'
import { ModelDirectory, ModelModal, ModelPickerList, QuotaFacts, SelectedModels, errorMessage, futureTimestamp, localDateInput, useModelDate, useModelPage } from './model-service-ui'
import { GroupEditor, ModelGroups } from './model-groups'
import { PublicationEditor } from './model-publications'
import css from './model-service.module.css'

export function ModelGrants({ editable = true }: { editable?: boolean }) {
  const t = useTranslate('modelService')
  const date = useModelDate()
  const directory = useModelPage<ModelGrant>(`${modelAdminPath}/grants`, 'grants')
  const [editor, setEditor] = React.useState<ModelGrant | 'new' | null>(null)
  const [target, setTarget] = React.useState<ModelGrant | null>(null)
  const [busy, setBusy] = React.useState(false)
  const [error, setError] = React.useState('')
  const revoke = async () => {
    if (!target || busy) return
    setBusy(true); setError('')
    try { await api.request<void>(modelResource(`${modelAdminPath}/grants`, target.grant_id), { method: 'DELETE' }); setTarget(null); directory.reload() }
    catch (cause) { setError(errorMessage(cause)) }
    finally { setBusy(false) }
  }
  return <div className={css.page}>
    <div><h3 className="text-base font-semibold">{t('grants')}</h3><p className={css.hint}>{t('grantDescription')}</p></div>
    <ModelDirectory state={directory} label={t('grants')} actions={editable ? <Button onClick={() => setEditor('new')}><Plus />{t('grantAdd')}</Button> : null}>
      <div className={css.list}>{directory.items.map(grant => <article className={css.row} key={grant.grant_id} data-model-grant={grant.grant_id}>
        <div className={css.identity}><strong>{grant.name}</strong>{grant.revoked_at_ms !== null && <span className={css.badge}>{t('revoked')}</span>}<p>{t(grant.subject.kind === 'group' ? 'group' : 'user')} · {grant.subject_name || grant.subject.id}</p><code>{grant.model_ids.join(' · ')}</code><QuotaFacts quota={grant.quota} /><p>{t(grant.subject.kind === 'group' ? 'groupBudget' : 'userBudget')}</p><p>{t(grant.allow_resource_sharing ? 'resourceSharingAllowed' : 'resourceSharingDenied')}</p>{grant.expires_at_ms !== null && <p>{t('expires', { date: date(grant.expires_at_ms) })}</p>}</div>
        {editable && <div className={css.rowActions}><Button variant="outline" disabled={grant.revoked_at_ms !== null} onClick={() => setEditor(grant)}>{t('edit')}</Button><Button variant="ghost" disabled={grant.revoked_at_ms !== null} onClick={() => { setTarget(grant); setError('') }}>{t('revoke')}</Button></div>}
      </article>)}</div>
    </ModelDirectory>
    <ModelGroups editable={editable} />
    {editable && editor && <GrantEditor key={editor === 'new' ? 'new' : editor.grant_id} initial={editor === 'new' ? null : editor} onClose={() => setEditor(null)} onSaved={() => { setEditor(null); directory.reload() }} />}
    <ActionDialog open={target !== null} title={t('revokeGrantTitle')} description={t('revokeGrantDescription', { name: target?.name ?? '' })} cancelLabel={t('cancel')} confirmLabel={t('revoke')} busy={busy} error={error} destructive onOpenChange={open => { if (!open) setTarget(null) }} onConfirm={() => void revoke()} />
  </div>
}

function GrantEditor({ initial, onClose, onSaved }: { initial: ModelGrant | null; onClose(): void; onSaved(): void }) {
  const t = useTranslate('modelService')
  const [name, setName] = React.useState(initial?.name ?? '')
  const [kind, setKind] = React.useState<'user' | 'group'>(initial?.subject.kind ?? 'user')
  const [subjectId, setSubjectId] = React.useState(initial?.subject.id ?? '')
  const [subjectName, setSubjectName] = React.useState(initial?.subject_name ?? initial?.subject.id ?? '')
  const [models, setModels] = React.useState<string[]>(initial?.model_ids ?? [])
  const [monthlyTokens, setMonthlyTokens] = React.useState(String(initial?.quota.limit_tokens ?? 1_000_000))
  const [concurrent, setConcurrent] = React.useState(String(initial?.quota.max_concurrent_requests ?? 4))
  const [allowSharing, setAllowSharing] = React.useState(initial?.allow_resource_sharing ?? true)
  const [expiry, setExpiry] = React.useState(localDateInput(initial?.expires_at_ms ?? null))
  const [busy, setBusy] = React.useState(false)
  const [error, setError] = React.useState('')
  const [addingGroup, setAddingGroup] = React.useState(false)
  const [addingModel, setAddingModel] = React.useState(false)
  const [groupRevision, refreshGroups] = React.useReducer(value => value + 1, 0)
  const [modelRevision, refreshModels] = React.useReducer(value => value + 1, 0)
  const save = async () => {
    if (busy) return
    setBusy(true); setError('')
    try {
      if (!subjectId) throw new Error(t('subjectRequired'))
      if (!models.length) throw new Error(t('modelsRequired'))
      await api.request<ModelGrant>(initial ? modelResource(`${modelAdminPath}/grants`, initial.grant_id) : `${modelAdminPath}/grants`, { method: initial ? 'PUT' : 'POST', body: {
        name: name.trim(), allow_resource_sharing: allowSharing, subject: { kind, id: subjectId }, model_ids: models, monthly_tokens: Number(monthlyTokens), max_concurrent_requests: Number(concurrent), expires_at_ms: futureTimestamp(expiry, t('invalidExpiry')),
      } })
      onSaved()
    } catch (cause) { setError(errorMessage(cause)) }
    finally { setBusy(false) }
  }
  return <ModelModal title={t(initial ? 'grantEdit' : 'grantAdd')} description={t('grantDescription')} busy={busy} onClose={onClose}>
    <form className={css.form} onSubmit={event => { event.preventDefault(); void save() }}>
      <fieldset className={css.formStep} disabled={busy}>
      <legend>{t('grantRecipientStep')}</legend>
      <p className={css.hint}>{t('grantRecipientHint')}</p>
      <Field><Label htmlFor="model-grant-kind">{t('subjectKind')}</Label><Select id="model-grant-kind" disabled={busy || Boolean(initial)} value={kind} onValueChange={nextValue => { setKind(nextValue as 'user' | 'group'); setSubjectId(''); setSubjectName('') }}><option value="user">{t('user')}</option><option value="group">{t('group')}</option></Select></Field>
      {subjectName && <p className={css.notice}>{subjectName}</p>}
      {!initial && (kind === 'group'
        ? <><ModelPickerList<ModelGroup> key={groupRevision} path={`${modelAdminPath}/groups`} field="groups" label={t('chooseSubject')} selected={[subjectId]} disabled={busy} id={group => group.group_id} name={group => <>{group.name}<small>{t('groupMembers', { count: group.member_count })}</small></>} onPick={group => { setSubjectId(group.group_id); setSubjectName(group.name) }} /><div className={css.inlineAction}><p className={css.hint}>{t('groupPrerequisite')}</p><Button type="button" variant="outline" onClick={() => setAddingGroup(true)}><Plus />{t('groupAdd')}</Button></div></>
        : <ModelPickerList<ModelUser> key="user" path="/admin/accounts" field="accounts" label={t('memberSearch')} selected={[subjectId]} disabled={busy} id={user => user.user_id} name={user => <>{user.username}<small>{user.user_id}</small></>} onPick={user => { setSubjectId(user.user_id); setSubjectName(user.username) }} />)}
      </fieldset>
      <fieldset className={css.formStep} disabled={busy}>
      <legend>{t('grantModelsStep')}</legend>
      <div className={css.inlineAction}><p className={css.hint}>{t('publicationPrerequisite')}</p><Button type="button" variant="outline" onClick={() => setAddingModel(true)}><Plus />{t('publish')}</Button></div>
      <ModelPickerList<ModelPublication> key={modelRevision} path={`${modelAdminPath}/publications`} field="models" label={t('selectedModels')} selected={models} disabled={busy} id={model => model.model_id} name={model => <>{model.display_name}<small>{model.model_id}</small></>} onPick={model => setModels(current => current.includes(model.model_id) ? current.filter(id => id !== model.model_id) : [...current, model.model_id])} />
      <SelectedModels ids={models} onRemove={id => setModels(current => current.filter(model => model !== id))} />
      </fieldset>
      <fieldset className={css.formStep} disabled={busy}>
      <legend>{t('grantBudgetStep')}</legend>
      <Field><Label htmlFor="model-grant-name">{t('grantName')}</Label><Input id="model-grant-name" value={name} required maxLength={120} disabled={busy} onChange={event => setName(event.target.value)} /></Field>
      <div className={css.fields}><Field><Label htmlFor="model-grant-tokens">{t('monthlyTokens')}</Label><Input id="model-grant-tokens" type="number" min={1} step={1} value={monthlyTokens} required disabled={busy} onChange={event => setMonthlyTokens(event.target.value)} /></Field><Field><Label htmlFor="model-grant-concurrency">{t('concurrentRequests')}</Label><Input id="model-grant-concurrency" type="number" min={1} step={1} value={concurrent} required disabled={busy} onChange={event => setConcurrent(event.target.value)} /></Field></div>
      <p className={css.notice}>{t(kind === 'group' ? 'groupBudget' : 'userBudget')}</p>
      <div><label className={css.check}><input type="checkbox" checked={allowSharing} disabled={busy} onChange={event => setAllowSharing(event.target.checked)} />{t('allowResourceSharing')}</label><p className={css.hint}>{t('allowResourceSharingDescription')}</p></div>
      <Field><Label htmlFor="model-grant-expiry">{t('expiry')}</Label><Input id="model-grant-expiry" type="datetime-local" value={expiry} disabled={busy} onChange={event => setExpiry(event.target.value)} /></Field>
      </fieldset>
      {error && <p role="alert" className="text-sm text-destructive">{error}</p>}
      <div className={css.formFooter}><Button type="button" variant="outline" disabled={busy} onClick={onClose}>{t('cancel')}</Button><Button type="submit" disabled={busy || !models.length || !subjectId}>{t(busy ? 'saving' : 'save')}</Button></div>
    </form>
    {addingGroup && <GroupEditor initial={null} onClose={() => setAddingGroup(false)} onSaved={refreshGroups} onSelect={group => { setSubjectId(group.group_id); setSubjectName(group.name); setAddingGroup(false) }} />}
    {addingModel && <PublicationEditor initial={null} onClose={() => setAddingModel(false)} onSaved={model => { setModels(current => [...current, model.model_id]); refreshModels(); setAddingModel(false) }} />}
  </ModelModal>
}
