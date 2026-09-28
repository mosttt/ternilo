import { copyText } from '@/lib/clipboard'
import * as React from 'react'
import { Copy, LoaderCircle, UserPlus } from 'lucide-react'
import { api } from '@/api/client'
import { invitationUrl, type ServerInvitation } from '@/auth/server'
import { Button } from '@/components/ui/button'
import { Field, Input, Label, Select } from '@/components/ui/field'
import { GroupHeader } from '@/components/settings/settings-ui'
import { useTranslate } from '@/i18n/provider'
import { useWorkbench } from '@/state/workbench'
import css from './admin.module.css'

export function InvitationPanel({ tenantId }: { tenantId?: string }) {
  const t = useTranslate('admin')
  const settingsT = useTranslate('settings')
  const { serverIdentity, notify } = useWorkbench()
  const [role, setRole] = React.useState<ServerInvitation['role']>('member')
  const [invitation, setInvitation] = React.useState<ServerInvitation | null>(null)
  const [busy, setBusy] = React.useState(false)
  const [error, setError] = React.useState('')
  const multiUser = serverIdentity?.instance.mode === 'multi_user'

  const invite = async () => {
    setBusy(true)
    setError('')
    setInvitation(null)
    try {
      setInvitation(await api.request<ServerInvitation>('/admin/invitations', {
        method: 'POST', body: { tenant_id: tenantId ?? null, role, expires_in_seconds: 604800 },
      }))
    } catch (cause) { setError(cause instanceof Error ? cause.message : String(cause)) }
    finally { setBusy(false) }
  }

  return <section className={css.panel} data-admin-invitations={tenantId ?? 'platform'}>
    <GroupHeader title={t(tenantId ? 'invite.teamTitle' : 'invite.title')} description={t(tenantId ? 'invite.teamDescription' : 'invite.description')} />
    {!multiUser ? <p className={css.hint}>{t('invite.multiOnly')}</p> : <>
      <div className={css.inviteActions}>
        {tenantId && <Field>
          <Label htmlFor="invitation-team-role">{t('invite.role')}</Label>
          <Select id="invitation-team-role" value={role} disabled={busy} onValueChange={nextValue => { setRole(nextValue as ServerInvitation['role']); setInvitation(null) }}>
            {(['viewer', 'member', 'admin'] as const).map(value => <option key={value} value={value}>{settingsT(`role.${value}`)}</option>)}
          </Select>
        </Field>}
        <Button variant="outline" disabled={busy} onClick={() => void invite()}>{busy ? <LoaderCircle className="animate-spin" /> : <UserPlus />}{t('invite.create')}</Button>
      </div>
      {invitation && <div className={css.invitation} role="status">
        <Label htmlFor="admin-invitation-url">{t('invite.link')}</Label>
        <Input id="admin-invitation-url" readOnly value={invitationUrl(invitation.token, Boolean(invitation.tenant_id))} onFocus={event => event.target.select()} />
        <p className={css.hint}>{t('invite.expires', { date: new Date(invitation.expires_at_ms).toLocaleString() })}</p>
        <div className={css.inviteActions}>
          <Button variant="outline" onClick={() => void copyText(invitationUrl(invitation.token, Boolean(invitation.tenant_id))).then(() => notify(t('invite.copied'))).catch(cause => setError(String(cause)))}><Copy />{t('invite.copy')}</Button>
          <Button variant="ghost" onClick={() => setInvitation(null)}>{t('invite.hide')}</Button>
        </div>
      </div>}
    </>}
    {error && <p className="text-sm text-destructive" role="alert">{error}</p>}
  </section>
}
