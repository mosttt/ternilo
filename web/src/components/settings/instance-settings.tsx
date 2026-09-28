import * as React from 'react'
import { api, ApiError } from '@/api/client'
import { type InstanceMode, type ServerInstance } from '@/auth/server'
import { Button } from '@/components/ui/button'
import { Field, Label, Select } from '@/components/ui/field'
import { useTranslate } from '@/i18n/provider'
import { useWorkbench } from '@/state/workbench'
import { ActionDialog, GroupHeader, SectionHeader } from './settings-ui'
import { isPlatformStaff } from '@/components/admin/admin-api'
import { ServerSecuritySettingsPanel } from './server-security-settings'

export function InstanceSettings() {
  const { serverIdentity } = useWorkbench()
  return isPlatformStaff(serverIdentity?.platform_role) ? <InstancePanel /> : null
}

function InstancePanel() {
  const { serverIdentity: identity, acceptInstance, notify } = useWorkbench()
  const t = useTranslate('settings')
  const [mode, setMode] = React.useState<InstanceMode>(identity?.instance.mode ?? 'single_user')
  const [confirming, setConfirming] = React.useState(false)
  const [busy, setBusy] = React.useState(false)
  const [loaded, setLoaded] = React.useState(false)
  const [error, setError] = React.useState('')
  const userId = identity?.user.user_id

  React.useEffect(() => {
    let cancelled = false
    setLoaded(false)
    void api.request<ServerInstance>('/admin/instance').then(instance => {
      if (cancelled) return
      acceptInstance(instance)
      setMode(instance.mode)
      setLoaded(true)
    }).catch(cause => { if (!cancelled) setError(cause instanceof Error ? cause.message : String(cause)) })
    return () => { cancelled = true }
  }, [acceptInstance, userId])


  const saveMode = async () => {
    if (!identity || identity.platform_role !== 'owner') return
    setBusy(true)
    setError('')
    try {
      const instance = await api.request<ServerInstance>('/admin/instance', {
        method: 'PATCH', body: { mode, revision: identity.instance.revision },
      })
      acceptInstance(instance)
      setMode(instance.mode)
      setConfirming(false)
      notify(t('instance.saved'))
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause))
      if (cause instanceof ApiError && cause.status === 409) {
        try {
          const instance = await api.request<ServerInstance>('/admin/instance')
          acceptInstance(instance)
          setMode(instance.mode)
          setConfirming(false)
        } catch { /* Keep the original conflict visible. */ }
      }
    } finally { setBusy(false) }
  }

  const editable = identity?.platform_role === 'owner'

  return (
    <div className="grid gap-6 [&>header]:mb-0">
      <SectionHeader title={t('nav.instance')} description={t('instance.description')} />
      <div className="rounded-xl border bg-card p-5">
        <GroupHeader title={t('instance.mode')} description={t('instance.independent')} />
        <Field>
          <Label htmlFor="instance-mode">{t('instance.access')}</Label>
          <Select id="instance-mode" value={mode} disabled={busy || !loaded || !editable} onValueChange={nextValue => setMode(nextValue as InstanceMode)}>
            <option value="single_user">{t('instance.single')}</option>
            <option value="multi_user">{t('instance.multi')}</option>
          </Select>
          <p className="text-sm leading-relaxed text-muted-foreground">{t(mode === 'single_user' ? 'instance.singleDescription' : 'instance.multiDescription')}</p>
        </Field>
        <Button className="mt-4" disabled={busy || !loaded || !editable || mode === identity?.instance.mode} onClick={() => { setError(''); setConfirming(true) }}>{t('instance.save')}</Button>
      </div>
      {error && !confirming && <p className="text-sm text-destructive" role="alert">{error}</p>}
      {editable && <ServerSecuritySettingsPanel />}
      <ActionDialog
        open={confirming}
        title={t('instance.confirmTitle')}
        description={t(mode === 'single_user' ? 'instance.singleImpact' : 'instance.multiImpact')}
        cancelLabel={t('instance.cancel')}
        confirmLabel={t('instance.save')}
        busy={busy}
        error={error}
        onOpenChange={setConfirming}
        onConfirm={() => void saveMode()}
      />
    </div>
  )
}
