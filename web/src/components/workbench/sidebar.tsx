import * as React from 'react'
import { BrandMark } from '@/components/ui/brand-mark'
import {
  FolderArchive, Shield, LogOut, MessageSquarePlus, PanelLeftClose, PanelLeftOpen, Settings, Sparkles, X,
} from 'lucide-react'
import { useWorkbench } from '@/state/workbench'
import { Tooltip, TooltipContent, TooltipTrigger } from '@/components/ui/tooltip'
import { useTranslate } from '@/i18n/provider'
import { cn } from '@/lib/utils'
import type { LiveConnectionStatus } from '@/api/live-client'
import type { SessionEvent } from '@/types'
import { WorkspaceBrowser } from './workspace-browser'
import css from './sidebar.module.css'
import { navigate } from '@/app/navigation'
import { isPlatformStaff } from '@/components/admin/admin-api'
import { SpaceSwitcher } from '@/components/admin/space-switcher'

const COLLAPSE_SETTLE_MS = 150

export function Sidebar({
  collapsed,
  mobileOpen,
  onCollapsedChange,
  onMobileOpenChange,
  onChooseWorkspace,
  onOpenSettings,
  currentSessionEvents = [],
  liveStatus = 'idle',
}: {
  collapsed: boolean
  mobileOpen: boolean
  onCollapsedChange(value: boolean): void
  onMobileOpenChange(value: boolean): void
  onChooseWorkspace(createSession?: boolean): void
  onOpenSettings(): void
  currentSessionEvents?: readonly SessionEvent[]
  liveStatus?: LiveConnectionStatus
}) {
  const {
    currentWorkspace, remote, platform,
    tenants, currentTenantRole, serverIdentity, logout, createSession, notify,
  } = useWorkbench()
  const canOperate = !platform || (currentTenantRole !== null && currentTenantRole !== 'viewer')
  const t = useTranslate('sidebar')
  const adminT = useTranslate('admin')
  const modelsT = useTranslate('modelService')
  const filesT = useTranslate('files')
  const [settled, setSettled] = React.useState(collapsed)
  const root = React.useRef<HTMLElement>(null)
  const lastWideWidth = React.useRef(280)

  React.useEffect(() => {
    if (!collapsed) {
      setSettled(false)
      return
    }
    const timer = window.setTimeout(() => setSettled(true), COLLAPSE_SETTLE_MS)
    return () => window.clearTimeout(timer)
  }, [collapsed])

  React.useEffect(() => {
    const element = root.current
    if (!element) return
    const observer = new ResizeObserver(entries => {
      if (!collapsed && entries[0]) lastWideWidth.current = entries[0].contentRect.width
    })
    observer.observe(element)
    return () => observer.disconnect()
  }, [collapsed])

  const wide = mobileOpen || !collapsed || !settled
  const liveConnected = liveStatus === 'ready'
  const liveConnecting = liveStatus === 'connecting' || liveStatus === 'authenticating'
  const connectionLabel = liveConnected
    ? platform ? t('connection.platform') : remote ? t('connection.remote') : currentWorkspace ? t('connection.local') : t('connection.kernel')
    : liveConnecting ? t('connection.connecting')
      : liveStatus === 'reconnecting' ? t('connection.reconnecting') : t('connection.disconnected')
  const startSession = async () => {
    if (!currentWorkspace) {
      onChooseWorkspace(true)
      return
    }
    try {
      await createSession(currentWorkspace.workspace_id)
      onMobileOpenChange(false)
    } catch (cause) {
      notify(cause instanceof Error ? cause.message : String(cause), 'error')
    }
  }

  return (
    <aside
      ref={root}
      className={cn(
        'app-sidebar', css.root,
        !wide && css.collapsed,
        collapsed && wide && css.fading,
        wide && css.wide,
      )}
      style={collapsed && wide ? { width: lastWideWidth.current } : undefined}
      aria-label={t('shell.aria')}
      data-collapsed={collapsed || undefined}
      data-mobile-open={mobileOpen || undefined}
    >
      <div className={css.brandRow}>
        {wide && (
          <button type="button" className={css.brand} aria-label={t('session.new.label')} disabled={!canOperate} onClick={() => void startSession()}>
            <span className={css.brandMark} aria-hidden="true"><BrandMark /></span>
            <strong className={css.brandName}>Ternilo</strong>
          </button>
        )}
        <button
          type="button"
          className={cn(css.iconButton, css.mobileClose)}
          data-mobile-sidebar-close=""
          aria-label={t('toggle.close')}
          onClick={() => onMobileOpenChange(false)}
        >
          <X size={18} />
        </button>
        <Tooltip>
          <TooltipTrigger asChild>
            <button
              type="button"
              className={cn(css.iconButton, css.desktopToggle, !wide && css.railToggle)}
              aria-label={collapsed ? t('toggle.open') : t('toggle.collapse')}
              onClick={() => onCollapsedChange(!collapsed)}
            >
              {!wide && <span className={css.railMark} aria-hidden="true"><BrandMark /></span>}
              {collapsed ? <PanelLeftOpen className={css.panelIcon} size={18} /> : <PanelLeftClose className={css.panelIcon} size={16} />}
            </button>
          </TooltipTrigger>
          <TooltipContent side="right">{collapsed ? t('toggle.open') : t('toggle.collapse')}</TooltipContent>
        </Tooltip>
      </div>

      {canOperate && <Tooltip>
        <TooltipTrigger asChild>
          <button type="button" className={css.newSession} data-sidebar-new-session="" aria-label={t('session.new')} onClick={() => void startSession()}>
            <MessageSquarePlus size={wide ? 14 : 18} />
            {wide && <><span className={css.newSessionLabel}>{t('session.new')}</span><kbd className={css.shortcut}>Ctrl K</kbd></>}
          </button>
        </TooltipTrigger>
        <TooltipContent side="right" hidden={wide}>{t('session.new')}</TooltipContent>
      </Tooltip>}

      <div className={css.browserSeat}>
        <WorkspaceBrowser
          wide={wide}
          readOnly={!canOperate}
          currentSessionEvents={currentSessionEvents}
          expandSidebar={() => onCollapsedChange(false)}
          onChooseWorkspace={onChooseWorkspace}
          onSessionActivated={() => onMobileOpenChange(false)}
        />
      </div>

      <div className={css.footer}>
        {platform && wide && tenants.length > 0 && (
          <SpaceSwitcher showManagement onNavigate={() => onMobileOpenChange(false)} />
        )}
        <Tooltip>
          <TooltipTrigger asChild><button type="button" className={css.footerAction} data-sidebar-files="" aria-label={filesT('title')} onClick={() => { onMobileOpenChange(false); navigate('/files') }}><FolderArchive size={16} />{wide && <span>{filesT('title')}</span>}</button></TooltipTrigger>
          <TooltipContent side="right" hidden={wide}>{filesT('title')}</TooltipContent>
        </Tooltip>
        {platform && <Tooltip>
          <TooltipTrigger asChild><button type="button" className={css.footerAction} aria-label={modelsT('accessTitle')} onClick={() => { onMobileOpenChange(false); navigate('/models') }}><Sparkles size={16} />{wide && <span>{modelsT('accessTitle')}</span>}</button></TooltipTrigger>
          <TooltipContent side="right" hidden={wide}>{modelsT('accessTitle')}</TooltipContent>
        </Tooltip>}
        <Tooltip>
          <TooltipTrigger asChild>
            <button type="button" className={css.footerAction} aria-label={platform ? t('userSettings') : t('settings')} onClick={() => { if (platform) onMobileOpenChange(false); onOpenSettings() }}>
              <Settings size={16} />{wide && <span>{platform ? t('userSettings') : t('settings')}</span>}
            </button>
          </TooltipTrigger>
          <TooltipContent side="right" hidden={wide}>{platform ? t('userSettings') : t('settings')}</TooltipContent>
        </Tooltip>
        {platform && isPlatformStaff(serverIdentity?.platform_role) && <div className={css.administration} data-administration-entry=""><Tooltip>
          <TooltipTrigger asChild><button type="button" className={css.footerAction} aria-label={adminT('title')} onClick={() => { onMobileOpenChange(false); navigate('/admin') }}><Shield size={16} />{wide && <span>{adminT('title')}</span>}</button></TooltipTrigger>
          <TooltipContent side="right" hidden={wide}>{adminT('title')}</TooltipContent>
        </Tooltip></div>}
        {platform && (
          <Tooltip>
            <TooltipTrigger asChild>
              <button type="button" className={css.footerAction} aria-label={t('logout')} onClick={logout}>
                <LogOut size={16} />{wide && <span>{t('logout')}</span>}
              </button>
            </TooltipTrigger>
            <TooltipContent side="right" hidden={wide}>{t('logout')}</TooltipContent>
          </Tooltip>
        )}
        {wide && (
          <div
            data-sidebar-connection=""
            data-live-state={liveStatus}
            className={css.connection}
            title={remote ? t('connection.relayTitle') : t('connection.localTitle')}
            role={liveConnected ? undefined : 'status'}
            aria-live={liveConnected ? undefined : 'polite'}
          >
            <span className={css.connectionDot} data-live-state={liveStatus} />
            <span className={css.connectionText}>
              {connectionLabel}
            </span>
          </div>
        )}
      </div>
    </aside>
  )
}
