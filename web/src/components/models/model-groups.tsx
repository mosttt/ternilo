import * as React from 'react'
import { Plus, Users } from 'lucide-react'
import { api } from '@/api/client'
import { Button } from '@/components/ui/button'
import { Field, Input, Label, Textarea } from '@/components/ui/field'
import { ActionDialog } from '@/components/settings/settings-ui'
import { useTranslate } from '@/i18n/provider'
import { modelAdminPath, modelResource, type ModelGroup, type ModelUser } from './model-service-api'
import { ModelDirectory, ModelModal, errorMessage, useModelPage } from './model-service-ui'
import css from './model-service.module.css'

export function ModelGroups({ editable = true }: { editable?: boolean }) {
  const t = useTranslate('modelService')
  const directory = useModelPage<ModelGroup>(`${modelAdminPath}/groups`, 'groups')
  const [editor, setEditor] = React.useState<ModelGroup | 'new' | null>(null)
  const [target, setTarget] = React.useState<ModelGroup | null>(null)
  const [busy, setBusy] = React.useState(false)
  const [error, setError] = React.useState('')
  const remove = async () => {
    if (!target || busy) return
    setBusy(true); setError('')
    try { await api.request<void>(modelResource(`${modelAdminPath}/groups`, target.group_id), { method: 'DELETE' }); setTarget(null); directory.reload() }
    catch (cause) { setError(errorMessage(cause)) }
    finally { setBusy(false) }
  }
  return <section className={css.page} data-model-groups="">
    <div><h3 className="text-base font-semibold">{t('groups')}</h3><p className={css.hint}>{t('groupDescription')}</p></div>
    <p className={css.notice}>{t('groupBoundary')}</p>
    <ModelDirectory state={directory} label={t('groups')} actions={editable ? <Button onClick={() => setEditor('new')}><Plus />{t('groupAdd')}</Button> : null}>
      <div className={css.list}>{directory.items.map(group => <article className={css.row} key={group.group_id} data-model-group={group.group_id}>
        <div className={css.identity}><strong>{group.name}</strong>{group.description && <p>{group.description}</p>}<p>{t('groupMembers', { count: group.member_count })}</p></div>
        {editable && <div className={css.rowActions}><Button variant="outline" onClick={() => setEditor(group)}><Users />{t('groupManage')}</Button><Button variant="ghost" onClick={() => { setTarget(group); setError('') }}>{t('groupRemove')}</Button></div>}
      </article>)}</div>
    </ModelDirectory>
    {editable && editor && <GroupEditor key={editor === 'new' ? 'new' : editor.group_id} initial={editor === 'new' ? null : editor} onClose={() => setEditor(null)} onSaved={directory.reload} />}
    <ActionDialog open={target !== null} title={t('groupRemove')} description={t('groupRemoveDescription', { name: target?.name ?? '' })} cancelLabel={t('cancel')} confirmLabel={t('groupRemove')} busy={busy} error={error} destructive onOpenChange={open => { if (!open) setTarget(null) }} onConfirm={() => void remove()} />
  </section>
}

