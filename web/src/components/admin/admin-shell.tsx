import * as React from 'react'
import { ArrowLeft, LogOut, Server, Settings2, Sparkles, Users } from 'lucide-react'
import { navigate } from '@/app/navigation'
import { BrandMark } from '@/components/ui/brand-mark'
import { Button } from '@/components/ui/button'
import { Select } from '@/components/ui/field'
import { InstanceSettings } from '@/components/settings/instance-settings'
import { PlatformSettings } from '@/components/settings/platform-settings'
import { WorkersSettings } from '@/components/settings/workers-settings'
import { useTranslate } from '@/i18n/provider'
import { useWorkbench } from '@/state/workbench'
import { cn } from '@/lib/utils'
import { AccountsPage } from './accounts-page'
import { ModelAdminPage } from '@/components/models/model-service-pages'
import { canReadAccounts, isPlatformStaff } from './admin-api'
import { SpaceSwitcher } from './space-switcher'
import css from './admin.module.css'

export function AdminShell({ path }: { path: string }) {
  const t = useTranslate('admin')
  const models = useTranslate('modelService')
  const sidebarT = useTranslate('sidebar')
  const { serverIdentity, platform, authRequired, tenants, currentTenantId, currentTenantRole, logout } = useWorkbench()
  const role = serverIdentity?.platform_role
  const space = tenants.find(item => item.tenant_id === currentTenantId)
  const spacePage = path === '/spaces/current'
  const staff = isPlatformStaff(role)
  const activePath = path === '/admin' ? canReadAccounts(role) ? '/admin/accounts' : '/admin/workers' : path
  const entries = staff ? [
    ...(canReadAccounts(role) ? [{ path: '/admin/accounts', label: t('accounts'), icon: Users }] : []),
    { path: '/admin/models', label: models('adminTitle'), icon: Sparkles },
    { path: '/admin/workers', label: t('workers'), icon: Server },
    { path: '/admin/instance', label: t('instance'), icon: Settings2 },
  ] : []
  const canManageSpace = currentTenantRole === 'owner' || currentTenantRole === 'admin'
  const allowed = platform && !authRequired && (spacePage ? Boolean(space && canManageSpace) : entries.some(item => item.path === activePath))
  React.useEffect(() => { document.title = `${spacePage ? t('space.manage') : t('title')} · Ternilo` }, [spacePage, t])

  return <div className={css.shell} data-admin-shell="" data-platform-admin={!spacePage || undefined} data-space-management={spacePage || undefined}>
    <a className="skip-link" href="#administration-main">{t('navigation')}</a>
    <aside className={css.sidebar}>
      <a href="/" className={css.brand} onClick={event => { event.preventDefault(); navigate('/') }}><BrandMark /><strong>Ternilo</strong></a>
      <span className={css.scope}>{spacePage ? t('space.manage') : t('title')}</span>
      {spacePage ? <SpaceSwitcher /> : <nav aria-label={t('navigation')}>
        {entries.map(entry => <a key={entry.path} href={entry.path} className={cn(css.navLink, entry.path === activePath && css.active)} aria-current={entry.path === activePath ? 'page' : undefined} onClick={event => { event.preventDefault(); navigate(entry.path) }}><entry.icon /><span>{entry.label}</span></a>)}
      </nav>}
      <div className={css.sidebarFooter}>
        <a href="/" className={css.navLink} onClick={event => { event.preventDefault(); navigate('/') }}><ArrowLeft />{t('workbench')}</a>
        <Button variant="ghost" className="justify-start" onClick={logout}><LogOut />{sidebarT('logout')}</Button>
      </div>
    </aside>
    <div className={css.main}>
      <header className={css.header}>
        <span className={css.mobileScope}>{spacePage ? t('space.manage') : t('title')}</span>
        <div className={css.mobileNavigation}>
          <a href="/" className={css.back} aria-label={t('workbench')} onClick={event => { event.preventDefault(); navigate('/') }}><ArrowLeft /></a>
          {spacePage ? <SpaceSwitcher /> : <Select aria-label={t('navigation')} value={activePath} onValueChange={nextValue => navigate(nextValue)}>{entries.map(entry => <option value={entry.path} key={entry.path}>{entry.label}</option>)}</Select>}
        </div>
        <span className={css.headerScope}>{spacePage ? space?.kind === 'personal' ? t('space.personal') : space?.display_name : t('scope')}</span>
        {serverIdentity && <div className={css.actor}><span>{serverIdentity.user.username}</span>{staff && !spacePage && <small>{t(`role.${serverIdentity.platform_role}`)}</small>}</div>}
      </header>
      <main id="administration-main" className={css.content}>
        {authRequired ? <div className={css.state} role="status">{t('loading')}</div>
          : !allowed ? <div className={css.state} role="alert">{t(spacePage ? 'space.unavailable' : 'denied')}</div>
            : spacePage && space ? <PlatformSettings key={space.tenant_id} tenantId={space.tenant_id} kind={space.kind} role={currentTenantRole === 'owner' ? 'owner' : 'admin'} />
              : activePath === '/admin/accounts' ? <AccountsPage key={serverIdentity?.user.user_id} />
                : activePath === '/admin/models' ? <ModelAdminPage key={role} /> : activePath === '/admin/workers' ? <WorkersSettings /> : <InstanceSettings />}
      </main>
    </div>
  </div>
}
