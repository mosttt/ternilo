import * as React from 'react'
import { Building2, Check, ChevronDown, LoaderCircle, Plus, Settings2, UserRound } from 'lucide-react'
import { Select as SelectPrimitive } from 'radix-ui'
import { navigate } from '@/app/navigation'
import { Button } from '@/components/ui/button'
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from '@/components/ui/dialog'
import { Field, Input, Label } from '@/components/ui/field'
import { useTranslate } from '@/i18n/provider'
import { useWorkbench } from '@/state/workbench'
import css from './admin.module.css'

export function SpaceSwitcher({ showManagement = false, showCreate = true, onNavigate }: { showManagement?: boolean; showCreate?: boolean; onNavigate?(): void }) {
  const t = useTranslate('admin')
  const { tenants, currentTenantId, currentTenantRole, selectTenant, createTenant, serverIdentity, notify } = useWorkbench()
  const [creating, setCreating] = React.useState(false)
  const [name, setName] = React.useState('')
  const [slug, setSlug] = React.useState('')
  const [busy, setBusy] = React.useState(false)
  const [error, setError] = React.useState('')
  const [switching, setSwitching] = React.useState(false)
  const selected = tenants.find(tenant => tenant.tenant_id === currentTenantId)
  const spaceName = (tenant: typeof tenants[number]) => tenant.kind === 'personal' ? t('space.personal') : tenant.display_name
  const changeSpace = async (tenantId: string) => {
    if (switching || tenantId === currentTenantId) return
    setSwitching(true)
    try { await selectTenant(tenantId) }
    catch (cause) { notify(cause instanceof Error ? cause.message : String(cause), 'error') }
    finally { setSwitching(false) }
  }
  const create = async () => {
    if (!name.trim() || !slug.trim() || busy) return
    setBusy(true)
    setError('')
    try {
      await createTenant(name.trim(), slug.trim())
      setCreating(false)
      setName('')
      setSlug('')
      notify(t('space.created'))
    } catch (cause) { setError(cause instanceof Error ? cause.message : String(cause)) }
    finally { setBusy(false) }
  }
  return <>
    <div className={css.spaceSwitcher} data-space-switcher="">
      <SelectPrimitive.Root value={currentTenantId ?? ''} onValueChange={value => void changeSpace(value)} disabled={switching || tenants.length === 0}>
        <SelectPrimitive.Trigger className={css.spaceTrigger} aria-label={t('space.switch')} aria-busy={switching} data-space-id={currentTenantId} title={selected ? spaceName(selected) : t('space.switch')}>
          {switching ? <LoaderCircle className="animate-spin" /> : selected?.kind === 'personal' ? <UserRound /> : <Building2 />}
          <SelectPrimitive.Value placeholder={t('space.switch')}>{selected ? spaceName(selected) : undefined}</SelectPrimitive.Value>
          <SelectPrimitive.Icon><ChevronDown /></SelectPrimitive.Icon>
        </SelectPrimitive.Trigger>
        <SelectPrimitive.Portal>
          <SelectPrimitive.Content className={css.spaceMenu} position="popper" side="top" align="start" sideOffset={6} collisionPadding={12} data-ternilo-dismiss-layer="" data-space-menu="">
            <SelectPrimitive.Viewport>
              {tenants.map(tenant => <SelectPrimitive.Item value={tenant.tenant_id} key={tenant.tenant_id} className={css.spaceOption} data-space-id={tenant.tenant_id} textValue={spaceName(tenant)}>
                {tenant.kind === 'personal' ? <UserRound /> : <Building2 />}
                <SelectPrimitive.ItemText>{spaceName(tenant)}</SelectPrimitive.ItemText>
                {tenant.kind !== 'personal' && <span className={css.spaceKind}>{t('space.team')}</span>}
                <SelectPrimitive.ItemIndicator className={css.spaceCheck}><Check /></SelectPrimitive.ItemIndicator>
              </SelectPrimitive.Item>)}
            </SelectPrimitive.Viewport>
          </SelectPrimitive.Content>
        </SelectPrimitive.Portal>
      </SelectPrimitive.Root>
      {showManagement && (currentTenantRole === 'owner' || currentTenantRole === 'admin') && <Button variant="ghost" size="icon" disabled={switching} aria-label={t('space.manage')} title={t('space.manage')} onClick={() => { onNavigate?.(); navigate('/spaces/current') }}><Settings2 /></Button>}
      {showCreate && serverIdentity?.instance.mode === 'multi_user' && <Button variant="ghost" size="icon" aria-label={t('space.create')} title={t('space.create')} onClick={() => { setError(''); setCreating(true) }}><Plus /></Button>}
    </div>
    <Dialog open={creating} onOpenChange={open => { if (!busy) setCreating(open) }}>
      <DialogContent className="max-w-md">
        <DialogHeader><DialogTitle>{t('space.create')}</DialogTitle><DialogDescription>{t('space.createDescription')}</DialogDescription></DialogHeader>
        <form className="grid gap-4" onSubmit={event => { event.preventDefault(); void create() }}>
          <Field><Label htmlFor="team-create-name">{t('space.name')}</Label><Input id="team-create-name" value={name} disabled={busy} required maxLength={256} onChange={event => setName(event.target.value)} /></Field>
          <Field><Label htmlFor="team-create-slug">{t('space.slug')}</Label><Input id="team-create-slug" value={slug} disabled={busy} required pattern="[a-z0-9]+(-[a-z0-9]+)*" maxLength={63} autoCapitalize="none" onChange={event => setSlug(event.target.value)} /><p className={css.hint}>{t('space.slugDescription')}</p></Field>
          {error && <p className="text-sm text-destructive" role="alert">{error}</p>}
          <Button disabled={busy || !name.trim() || !slug.trim()}>{t(busy ? 'loading' : 'space.create')}</Button>
        </form>
      </DialogContent>
    </Dialog>
  </>
}
