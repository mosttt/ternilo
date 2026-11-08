import * as React from 'react'
import { ArrowLeftRight, ChevronDown, ChevronRight, Columns2, File, Folder, FolderOpen, LoaderCircle, Maximize, Minimize, PanelRightClose, PanelRightOpen, Plus, RefreshCw, X } from 'lucide-react'
import { useTranslate } from '@/i18n/provider'
import { useWorkbench } from '@/state/workbench'
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger } from '@/components/ui/dropdown-menu'
import { WorkspaceFilePreview } from './workspace-file-preview'
import { closeWorkspaceTab, fileTreeTab, moveWorkspaceTab, openWorkspaceTab, type WorkspaceTab } from './workspace-panel-state'
import { useWorkspaceRead, useWorkspaceSurface, type WorkspaceApplication, type WorkspaceDirectory } from './workspace-panel-context'
import css from './workspace-panel.module.css'

const names = new Intl.Collator(undefined, { numeric: true, sensitivity: 'base' })

function ApplicationIcon({ app }: { app: WorkspaceApplication }) {
  const [failed, setFailed] = React.useState(false)
  return app.icon && !failed
    ? <img className={css.appIcon} src={app.icon} alt="" aria-hidden draggable={false} onError={() => setFailed(true)} />
    : <svg className={css.appIcon} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" aria-hidden><rect x="3" y="3" width="18" height="18" rx="5" /></svg>
}

export function WorkspaceHeaderActions() {
  const surface = useWorkspaceSurface()
  const { platform } = useWorkbench()
  const t = useTranslate('files')
  const [busy, setBusy] = React.useState(false)
  React.useEffect(() => {
    if (!surface?.launching) { setBusy(false); return }
    const timer = setTimeout(() => setBusy(true), 250)
    return () => clearTimeout(timer)
  }, [surface?.launching])
  if (!surface?.sessionId) return null
  const apps = platform ? [] : surface.info?.applications ?? []
  const selected = apps.find(app => app.id === surface.choice) ?? apps[0]
  const label = (app: WorkspaceApplication) => app.id === 'filemanager' ? t('workspace.fileManager') : app.label
  const launch = (app: typeof apps[number]) => { void surface.launch(app).catch(cause => surface.notifyError(cause instanceof Error ? cause.message : String(cause))) }
  return <>
    {selected && <div className={css.openApp} data-workspace-open-app="" data-busy={busy || undefined}>
      <button type="button" disabled={surface.launching} title={t('workspace.openIn', { app: label(selected) })} aria-label={t('workspace.openIn', { app: label(selected) })} onClick={() => launch(selected)}>
        {busy ? <LoaderCircle className={css.spinner} /> : <ApplicationIcon key={selected.id} app={selected} />}
      </button>
      <DropdownMenu><DropdownMenuTrigger asChild><button type="button" disabled={surface.launching} aria-label={t('workspace.chooseApp')}><ChevronDown /></button></DropdownMenuTrigger>
        <DropdownMenuContent align="end" sideOffset={4} className={css.appMenu} data-workspace-app-menu="">{apps.map(app => <DropdownMenuItem key={app.id} className={css.appMenuItem} data-current={app.id === selected.id || undefined} aria-current={app.id === selected.id || undefined} onSelect={() => launch(app)}>
          <ApplicationIcon app={app} /><span>{label(app)}</span>
        </DropdownMenuItem>)}</DropdownMenuContent>
      </DropdownMenu>
    </div>}
    <button ref={surface.triggerRef} type="button" className={css.headerButton} data-workspace-toggle="" aria-label={t(surface.state.open ? 'workspace.collapse' : 'workspace.open')} aria-expanded={surface.state.open}
      title={surface.error || (!surface.info?.can_browse ? t('workspace.denied') : t('workspace.open'))}
      disabled={!surface.info?.can_browse} onClick={surface.state.open ? surface.hide : surface.show}><PanelRightOpen /></button>
  </>
}

