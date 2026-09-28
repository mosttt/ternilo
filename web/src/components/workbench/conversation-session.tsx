import { FolderArchive, Menu } from 'lucide-react'
import { useEffect, useRef, type KeyboardEvent, type ReactNode } from 'react'
import type { LocalSession, Workspace } from '@/types'
import type { Translate } from '@/i18n/runtime'
import type { ConversationView } from '@/domain/conversation-scroll-memory'
import type { ConversationViewContribution } from '@/plugins/conversation-registry'
import { Button } from '@/components/ui/button'
import { navigate } from '@/app/navigation'
import { filesLocation } from '@/components/files/files-api'
import { useTranslate } from '@/i18n/provider'
import css from './conversation-root.module.css'
import { WorkspaceHeaderActions } from './workspace-panel'

export function ConversationSessionHeader({
  session,
  workspace,
  blank,
  view,
  onView,
  onOpenMobileSidebar,
  actions,
  t,
  workspaceSessionTitle,
  displayTitle,
  views,
  viewTabsAvailable = true,
  workspacePathLabel,
  workspaceLocation,
  projectName,
}: {
  session: LocalSession
  workspace: Workspace | null
  blank: boolean
  view: ConversationView
  onView(view: ConversationView): void
  onOpenMobileSidebar(): void
  actions: ReactNode
  t: Translate<'conversation'>
  workspaceSessionTitle: string
  displayTitle?: string
  views: readonly ConversationViewContribution[]
  viewTabsAvailable?: boolean
  workspacePathLabel?: string
  workspaceLocation?: string
  projectName?: string
}) {
  const filesT = useTranslate('files')
  const workspaceT = useTranslate('workspace')
  const workspaceContext = useRef<HTMLDivElement>(null)
  const workspaceLabel = <>{workspacePathLabel ?? workspace?.path ?? session.workspace_path}{projectName && <span data-session-project=""> · {projectName}</span>}</>
  useEffect(() => {
    const element = workspaceContext.current
    if (!element) return
    const scroll = (event: WheelEvent) => {
      if (event.ctrlKey || Math.abs(event.deltaY) <= Math.abs(event.deltaX) || element.scrollWidth <= element.clientWidth) return
      const scale = event.deltaMode === 1 ? 16 : event.deltaMode === 2 ? element.clientWidth : 1
      element.scrollLeft += event.deltaY * scale
      event.preventDefault()
    }
    element.addEventListener('wheel', scroll, { passive: false })
    return () => element.removeEventListener('wheel', scroll)
  }, [])
  const title = displayTitle
    ?? (session.blank || session.title === 'New session' ? workspaceSessionTitle : session.title || workspaceSessionTitle)
  const selectAdjacentView = (event: KeyboardEvent<HTMLButtonElement>, index: number) => {
    let nextIndex: number | undefined
    if (event.key === 'ArrowRight') nextIndex = (index + 1) % views.length
    if (event.key === 'ArrowLeft') nextIndex = (index - 1 + views.length) % views.length
    if (event.key === 'Home') nextIndex = 0
    if (event.key === 'End') nextIndex = views.length - 1
    if (nextIndex === undefined) return
    event.preventDefault()
    onView(views[nextIndex].id)
    const tabs = event.currentTarget.parentElement?.querySelectorAll<HTMLButtonElement>('[role="tab"]')
    tabs?.[nextIndex]?.focus()
  }
  return (
    <header className={`${css.header} session-header ${blank ? css.headerHidden : ''}`} aria-hidden={blank || undefined}>
      <div className={`${css.titleRow} session-title-row`}>
        <div className={css.titleCluster}>
          <Button className={css.mobileSidebarButton} variant="ghost" size="icon-sm" onClick={onOpenMobileSidebar} aria-label={t('mobile.openSidebar')}>
            <Menu />
          </Button>
          <div className={css.titleText}>
            <h1 data-session-title="">{title}</h1>
            <div ref={workspaceContext} className={css.workspaceContext} data-session-workspace-context="" role="group" aria-label={workspaceT('location.title', { name: workspace?.title ?? workspaceSessionTitle })} tabIndex={0}>
              <span data-session-workspace="">{workspaceLabel}</span>
              {workspaceLocation && <> · <span className={css.workspaceLocation} data-session-workspace-path="" title={workspaceLocation}>{workspaceLocation}</span></>}
            </div>
          </div>
        </div>
        <div className={css.headerUtilities}>
          <Button variant="ghost" size="icon-sm" data-session-files="" aria-label={filesT('sessionFiles')} title={filesT('sessionFiles')} onClick={() => navigate(filesLocation({ session_id: session.identity.session_id }))}><FolderArchive /></Button>
          {actions}
          {!blank && <WorkspaceHeaderActions />}
        </div>
      </div>
      {viewTabsAvailable && <div className={css.tabs} role="tablist" aria-label={t('view.aria')}>
        {views.map((item, index) => <button
          key={item.id}
          id={`conversation-view-${encodeURIComponent(item.id)}-tab`}
          type="button"
          role="tab"
          aria-selected={view === item.id}
          aria-controls={`conversation-view-${encodeURIComponent(item.id)}-panel`}
          tabIndex={view === item.id ? 0 : -1}
          className={`${css.tab} ${view === item.id ? css.tabActive : ''}`}
          onClick={() => onView(item.id)}
          onKeyDown={event => selectAdjacentView(event, index)}
        >{item.label(t)}</button>)}
      </div>}
    </header>
  )
}

export function ConversationSession({ view, views = [view], content, contentClassName }: {
  view: ConversationView
  views?: readonly ConversationView[]
  content: ReactNode
  contentClassName?: string
}) {
  const orderedViews = [view, ...views.filter(item => item !== view)]
  return <>
    {orderedViews.map(item => (
      <div
        key={item}
        id={`conversation-view-${encodeURIComponent(item)}-panel`}
        className={css.viewArea}
        data-conversation-view={item}
        role="tabpanel"
        aria-labelledby={`conversation-view-${encodeURIComponent(item)}-tab`}
        hidden={item !== view}
      >
        {item === view && <div className={`${css.content} ${contentClassName ?? ''}`}>
          {content}
        </div>}
      </div>
    ))}
  </>
}
