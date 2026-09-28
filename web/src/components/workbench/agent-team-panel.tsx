import * as React from 'react'
import { ListTodo, LoaderCircle, Mail, RefreshCcw, UsersRound } from 'lucide-react'
import type { SessionLiveActivity } from '@/api/live-client'
import { deriveSubagents, mergeCanonicalSubagentEvents } from '@/domain/observability'
import { useTranslate } from '@/i18n/provider'
import type { AgentTeamSnapshot, LocalSession, SessionEvent } from '@/types'
import { Button } from '@/components/ui/button'
import {
  Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle,
} from '@/components/ui/dialog'
import { AgentTeamMailbox } from './agent-team-mailbox'
import { AgentTeamRoster } from './agent-team-roster'
import { AgentTeamTasks } from './agent-team-tasks'
import { useAgentTeam } from './use-agent-team'
import css from './agent-team-panel.module.css'

type AgentTeamView = 'roster' | 'tasks' | 'mailbox'
type OpenAgentTeam = (subagentId?: string) => void

const AgentTeamOpenContext = React.createContext<OpenAgentTeam | null>(null)

function publicError(message: string, sessions: readonly LocalSession[], replacement: string) {
  return sessions.reduce((safe, session) => (
    safe.split(session.identity.session_id).join(replacement)
  ), message)
}

/** Open the Team shared by the active root, child, or grandchild Session. */
export function useOpenAgentTeam(): OpenAgentTeam | null {
  return React.useContext(AgentTeamOpenContext)
}

export function AgentTeamTrigger({ className, label = false }: { className?: string; label?: boolean }) {
  const openTeam = useOpenAgentTeam()
  const t = useTranslate('observability')
  if (!openTeam) return null
  return <Button
    type="button"
    variant="ghost"
    size={label ? 'sm' : 'icon-xs'}
    className={className}
    aria-label={t('team.open')}
    onClick={() => openTeam()}
  >
    <UsersRound aria-hidden="true" />{label ? t('team.title') : null}
  </Button>
}

/**
 * Own the Team surface for the active Session. The shared live snapshot is
 * canonical; child Session histories refine durable member status.
 */