export function GroupEditor({ initial, onClose, onSaved, onSelect }: { initial: ModelGroup | null; onClose(): void; onSaved(): void; onSelect?(group: ModelGroup): void }) {
  const t = useTranslate('modelService')
  const [group, setGroup] = React.useState(initial)
  const [name, setName] = React.useState(initial?.name ?? '')
  const [description, setDescription] = React.useState(initial?.description ?? '')
  const [saving, setSaving] = React.useState(false)
  const [memberBusy, setMemberBusy] = React.useState(false)
  const [error, setError] = React.useState('')
  const busy = saving || memberBusy
  const save = async () => {
    if (busy) return
    setSaving(true); setError('')
    try {
      const next = await api.request<ModelGroup>(group ? modelResource(`${modelAdminPath}/groups`, group.group_id) : `${modelAdminPath}/groups`, { method: group ? 'PUT' : 'POST', body: { name: name.trim(), description: description.trim() || null } })
      setGroup(next); setName(next.name); setDescription(next.description ?? ''); onSaved()
    } catch (cause) { setError(errorMessage(cause)) }
    finally { setSaving(false) }
  }
  return <ModelModal title={t(group ? 'groupManage' : 'groupAdd')} description={t('groupBoundary')} busy={busy} onClose={onClose}>
    <form className={css.form} onSubmit={event => { event.preventDefault(); void save() }}>
      <Field><Label htmlFor="model-group-name">{t('name')}</Label><Input id="model-group-name" value={name} required maxLength={120} disabled={busy} onChange={event => setName(event.target.value)} /></Field>
      <Field><Label htmlFor="model-group-description">{t('description')}</Label><Textarea id="model-group-description" value={description} maxLength={2000} disabled={busy} onChange={event => setDescription(event.target.value)} /></Field>
      {error && <p role="alert" className="text-sm text-destructive">{error}</p>}
      <div className={css.formFooter}><Button type="submit" disabled={busy || !name.trim()}>{t(saving ? 'saving' : 'save')}</Button></div>
    </form>
    {group && <GroupMembers group={group} onBusy={setMemberBusy} disabled={busy} onChanged={next => { setGroup(next); onSaved() }} />}
    {group && onSelect && <div className={css.formFooter}><Button disabled={busy || group.member_count === 0} onClick={() => onSelect(group)}>{t('useGroup')}</Button>{group.member_count === 0 && <p className={css.hint}>{t('groupNeedsMembers')}</p>}</div>}
  </ModelModal>
}

function GroupMembers({ group, onBusy, onChanged, disabled }: { group: ModelGroup; onBusy(value: boolean): void; onChanged(value: ModelGroup): void; disabled: boolean }) {
  const t = useTranslate('modelService')
  const [adding, setAdding] = React.useState(false)
  return <section className={css.page}>
    <div className={css.search}><h3 className="text-sm font-semibold">{t('groupMembers', { count: group.member_count })}</h3><Button disabled={disabled} variant="outline" onClick={() => setAdding(!adding)}>{t(adding ? 'backMembers' : 'addMembers')}</Button></div>
    <GroupMemberDirectory key={String(adding)} group={group} adding={adding} disabled={disabled} onBusy={onBusy} onChanged={onChanged} />
  </section>
}

function GroupMemberDirectory({ group, adding, disabled, onBusy, onChanged }: { group: ModelGroup; adding: boolean; disabled: boolean; onBusy(value: boolean): void; onChanged(value: ModelGroup): void }) {
  const t = useTranslate('modelService')
  const base = modelResource(`${modelAdminPath}/groups`, group.group_id)
  const directory = useModelPage<ModelUser>(`${base}/${adding ? 'candidates' : 'members'}`, 'users')
  const [busy, setBusy] = React.useState('')
  const [added, setAdded] = React.useState<string[]>([])
  const [error, setError] = React.useState('')
  const save = async (userId: string) => {
    if (busy || disabled) return
    setBusy(userId); onBusy(true); setError('')
    try {
      await api.request<void>(modelResource(`${base}/members`, userId), { method: adding ? 'PUT' : 'DELETE' })
      if (adding) setAdded(current => [...current, userId]); else directory.reload()
      onChanged(await api.request<ModelGroup>(base))
    } catch (cause) { setError(errorMessage(cause)) }
    finally { setBusy(''); onBusy(false) }
  }
  return <div data-model-group-members="">
    {error && <p role="alert" className="mb-3 text-sm text-destructive">{error}</p>}
    <ModelDirectory state={directory} label={t('memberSearch')}>
      <div className={css.list}>{directory.items.map(user => <article className={css.row} key={user.user_id} data-model-group-member={user.user_id}>
        <div className={css.identity}><strong>{user.username}</strong><code>{user.user_id}</code></div>
        <Button variant="outline" disabled={disabled || Boolean(busy) || added.includes(user.user_id)} onClick={() => void save(user.user_id)}>{t(adding ? added.includes(user.user_id) ? 'added' : 'add' : 'remove')}</Button>
      </article>)}</div>
    </ModelDirectory>
  </div>
}
