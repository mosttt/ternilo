import * as React from 'react'
import { ArrowLeft, FolderCog, X } from 'lucide-react'
import { api } from '@/api/client'
import { navigate } from '@/app/navigation'
import { Button } from '@/components/ui/button'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { useTranslate } from '@/i18n/provider'
import { settingsSectionRegistry, visibleSettingsSections } from '@/plugins/settings-registry'
import { useWorkbench } from '@/state/workbench'
import type { Profile, SettingsSection } from '@/types'
import { ownsExecutionConfiguration } from '@/domain/resource-access'
import { cn } from '@/lib/utils'
import './builtin-settings'
import styles from './settings-layout.module.css'

export function SettingsDialog({
  open,
  onOpenChange,
  onSessionChanged,
  initialSection,
  effectiveProfile,
  page = false,
}: {
  open: boolean
  onOpenChange(open: boolean): void
  onSessionChanged(): Promise<void>
  initialSection?: SettingsSection
  effectiveProfile?: Profile | null
  page?: boolean
}) {
  const [section, setSection] = React.useState<string>('general')
  const t = useTranslate('settings')
  const adminT = useTranslate('admin')
  const {
    currentTenantId,
    tenants,
    currentTenantRole: tenantRole,
    currentWorkspace,
    currentSession,
    serverIdentity,
    platform,
    remote,
    notify,
  } = useWorkbench()
  const registeredSections = React.useSyncExternalStore(
    settingsSectionRegistry.subscribe,
    settingsSectionRegistry.getSnapshot,
    settingsSectionRegistry.getSnapshot,
  )
  const instanceOwner = serverIdentity?.is_instance_owner ?? false
  const targetOwner = ownsExecutionConfiguration(currentWorkspace, currentSession, !platform || tenantRole !== 'viewer')
  const contributions = React.useMemo(
    () => visibleSettingsSections({ platform, tenantRole, instanceOwner, targetOwner }, registeredSections),
    [platform, registeredSections, tenantRole, instanceOwner, targetOwner],
  )
  const activeContribution = contributions.find(item => item.id === section) ?? contributions[0]
  const closeRef = React.useRef<HTMLButtonElement>(null)
  const contentRef = React.useRef<HTMLDivElement>(null)
  const restoreFocusRef = React.useRef<HTMLElement | null>(null)
  const [openingConfiguration, setOpeningConfiguration] = React.useState(false)
  const targetLocation = currentWorkspace?.placement === 'local_node' || (remote && !platform)
    ? t('target.node')
    : currentWorkspace?.placement === 'cloud' || platform
      ? t('target.cloud')
      : t('target.local')
  const workspaceTargetLabel = currentWorkspace
    ? `${targetLocation} · ${currentWorkspace.title}`
    : targetLocation
  const targetLabel = activeContribution?.id === 'computers'
      ? t('target.space', { name: tenants?.find(tenant => tenant.tenant_id === currentTenantId)?.display_name ?? currentTenantId ?? '' })
      : activeContribution?.id === 'general' && platform ? t('target.account')
        : platform && !currentWorkspace && !serverIdentity?.instance.managed_execution_enabled
        ? t('target.choose')
        : workspaceTargetLabel

  const openConfiguration = async () => {
    setOpeningConfiguration(true)
    try {
      await api.request('/configuration/open', { method: 'POST' })
      notify(t('configurationOpened'))
    } catch (cause) {
      notify(cause instanceof Error ? cause.message : String(cause), 'error')
    } finally {
      setOpeningConfiguration(false)
    }
  }

  React.useEffect(() => {
    if (!open) return
    const requested = initialSection ?? 'general'
    setSection(contributions.some(item => item.id === requested) ? requested : contributions[0]?.id ?? 'general')
  }, [initialSection, open, platform])

  React.useEffect(() => {
    if (!open || contributions.some(item => item.id === section)) return
    setSection(contributions[0]?.id ?? 'general')
  }, [contributions, open, section])

  React.useEffect(() => { if (page) document.title = `${t('userTitle')} · Ternilo` }, [page, t])
  React.useEffect(() => {
    const content = contentRef.current
    if (!page || !content) return
    let frame = 0
    const observer = new ResizeObserver(() => {
      cancelAnimationFrame(frame)
      frame = requestAnimationFrame(() => {
        const focused = document.activeElement
        if (!(focused instanceof HTMLElement) || !content.contains(focused) || !focused.matches('input, textarea, select, [contenteditable="true"]')) return
        const bounds = content.getBoundingClientRect()
        const field = focused.getBoundingClientRect()
        if (field.top < bounds.top || field.bottom > bounds.bottom) focused.scrollIntoView({ block: 'nearest' })
      })
    })
    observer.observe(content)
    return () => { observer.disconnect(); cancelAnimationFrame(frame) }
  }, [page])

  const content = <>
        <aside className={styles.navigation}>
          {page ? <header className={styles.navigationHeader}><h1 className="text-lg font-semibold">{t('userTitle')}</h1><p className="sr-only">{t('userDescription')}</p></header> : <DialogHeader className={styles.navigationHeader}>
            <DialogTitle>{t('title')}</DialogTitle>
            <DialogDescription id="settings-description" className="sr-only">{t('description')}</DialogDescription>
          </DialogHeader>}
          <nav className={styles.navigationList} aria-label={t('navigation')}>
            {contributions.map((item) => {
              const Icon = item.icon
              const label = item.label(t)
              return (
                <button
                  type="button"
                  key={item.id}
                  className={cn(
                    styles.navigationButton,
                    activeContribution?.id === item.id && styles.navigationButtonActive,
                  )}
                  aria-current={activeContribution?.id === item.id ? 'page' : undefined}
                  onClick={() => { setSection(item.id); if (page) navigate(`/settings/${encodeURIComponent(item.id)}`) }}
                >
                  <Icon className="size-4" />
                  <span>{label}</span>
                </button>
              )
            })}
          </nav>
        </aside>
        <div className={styles.main}>
          <div className={styles.toolbar} data-settings-toolbar="">
            <span
              className={styles.target}
              title={t('target.label', { target: targetLabel })}
              aria-label={t('target.label', { target: targetLabel })}
              data-settings-target=""
            >
              {targetLabel}
            </span>
            {window.__TERNILO_BOOT__?.openConfig ? (
              <Button
                type="button"
                variant="outline"
                size="sm"
                className={styles.openConfiguration}
                disabled={openingConfiguration}
                onClick={() => void openConfiguration()}
              >
                <FolderCog />
                <span className={styles.openConfigurationLabel}>
                  {t(openingConfiguration ? 'openingConfiguration' : 'openConfiguration')}
                </span>
              </Button>
            ) : null}
            <Button
              ref={closeRef}
              type="button"
              variant="ghost"
              size={page ? 'sm' : 'icon-sm'}
              className={styles.close}
              aria-label={page ? adminT('workbench') : t('close')}
              onClick={() => onOpenChange(false)}
            >
              {page ? <><ArrowLeft /><span>{adminT('workbench')}</span></> : <X />}
            </Button>
          </div>
          <div ref={contentRef} className={styles.content} data-settings-content="" role={page ? 'main' : undefined} id={page ? 'user-settings-main' : undefined}>
            <div className={styles.section} data-settings-page-content="">
              {activeContribution?.render({ platform, tenantRole, instanceOwner, targetOwner, currentTenantId, effectiveProfile, onSessionChanged })}
            </div>
          </div>
        </div>
  </>
  if (page) return <div className={cn(styles.shell, styles.pageShell)} data-settings-surface="" data-user-settings=""><a className="skip-link" href="#user-settings-main">{t('navigation')}</a>{content}</div>
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent
        className={cn(styles.shell, 'max-w-none')}
        showClose={false}
        aria-describedby="settings-description"
        data-settings-surface=""
        onOpenAutoFocus={(event) => {
          event.preventDefault()
          restoreFocusRef.current = document.activeElement instanceof HTMLElement ? document.activeElement : null
          closeRef.current?.focus()
        }}
        onCloseAutoFocus={(event) => {
          event.preventDefault()
          if (restoreFocusRef.current?.isConnected) restoreFocusRef.current.focus()
          restoreFocusRef.current = null
        }}
      >
        {content}
      </DialogContent>
    </Dialog>
  )
}

export function UserSettingsPage({ section }: { section: SettingsSection }) {
  const { accountScope, refresh, loading, authRequired, serverIdentity, currentTenantId, currentTenantRole } = useWorkbench()
  const t = useTranslate('settings')
  const [readyScope, setReadyScope] = React.useState<string | undefined>()
  const ready = !authRequired && Boolean(serverIdentity && currentTenantId && currentTenantRole)
    && (!loading || readyScope === accountScope)
  React.useEffect(() => {
    if (ready && !loading) setReadyScope(accountScope)
  }, [ready, loading, accountScope])
  if (!ready) return <div className={cn(styles.shell, styles.pageShell)} data-user-settings=""><p role="status" className="p-6 text-sm text-muted-foreground">{t('platform.loading')}</p></div>
  return <SettingsDialog key={accountScope ?? 'account'} page open onOpenChange={open => { if (!open) navigate('/') }} initialSection={section} onSessionChanged={refresh} />
}