function DirectoryLevel({ sessionId, path, expanded, onToggle, onOpen, revision }: {
  sessionId: string; path: string; expanded: string[]; onToggle(path: string): void; onOpen(path: string): void; revision: number
}) {
  const t = useTranslate('files')
  const result = useWorkspaceRead<WorkspaceDirectory>(sessionId, { kind: 'list', path }, revision)
  if (result.error) return <li className={css.note} role="alert">{result.error}</li>
  if (!result.data) return <li className={css.note}>{t('loading')}</li>
  const entries = [...result.data.entries].sort((left, right) => Number(right.kind === 'directory') - Number(left.kind === 'directory') || names.compare(left.name, right.name))
  return <>
    {entries.length === 0 && <li className={css.note}>{t('workspace.empty')}</li>}
    {entries.map(entry => {
      const child = path ? `${path}/${entry.name}` : entry.name
      const opened = expanded.includes(child)
      const directory = entry.kind === 'directory'
      return <li key={entry.name}>
        <button type="button" className={css.treeRow} data-workspace-entry={child} data-kind={entry.kind} disabled={entry.kind === 'other'}
          title={entry.kind === 'other' ? t('workspace.other') : entry.name} aria-expanded={directory ? opened : undefined}
          onClick={() => directory ? onToggle(child) : onOpen(child)}>
          {directory ? opened ? <ChevronDown /> : <ChevronRight /> : <span className={css.chevronSpace} />}
          {directory ? opened ? <FolderOpen /> : <Folder /> : <File />}<span>{entry.name}</span>
        </button>
        {directory && opened && <ul className={css.level}><DirectoryLevel sessionId={sessionId} path={child} expanded={expanded} onToggle={onToggle} onOpen={onOpen} revision={revision} /></ul>}
      </li>
    })}
    {result.data.truncated && <li className={css.note}>{t('workspace.listTruncated')}</li>}
  </>
}

function WorkspacePane({ index }: { index: number }) {
  const surface = useWorkspaceSurface()!
  const t = useTranslate('files')
  const pane = surface.state.panes[index]!
  const [revision, refresh] = React.useReducer(value => value + 1, 0)
  const active = pane.tabs.find(tab => tab.id === pane.active)!
  const title = (tab: WorkspaceTab) => tab.path?.split('/').at(-1) ?? t('title')
  const select = (id: string) => surface.update(state => ({ ...state, panes: state.panes.map((pane, paneIndex) => paneIndex === index ? { ...pane, active: id } : pane) }))
  const toggle = (id: string, path: string) => surface.update(state => ({ ...state, panes: state.panes.map((pane, paneIndex) => paneIndex === index ? { ...pane, tabs: pane.tabs.map(tab => tab.id === id ? { ...tab, expanded: tab.expanded.includes(path) ? tab.expanded.filter(value => value !== path) : [...tab.expanded, path] } : tab) } : pane) }))
  const open = (path: string) => surface.update(state => openWorkspaceTab(state, state.panes.length === 2 ? 1 - index : index, path))
  return <section className={css.pane} data-workspace-pane={index} onDragOver={event => { if (event.dataTransfer.types.includes('application/x-ternilo-workspace-tab')) event.preventDefault() }} onDrop={event => {
    const id = event.dataTransfer.getData('application/x-ternilo-workspace-tab')
    if (id) { event.preventDefault(); surface.update(state => moveWorkspaceTab(state, id, index)) }
  }}>
    <div className={css.tabStrip}>
      <div className={css.tabs} role="tablist" aria-label={t('workspace.tabs')}>
        {pane.tabs.map((tab, tabIndex) => <div className={css.tabItem} data-active={tab.id === pane.active || undefined} key={tab.id} draggable onDragStart={event => event.dataTransfer.setData('application/x-ternilo-workspace-tab', tab.id)}>
          <button type="button" role="tab" id={`workspace-tab-${tab.id}`} aria-controls={`workspace-view-${tab.id}`} aria-selected={tab.id === pane.active} tabIndex={tab.id === pane.active ? 0 : -1} title={tab.path ?? t('title')}
            onClick={() => select(tab.id)} onKeyDown={event => {
              const offset = event.key === 'ArrowRight' ? 1 : event.key === 'ArrowLeft' ? -1 : 0
              if (!offset) return
              event.preventDefault()
              const next = pane.tabs[(tabIndex + offset + pane.tabs.length) % pane.tabs.length]!
              select(next.id); document.getElementById(`workspace-tab-${next.id}`)?.focus()
            }}>{tab.path ? <File /> : <Folder />}<span>{title(tab)}</span></button>
          <button type="button" aria-label={t('workspace.closeTab', { name: title(tab) })} onClick={() => surface.update(state => closeWorkspaceTab(state, index, tab.id))}><X /></button>
        </div>)}
      </div>
      <button type="button" className={css.icon} aria-label={t('workspace.newTab')} title={t('workspace.newTab')} onClick={() => surface.update(state => openWorkspaceTab(state, index, null))}><Plus /></button>
      {surface.state.panes.length === 2 && <button type="button" className={css.icon} aria-label={t('workspace.moveTab')} title={t('workspace.moveTab')} onClick={() => surface.update(state => moveWorkspaceTab(state, active.id, 1 - index))}><ArrowLeftRight /></button>}
    </div>
    <div className={css.path}><span title={active.path ? `${surface.info?.root}/${active.path}` : surface.info?.root}>{active.path ?? surface.info?.root}</span><button type="button" className={css.icon} aria-label={t('refresh')} onClick={refresh}><RefreshCw /></button></div>
    {pane.tabs.map(tab => <div key={tab.id} id={`workspace-view-${tab.id}`} role="tabpanel" aria-labelledby={`workspace-tab-${tab.id}`} hidden={tab.id !== pane.active} className={css.tabBody}>
      {tab.path === null ? <ul className={css.tree}><DirectoryLevel sessionId={surface.sessionId!} path="" expanded={tab.expanded} onToggle={path => toggle(tab.id, path)} onOpen={open} revision={revision} /></ul>
        : <WorkspaceFilePreview sessionId={surface.sessionId!} path={tab.path} revision={revision} />}
    </div>)}
  </section>
}

