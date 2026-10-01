import * as React from 'react'
import { LoaderCircle } from 'lucide-react'
import { ApiError } from '@/api/client'
import type { RegistrationSettings } from '@/auth/server'
import { Button } from '@/components/ui/button'
import { Field, Label, Select } from '@/components/ui/field'
import { GroupHeader } from '@/components/settings/settings-ui'
import { useTranslate } from '@/i18n/provider'
import { useWorkbench } from '@/state/workbench'
import { getRegistrationSettings, setRegistrationSettings } from './admin-api'
import { InvitationPanel } from './invitation-panel'
import css from './admin.module.css'

export function RegistrationPanel() {
  const t = useTranslate('admin')
  const { serverIdentity } = useWorkbench()
  if (serverIdentity?.instance.mode !== 'multi_user') {
    return <p className={css.hint} data-registration-disabled="">{t('registration.singleUser')}</p>
  }
  return <RegistrationSettingsEditor />
}

function RegistrationSettingsEditor() {
  const t = useTranslate('admin')
  const { serverIdentity, notify } = useWorkbench()
  const canEdit = serverIdentity?.platform_role === 'owner' || serverIdentity?.platform_role === 'admin'
  const [settings, setSettings] = React.useState<RegistrationSettings | null>(null)
  const [draft, setDraft] = React.useState<RegistrationSettings | null>(null)
  const [loading, setLoading] = React.useState(true)
  const [saving, setSaving] = React.useState(false)
  const [error, setError] = React.useState('')
  const [revision, reload] = React.useReducer(value => value + 1, 0)
  React.useEffect(() => {
    const controller = new AbortController()
    setLoading(true)
    setError('')
    void getRegistrationSettings(controller.signal).then(value => {
      if (!controller.signal.aborted) { setSettings(value); setDraft(value) }
    }).catch(cause => {
      if (!controller.signal.aborted) setError(cause instanceof Error ? cause.message : String(cause))
    }).finally(() => { if (!controller.signal.aborted) setLoading(false) })
    return () => controller.abort()
  }, [revision])

  const save = async () => {
    if (!draft || !canEdit || saving) return
    setSaving(true)
    setError('')
    try {
      const value = await setRegistrationSettings(draft)
      setSettings(value)
      setDraft(value)
      notify(t('registration.saved'))
    } catch (cause) {
      if (cause instanceof ApiError && cause.status === 409) {
        notify(t('registration.conflict'), 'error')
        reload()
      } else setError(cause instanceof Error ? cause.message : String(cause))
    } finally { setSaving(false) }
  }

  return <>
    <section className={css.panel} data-registration-settings="">
      <GroupHeader title={t('registration.title')} description={t('registration.description')} />
      {loading ? <p className={css.state} role="status"><LoaderCircle className="size-4 animate-spin" />{t('loading')}</p>
        : draft && <form className={css.registrationForm} onSubmit={event => { event.preventDefault(); void save() }}>
          <Field>
            <Label htmlFor="registration-mode">{t('registration.mode')}</Label>
            <Select id="registration-mode" value={draft.mode} disabled={!canEdit || saving} onValueChange={nextValue => {
              const mode = nextValue as RegistrationSettings['mode']
              setDraft({ ...draft, mode, require_approval: mode === 'open' && draft.require_approval })
            }}>
              <option value="invite">{t('registration.invite')}</option>
              <option value="open">{t('registration.open')}</option>
            </Select>
          </Field>
          {draft.mode === 'open' ? <label className={css.approvalOption}>
            <input type="checkbox" checked={draft.require_approval} disabled={!canEdit || saving} onChange={event => setDraft({ ...draft, require_approval: event.target.checked })} />
            <span>{t('registration.requireApproval')}<small>{t('registration.approvalDescription')}</small></span>
          </label> : <p className={css.hint}>{t('registration.inviteDescription')}</p>}
          {canEdit && <Button type="submit" variant="outline" disabled={saving || (draft.mode === settings?.mode && draft.require_approval === settings.require_approval)}>
            {saving && <LoaderCircle className="animate-spin" />}{t('registration.save')}
          </Button>}
        </form>}
      {error && <div className={css.state} role="alert"><span>{error}</span>{!settings && <Button variant="outline" onClick={reload}>{t('retry')}</Button>}</div>}
    </section>
    {canEdit && !loading && settings?.mode === 'invite' && draft?.mode === 'invite' && <InvitationPanel key={settings.revision} />}
  </>
}
