import * as React from 'react'
import DOMPurify from 'dompurify'
import { ArrowLeftRight, ChevronDown, ChevronRight, Columns2, Copy, Download, File, Folder, FolderOpen, LoaderCircle, Maximize, Minimize, PanelRightClose, PanelRightOpen, Plus, RefreshCw, X } from 'lucide-react'
import { renderMarkdown } from '../../../rich-text.source.js'
import { useTranslate } from '@/i18n/provider'
import { useWorkbench } from '@/state/workbench'
import { copyText } from '@/lib/clipboard'
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger } from '@/components/ui/dropdown-menu'
import { StreamingCodeBlock } from './chat/streaming-code-block'
import { closeWorkspaceTab, fileTreeTab, moveWorkspaceTab, openWorkspaceTab, type WorkspaceTab } from './workspace-panel-state'
import { useWorkspaceSurface, workspaceRequest, type WorkspaceApplication, type WorkspaceDirectory, type WorkspacePreview } from './workspace-panel-context'
import css from './workspace-panel.module.css'

const names = new Intl.Collator(undefined, { numeric: true, sensitivity: 'base' })

function useWorkspaceRead<Result>(sessionId: string, body: unknown, revision: number) {
  const key = JSON.stringify([sessionId, body, revision])
  const [result, setResult] = React.useState<{ key: string; data: Result | null; error: string }>({ key: '', data: null, error: '' })
  React.useEffect(() => {
    const controller = new AbortController()
    const [, request] = JSON.parse(key) as [string, unknown]
    void workspaceRequest<Result>(sessionId, request, controller.signal)
      .then(data => { if (!controller.signal.aborted) setResult({ key, data, error: '' }) })
      .catch(cause => { if (!controller.signal.aborted) setResult({ key, data: null, error: cause instanceof Error ? cause.message : String(cause) }) })
    return () => controller.abort()
  }, [key, sessionId])
  return result.key === key ? result : { data: null, error: '' }
}

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

function FilePreview({ sessionId, path, revision }: { sessionId: string; path: string; revision: number }) {
  const t = useTranslate('files')
  const chatT = useTranslate('chat')
  const result = useWorkspaceRead<WorkspacePreview>(sessionId, { kind: 'read', path }, revision)
  const [rendered, setRendered] = React.useState(/\.(md|markdown)$/i.test(path))
  const [objectUrl, setObjectUrl] = React.useState('')
  const [error, setError] = React.useState('')
  const [palette, setPalette] = React.useState(() => ({ background: getComputedStyle(document.body).getPropertyValue('--background'), foreground: getComputedStyle(document.body).getPropertyValue('--foreground') }))
  React.useEffect(() => {
    const observer = new MutationObserver(() => setPalette({ background: getComputedStyle(document.body).getPropertyValue('--background'), foreground: getComputedStyle(document.body).getPropertyValue('--foreground') }))
    observer.observe(document.documentElement, { attributes: true, attributeFilter: ['class'] })
    return () => observer.disconnect()
  }, [])
  React.useEffect(() => {
    setObjectUrl('')
    const file = result.data
    if (!file || file.truncated || file.encoding === 'unsupported') return
    const bytes = file.encoding === 'base64' ? Uint8Array.from(atob(file.content), char => char.charCodeAt(0)) : file.content
    const url = URL.createObjectURL(new Blob([bytes], { type: file.media_type }))
    setObjectUrl(url)
    return () => URL.revokeObjectURL(url)
  }, [result.data])
  const file = result.data
  const renderable = /\.(md|markdown|html?|svg)$/i.test(path)
  const html = React.useMemo(() => {
    if (!file || file.encoding !== 'utf8' || !rendered || !renderable) return ''
    const content = /\.(md|markdown)$/i.test(path) ? renderMarkdown(file.content, { copy: chatT('message.copy'), copyCode: chatT('message.copyCode'), taskCompleted: chatT('message.taskCompleted'), taskPending: chatT('message.taskPending') }) : file.content
    const clean = DOMPurify.sanitize(content, { FORBID_TAGS: ['button', 'iframe', 'form'] })
    return `<!doctype html><html><head><meta charset="utf-8"><meta http-equiv="Content-Security-Policy" content="default-src 'none'; img-src data:; style-src 'unsafe-inline'; form-action 'none'; base-uri 'none'"><style>html{color-scheme:light dark}body{margin:18px;background:${palette.background};color:${palette.foreground};font:14px/1.7 system-ui;overflow-wrap:anywhere}pre{overflow:auto;padding:12px;background:rgba(128,128,128,.08)}code{font-family:monospace}img{max-width:100%}table{border-collapse:collapse}td,th{border:1px solid #888;padding:6px}a{color:#679efe}</style></head><body>${clean}</body></html>`
  }, [file, rendered, renderable, path, palette, chatT])
  if (result.error) return <div className={css.note} role="alert">{result.error}</div>
  if (!file) return <div className={css.note}>{t('previewLoading')}</div>
  return <div className={css.document} data-workspace-preview={path}>
    <div className={css.previewToolbar}>
      <span>{t('workspace.bytes', { count: file.bytes })}</span>
      {renderable && file.encoding === 'utf8' && <button type="button" onClick={() => setRendered(value => !value)}>{t(rendered ? 'workspace.source' : 'workspace.render')}</button>}
      {file.encoding === 'utf8' && <button type="button" aria-label={t('workspace.copy')} onClick={() => void copyText(file.content).catch(cause => setError(String(cause)))}><Copy /></button>}
      {objectUrl && <a href={objectUrl} download={path.split('/').at(-1)} aria-label={t('download')} title={t('download')}><Download /></a>}
    </div>
    {error && <div role="alert" className={css.note}>{error}</div>}
    {file.truncated && <div className={css.note}>{t('workspace.previewTruncated')}</div>}
    {file.encoding === 'utf8' ? rendered && renderable
      ? <><div className={css.safety}>{t('workspace.safePreview')}</div><iframe className={css.documentFrame} title={t('preview', { name: path })} sandbox="" srcDoc={html} /></>
      : <div className={css.code} onClick={event => { if ((event.target as HTMLElement).closest('.markdown-copy')) void copyText(file.content).catch(cause => setError(String(cause))) }}><StreamingCodeBlock code={file.content} lang={path.split('.').at(-1)} copyLabel={t('workspace.copy')} copyAria={t('workspace.copy')} /></div>
      : file.media_type.startsWith('image/') && objectUrl ? <div className={css.image}><img src={objectUrl} alt={path} /></div>
      : file.media_type === 'application/pdf' && objectUrl ? <iframe className={css.documentFrame} title={t('preview', { name: path })} src={objectUrl} />
      : <div className={css.note}>{t('workspace.unsupported')}</div>}
  </div>
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
        : <FilePreview sessionId={surface.sessionId!} path={tab.path} revision={revision} />}
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