export function WorkspacePanel({ overlay }: { overlay: boolean }) {
  const surface = useWorkspaceSurface()!
  const t = useTranslate('files')
  const root = React.useRef<HTMLElement>(null)
  const close = React.useRef<HTMLButtonElement>(null)
  React.useEffect(() => {
    const previous = document.activeElement instanceof HTMLElement ? document.activeElement : null
    close.current?.focus({ preventScroll: true })
    return () => { if (previous?.isConnected) previous.focus({ preventScroll: true }) }
  }, [])
  const split = () => surface.update(state => {
    if (state.panes.length === 2) return { ...state, panes: [{ tabs: state.panes.flatMap(pane => pane.tabs), active: state.panes[0]!.active }] }
    const selected = state.panes[0]!.tabs.find(tab => tab.id === state.panes[0]!.active)!
    const tab = { ...fileTreeTab(), path: selected.path, expanded: [...selected.expanded] }
    return { ...state, panes: [...state.panes, { tabs: [tab], active: tab.id }] }
  })
  return <aside ref={root} className={css.panel} data-workspace-panel="" role={overlay ? 'dialog' : 'complementary'} aria-modal={overlay || undefined} aria-label={t('workspace.title')}
    onKeyDown={event => {
      if (event.defaultPrevented) return
      if (event.key === 'Escape') { event.preventDefault(); surface.hide() }
      if (event.key === 'Tab' && overlay) {
        const items = [...(root.current?.querySelectorAll<HTMLElement>('button:not(:disabled), a[href], [tabindex="0"]') ?? [])].filter(item => item.getClientRects().length > 0)
        const first = items[0], last = items.at(-1)
        if (event.shiftKey && document.activeElement === first) { event.preventDefault(); last?.focus() }
        else if (!event.shiftKey && document.activeElement === last) { event.preventDefault(); first?.focus() }
      }
    }}>
    <div className={css.panelTools}><span>{t('workspace.title')}</span>
      <button type="button" className={css.icon} aria-label={t('workspace.split')} aria-pressed={surface.state.panes.length === 2} title={t('workspace.split')} onClick={split}><Columns2 /></button>
      <button type="button" className={css.icon} aria-label={t(surface.state.fullscreen ? 'workspace.restore' : 'workspace.fullscreen')} onClick={() => surface.update(state => ({ ...state, fullscreen: !state.fullscreen }))}>{surface.state.fullscreen ? <Minimize /> : <Maximize />}</button>
      <button ref={close} type="button" className={css.icon} aria-label={t('workspace.collapse')} onClick={surface.hide}><PanelRightClose /></button>
    </div>
    {surface.error ? <div className={css.note} role="alert">{surface.error}<button type="button" onClick={surface.reloadInfo}>{t('retry')}</button></div>
      : !surface.info ? <div className={css.note}>{t('loading')}</div>
      : !surface.info.can_browse ? <div className={css.note}>{t('workspace.denied')}</div>
      : <div className={css.panes} data-split={surface.state.panes.length === 2 || undefined}>{surface.state.panes.map((_, index) => <WorkspacePane key={index} index={index} />)}</div>}
  </aside>
}
