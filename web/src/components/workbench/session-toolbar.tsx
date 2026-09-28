import * as React from 'react'
import {
  Archive, Bot, Check, ChevronDown, CircleGauge, Download, Ellipsis, FolderOpen, GitFork, Pencil,
  Plug, ScrollText, Shield, UsersRound,
} from 'lucide-react'
import type { AgentPresetRoster, LocalSession, PermissionPreset } from '@/types'
import { resourcePermissions } from '@/domain/resource-access'
import { permissionPresetsForPlacement } from '@/domain/default-permission'
import { Button } from '@/components/ui/button'
import {
  DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuLabel,
  DropdownMenuRadioGroup, DropdownMenuRadioItem, DropdownMenuSeparator, DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import { Tooltip, TooltipContent, TooltipTrigger } from '@/components/ui/tooltip'
import { localizeAgentPreset } from '@/i18n/builtin-metadata'
import { useTranslate } from '@/i18n/provider'
import { useOpenAgentTeam } from './agent-team-panel'
import { SessionServicesDialog } from './session-services-dialog'
import css from './session-toolbar.module.css'

export function SessionToolbar({
  session,
  onSetPermissions,
  open,
  onOpenChange,
}: {
  session: LocalSession
  onSetPermissions(value: PermissionPreset): Promise<void>
  open?: boolean
  onOpenChange?(open: boolean): void
}) {
  const t = useTranslate('conversation')
  const canConfigure = session.access?.permissions.configure ?? true
  const permissionLabels: Record<PermissionPreset, string> = {
    read_only: t('permission.readOnly'),
    workspace_write: t('permission.workspaceWrite'),
    full_access: t('permission.fullAccess'),
  }
  return (
    <div className={css.composerControls} aria-label={t('session.controls')}>
      <DropdownMenu open={open} onOpenChange={onOpenChange}>
        <DropdownMenuTrigger asChild>
          <Button type="button" disabled={!canConfigure} variant="ghost" size="xs" className={css.composerControl} aria-label={`${t('session.permission')}: ${permissionLabels[session.permissions]}`}>
            <Shield className="size-3.5" />
            <span className={css.composerControlLabel}>{permissionLabels[session.permissions]}</span>
            <ChevronDown className="size-3" />
          </Button>
        </DropdownMenuTrigger>
        <DropdownMenuContent side="top" align="start">
          <DropdownMenuLabel>{t('session.permissionCurrent')}</DropdownMenuLabel>
          <DropdownMenuRadioGroup value={session.permissions} onValueChange={value => { if (canConfigure) void onSetPermissions(value as PermissionPreset) }}>
            {permissionPresetsForPlacement(session.placement).map(value => (
              <DropdownMenuRadioItem key={value} value={value} disabled={!canConfigure}>{permissionLabels[value]}</DropdownMenuRadioItem>
            ))}
          </DropdownMenuRadioGroup>
        </DropdownMenuContent>
      </DropdownMenu>
    </div>
  )
}

export function SessionHeaderActions({
  session,
  presets,
  onSetPreset,
  onTogglePlan,
  readOnly = false,
  planLocked = false,
  presetLocked = !session.blank,
  forkLocked = false,
  onChooseWorkspace,
  onRename,
  onFork,
  onArchive,
  onExport,
  onEditModelLimit,
}: {
  session: LocalSession
  presets: AgentPresetRoster
  onSetPreset(value: string): Promise<void>
  onTogglePlan(): Promise<void>
  readOnly?: boolean
  planLocked?: boolean
  presetLocked?: boolean
  forkLocked?: boolean
  onChooseWorkspace(): void
  onRename(): void
  onFork(): Promise<void>
  onArchive(): Promise<void>
  onExport(): Promise<void>
  onEditModelLimit?(): void
}) {
  const t = useTranslate('conversation')
  const builtins = useTranslate('builtins')
  const observabilityT = useTranslate('observability')
  const permissions = resourcePermissions(session.access, !readOnly)
  const openTeam = useOpenAgentTeam()
  const [moreOpen, setMoreOpen] = React.useState(false)
  const [servicesOpen, setServicesOpen] = React.useState(false)
  const servicesDialog = servicesOpen ? <SessionServicesDialog key={session.identity.session_id} session={session} readOnly={readOnly} onClose={() => setServicesOpen(false)} /> : null
  const preset = presets.presets.find(item => item.id === session.agent_preset)
  const displayPreset = preset ? localizeAgentPreset(preset, builtins) : undefined
  const displayPresets = presets.presets.map(item => localizeAgentPreset(item, builtins))
  if (!permissions.configure && !permissions.submit) return (
    <>
    <div className={`${css.headerActions} session-header-actions`} aria-label={t('session.actions')} data-session-read-only-actions="">
      {openTeam && <Tooltip>
        <TooltipTrigger asChild>
          <Button type="button" variant="ghost" size="icon-xs" className={css.headerAction} aria-label={observabilityT('team.open')} onClick={() => openTeam()}>
            <UsersRound />
          </Button>
        </TooltipTrigger>
        <TooltipContent>{observabilityT('team.title')}</TooltipContent>
      </Tooltip>}
      <Tooltip><TooltipTrigger asChild><Button type="button" variant="ghost" size="icon-xs" className={`${css.headerAction} ${css.headerService}`} aria-label={t('services.title')} onClick={() => setServicesOpen(true)}><Plug /></Button></TooltipTrigger><TooltipContent>{t('services.title')}</TooltipContent></Tooltip>
      <Tooltip>
        <TooltipTrigger asChild>
          <Button type="button" variant="ghost" size="icon-xs" className={css.headerAction} aria-label={t('session.export')} onClick={() => void onExport()}><Download /></Button>
        </TooltipTrigger>
        <TooltipContent>{t('session.export')}</TooltipContent>
      </Tooltip>
    </div>
    {servicesDialog}
    </>
  )
  return (
    <>
    <div className={`${css.headerActions} session-header-actions`} aria-label={t('session.actions')}>
      <DropdownMenu>
        <DropdownMenuTrigger asChild>
          <Button type="button" disabled={!permissions.configure || presetLocked} title={presetLocked ? t('session.presetLocked') : undefined} variant="ghost" size="xs" className={`${css.headerAction} ${css.presetAction}`} aria-label={`${t('session.agentPreset')}: ${displayPreset?.display_name ?? session.agent_preset}`}>
            <Bot className="size-3.5" />
            <span className={css.headerActionLabel}>{displayPreset?.display_name ?? session.agent_preset}</span>
            <ChevronDown className="size-3" />
          </Button>
        </DropdownMenuTrigger>
        <DropdownMenuContent side="top" align="start" className="w-72">
          <DropdownMenuLabel>{t('session.agentPreset')}</DropdownMenuLabel>
          {displayPresets.map(item => (
            <DropdownMenuItem key={item.id} disabled={!permissions.configure || presetLocked} onSelect={() => void onSetPreset(item.id)}>
              <div className="min-w-0 flex-1">
                <div>{item.display_name}</div>
                <div className="line-clamp-2 text-xs text-muted-foreground">{item.description}</div>
              </div>
              {item.id === session.agent_preset && <Check />}
            </DropdownMenuItem>
          ))}
        </DropdownMenuContent>
      </DropdownMenu>

      {openTeam && <Tooltip>
        <TooltipTrigger asChild>
          <Button type="button" variant="ghost" size="icon-xs" className={css.headerAction} aria-label={observabilityT('team.open')} onClick={() => openTeam()}>
            <UsersRound />
          </Button>
        </TooltipTrigger>
        <TooltipContent>{observabilityT('team.title')}</TooltipContent>
      </Tooltip>}

      <DropdownMenu open={moreOpen} onOpenChange={setMoreOpen}>
        <DropdownMenuTrigger asChild><Button type="button" variant="ghost" size="icon-xs" className={`${css.headerAction} ${css.headerMore}`} aria-label={t('session.moreActions')}><Ellipsis /></Button></DropdownMenuTrigger>
        <DropdownMenuContent side="bottom" align="end">
          <DropdownMenuLabel className={css.mobileOnly}><span className="flex items-center gap-2"><Bot className="size-3.5" />{t('session.agentPreset')}</span></DropdownMenuLabel>
          {displayPresets.map(item => <DropdownMenuItem disabled={!permissions.configure || presetLocked} className={css.mobileOnly} key={`mobile-${item.id}`} onSelect={() => void onSetPreset(item.id)}><div className="min-w-0 flex-1"><div>{item.display_name}</div><div className="line-clamp-2 text-xs text-muted-foreground">{item.description}</div></div>{item.id === session.agent_preset && <Check />}</DropdownMenuItem>)}
          <DropdownMenuSeparator className={css.mobileOnlySeparator} />
          {openTeam && <DropdownMenuItem className={css.mobileOnly} onSelect={() => openTeam()}><UsersRound />{observabilityT('team.title')}</DropdownMenuItem>}
          <DropdownMenuItem disabled={planLocked || !permissions.configure} onSelect={() => void onTogglePlan()}><ScrollText />{session.mode === 'plan' ? t('session.toExecution') : t('session.toPlan')}</DropdownMenuItem>
          <DropdownMenuItem className={css.mobileOnly} onSelect={onChooseWorkspace}><FolderOpen />{t('session.chooseFolder')}</DropdownMenuItem>
          <DropdownMenuSeparator className={css.mobileOnlySeparator} />
          <DropdownMenuItem onSelect={() => setServicesOpen(true)}><Plug />{t('services.title')}</DropdownMenuItem>
          <DropdownMenuItem disabled={!permissions.configure} onSelect={onRename}><Pencil />{t('session.rename')}</DropdownMenuItem>
          {session.placement === 'cloud' && onEditModelLimit && <DropdownMenuItem disabled={!permissions.configure} onSelect={onEditModelLimit}><CircleGauge />{t('session.modelLimit')}</DropdownMenuItem>}
          <DropdownMenuItem disabled={forkLocked || !permissions.submit} onSelect={() => void onFork()}><GitFork />{t(forkLocked ? 'session.forking' : 'session.fork')}</DropdownMenuItem>
          <DropdownMenuItem disabled={!permissions.configure} onSelect={() => void onArchive()}><Archive />{t('session.archive')}</DropdownMenuItem>
          <DropdownMenuSeparator />
          <DropdownMenuItem asChild>
            <button type="button" onClick={() => {
              setMoreOpen(false)
              void onExport()
            }}><Download />{t('session.export')}</button>
          </DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>
    </div>
    {servicesDialog}
    </>
  )
}