export function AgentTeamSurface({
  sessionId,
  events,
  sessions,
  liveSnapshot = null,
  liveActivity = {},
  readOnly = false,
  canStop = !readOnly,
  onOpenSession,
  onFollowup,
  onStop,
  children,
}: {
  sessionId: string | null
  events: readonly SessionEvent[]
  sessions: readonly LocalSession[]
  liveSnapshot?: AgentTeamSnapshot | null
  liveActivity?: Readonly<Record<string, SessionLiveActivity>>
  readOnly?: boolean
  canStop?: boolean
  onOpenSession(id: string): void
  onFollowup(subagentId: string, message: string): Promise<void>
  onStop(subagentId: string): Promise<void>
  children: React.ReactNode
}) {
  const t = useTranslate('observability')
  const workspaceT = useTranslate('workspace')
  const eventSubagents = React.useMemo(() => deriveSubagents(events), [events])
  const [open, setOpen] = React.useState(false)
  const [view, setView] = React.useState<AgentTeamView>('roster')
  const [selectedSubagentId, setSelectedSubagentId] = React.useState<string | null>(null)
  const [messageRecipient, setMessageRecipient] = React.useState<string | null>(null)
  const [legacyPending, setLegacyPending] = React.useState<string | null>(null)
  const [legacyError, setLegacyError] = React.useState('')
  const legacyActionToken = React.useRef<symbol | null>(null)
  const sessionEpoch = React.useRef({ id: sessionId, value: 0 })
  if (sessionEpoch.current.id !== sessionId) {
    sessionEpoch.current = { id: sessionId, value: sessionEpoch.current.value + 1 }
    legacyActionToken.current = null
  }
  const team = useAgentTeam(sessionId, open, sessions, liveSnapshot, liveActivity)
  const visibleSnapshot = React.useMemo(() => team.snapshot ? {
    ...team.snapshot,
    members: team.snapshot.members.map(member => (
      member.label === 'New session' ? { ...member, label: workspaceT('session.new') } : member
    )),
  } : null, [team.snapshot, workspaceT])
  const subagents = React.useMemo(
    () => mergeCanonicalSubagentEvents(eventSubagents, team.memberEvents),
    [eventSubagents, team.memberEvents],
  )

  React.useEffect(() => {
    setOpen(false)
    setView('roster')
    setSelectedSubagentId(null)
    setMessageRecipient(null)
    legacyActionToken.current = null
    setLegacyPending(null)
    setLegacyError('')
  }, [sessionId])

  const openTeam = React.useCallback<OpenAgentTeam>((subagentId) => {
    setSelectedSubagentId(subagentId ?? null)
    setMessageRecipient(null)
    setView('roster')
    setLegacyError('')
    setOpen(true)
  }, [])

  const selectedMemberId = React.useMemo(() => {
    if (!visibleSnapshot) return null
    if (!selectedSubagentId) return visibleSnapshot.current_member_id
    return visibleSnapshot.members.find(member => member.subagent_id === selectedSubagentId)?.id ?? null
  }, [selectedSubagentId, visibleSnapshot])

  const unread = visibleSnapshot?.messages.filter(message => (
    message.to === visibleSnapshot.current_member_id && !message.read_at_ms
  )).length ?? 0
  const actionDetail = publicError(team.actionError || legacyError, sessions, t('team.sessionReference'))

  const legacyAct = async (key: string, operation: () => Promise<void>) => {
    const targetSessionId = sessionId
    const targetEpoch = sessionEpoch.current.value
    const isCurrent = () => sessionEpoch.current.id === targetSessionId
      && sessionEpoch.current.value === targetEpoch
    if (!targetSessionId || !isCurrent() || legacyActionToken.current !== null) return false
    const token = Symbol(key)
    legacyActionToken.current = token
    setLegacyPending(key)
    setLegacyError('')
    try {
      await operation()
      if (!isCurrent() || legacyActionToken.current !== token) return false
      setOpen(false)
      return true
    } catch (cause) {
      if (isCurrent() && legacyActionToken.current === token) {
        setLegacyError(cause instanceof Error ? cause.message : String(cause))
      }
      return false
    } finally {
      if (isCurrent() && legacyActionToken.current === token) {
        legacyActionToken.current = null
        setLegacyPending(null)
      }
    }
  }

  const selectView = (next: AgentTeamView) => {
    team.clearNotice()
    setView(next)
  }

  const selectAdjacentView = (event: React.KeyboardEvent<HTMLButtonElement>, index: number) => {
    let nextIndex: number | undefined
    if (event.key === 'ArrowRight') nextIndex = (index + 1) % tabs.length
    if (event.key === 'ArrowLeft') nextIndex = (index - 1 + tabs.length) % tabs.length
    if (event.key === 'Home') nextIndex = 0
    if (event.key === 'End') nextIndex = tabs.length - 1
    if (nextIndex === undefined) return
    event.preventDefault()
    const next = tabs[nextIndex]!
    selectView(next.id)
    event.currentTarget.parentElement?.querySelectorAll<HTMLButtonElement>('[role="tab"]')[nextIndex]?.focus()
  }

  const tabs: { id: AgentTeamView; icon: typeof UsersRound; label: string; count: number }[] = [
    { id: 'roster', icon: UsersRound, label: t('team.view.roster'), count: visibleSnapshot?.members.length ?? 0 },
    { id: 'tasks', icon: ListTodo, label: t('team.view.tasks'), count: visibleSnapshot?.tasks.length ?? 0 },
    { id: 'mailbox', icon: Mail, label: t('team.view.mailbox'), count: unread },
  ]

  return <AgentTeamOpenContext.Provider value={openTeam}>
    {children}
    <Dialog open={open} onOpenChange={setOpen}>
      <DialogContent className={css.panel} data-agent-team-panel="">
        <DialogHeader className={css.header}>
          <div className={css.titleRow}>
            <DialogTitle className={css.title}><UsersRound aria-hidden="true" />{t('team.title')}</DialogTitle>
            <Button type="button" variant="ghost" size="icon-sm" aria-label={t('team.refresh')} disabled={team.loading} onClick={() => void team.refresh()}>
              <RefreshCcw className={team.loading ? css.spin : undefined} aria-hidden="true" />
            </Button>
          </div>
          <DialogDescription>{t('team.description')}</DialogDescription>
          {readOnly && !canStop && <p className={css.readOnly} data-agent-team-viewer-read-only="">{t('team.readOnly.viewer')}</p>}
        </DialogHeader>
        <div className={css.tabs} role="tablist" aria-label={t('team.views')}>
          {tabs.map((tab, index) => {
            const Icon = tab.icon
            return <button
              key={tab.id}
              id={`agent-team-${tab.id}-tab`}
              type="button"
              role="tab"
              aria-selected={view === tab.id}
              aria-controls={`agent-team-${tab.id}-panel`}
              tabIndex={view === tab.id ? 0 : -1}
              data-active={view === tab.id || undefined}
              onClick={() => selectView(tab.id)}
              onKeyDown={event => selectAdjacentView(event, index)}
            >
              <Icon aria-hidden="true" /><span>{tab.label}</span>
              {tab.count > 0 && <em data-unread={tab.id === 'mailbox' && unread > 0 || undefined}>{tab.count}</em>}
            </button>
          })}
        </div>
        {(team.conflict || team.actionError || legacyError) && <div className={css.notice} data-kind={team.conflict ? 'conflict' : 'error'} role="alert">
          {team.conflict ? t('team.conflict') : <><strong>{t('team.actionFailed')}</strong><small>{actionDetail}</small></>}
        </div>}
        <div className={css.body}>
          {team.loading && !visibleSnapshot ? <div className={css.loadState} role="status"><LoaderCircle className={css.spin} aria-hidden="true" /><strong>{t('team.loading')}</strong></div> : null}
          {team.loadError && !visibleSnapshot ? <div className={css.loadState} role="alert"><strong>{t('team.loadFailed')}</strong><p>{t('team.loadFailedHint')}</p><Button type="button" size="sm" variant="outline" onClick={() => void team.refresh()}><RefreshCcw />{t('team.retry')}</Button></div> : null}
          {visibleSnapshot && <>
            <section id="agent-team-roster-panel" role="tabpanel" aria-labelledby="agent-team-roster-tab" hidden={view !== 'roster'}>
              {view === 'roster' && <AgentTeamRoster
                snapshot={visibleSnapshot}
                subagents={subagents}
                sessions={sessions}
                selectedMemberId={selectedMemberId}
                pendingAction={legacyPending}
                readOnly={readOnly}
                allowStop={canStop}
                onOpenSession={id => { setOpen(false); onOpenSession(id) }}
                onFollowup={(subagentId, message) => legacyAct(`followup:${subagentId}`, () => onFollowup(subagentId, message))}
                onStop={subagentId => legacyAct(`stop:${subagentId}`, () => onStop(subagentId))}
                onMessage={memberId => { setMessageRecipient(memberId); selectView('mailbox') }}
                t={t}
              />}
            </section>
            <section id="agent-team-tasks-panel" role="tabpanel" aria-labelledby="agent-team-tasks-tab" hidden={view !== 'tasks'}>
              {view === 'tasks' && <AgentTeamTasks
                tasks={visibleSnapshot.tasks}
                members={visibleSnapshot.members}
                pending={team.pending}
                readOnly={readOnly}
                onCreate={team.createTask}
                onReplace={team.replaceTask}
                onDelete={team.deleteTask}
                t={t}
              />}
            </section>
            <section id="agent-team-mailbox-panel" role="tabpanel" aria-labelledby="agent-team-mailbox-tab" hidden={view !== 'mailbox'}>
              {view === 'mailbox' && <AgentTeamMailbox
                snapshot={visibleSnapshot}
                pending={team.pending}
                readOnly={readOnly}
                initialRecipient={messageRecipient}
                onSend={team.sendMessage}
                onMarkRead={team.markMessageRead}
                t={t}
              />}
            </section>
          </>}
        </div>
      </DialogContent>
    </Dialog>
  </AgentTeamOpenContext.Provider>
}
