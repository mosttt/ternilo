import { Bot, ExternalLink, LoaderCircle, UsersRound } from 'lucide-react'
import type { SessionLiveActivity } from '@/api/live-client'
import { currentTurnActivity } from '@/domain/turn-activity'
import { buildToolTraces } from '@/domain/events'
import { deriveSubagents } from '@/domain/observability'
import { useTranslate } from '@/i18n/provider'
import type { LocalSession, SessionEvent } from '@/types'
import { useOpenAgentTeam } from './agent-team-panel'
import { useExecutionAvailability } from './execution-availability'
import css from './subagent-activity.module.css'

export function SubagentActivity({ sessionId, events, sessions, activity, onOpenSession }: {
  sessionId: string
  events: readonly SessionEvent[]
  sessions: readonly LocalSession[]
  activity: Readonly<Record<string, SessionLiveActivity>>
  onOpenSession(id: string): void
}) {
  const t = useTranslate('observability')
  const openTeam = useOpenAgentTeam()
  const availability = useExecutionAvailability()
  const published = deriveSubagents(events)
  const children = sessions.filter(session => session.parent_session_id === sessionId && session.subagent)
  const active = children.flatMap(session => {
    const snapshot = published.find(agent => agent.id === session.subagent!.subagent_id)
    const running = activity[session.identity.session_id]?.running ?? snapshot?.status === 'running'
    return running ? [{ id: session.subagent!.subagent_id, label: session.title,
      sessionId: session.subagent!.transcript_kind === 'conversation' ? session.identity.session_id : undefined }] : []
  })
  for (const snapshot of published) {
    if (snapshot.status === 'running' && !children.some(child => child.subagent!.subagent_id === snapshot.id)) {
      active.push({ id: snapshot.id, label: snapshot.label, sessionId: undefined })
    }
  }
  const run = currentTurnActivity([...events])
  const starting = !active.length && [...buildToolTraces([...events]).values()]
    .some(trace => trace.name === 'spawn_agent' && trace.started.run_id === run?.runId && !trace.output)
  if (!active.length && !starting) return null
  if (availability !== 'live') return null
  return <section className={css.root} data-subagent-activity="" aria-label={t('activity.title')}>
    <div className={css.heading} role="status">
      <LoaderCircle className={css.spin} aria-hidden="true" />
      <span>{t(starting ? 'activity.starting' : 'activity.running', { count: active.length })}</span>
      {openTeam && <button type="button" onClick={() => openTeam()}><UsersRound aria-hidden="true" />{t('team.title')}</button>}
    </div>
    {active.map(agent => <button type="button" className={css.member} key={agent.id}
      disabled={!agent.sessionId && !openTeam}
      onClick={() => agent.sessionId ? onOpenSession(agent.sessionId) : openTeam?.(agent.id)}>
      <Bot aria-hidden="true" /><span>{agent.label}</span><ExternalLink aria-hidden="true" />
    </button>)}
  </section>
}
