import * as React from 'react'
import { Bot, ExternalLink, LoaderCircle, Mail, Send, Square, UserRound } from 'lucide-react'
import { addressableSubagentSession, type SubagentView } from '@/domain/observability'
import type { Translate } from '@/i18n/runtime'
import type { AgentTeamSnapshot, LocalSession } from '@/types'
import { Button } from '@/components/ui/button'
import css from './agent-team-panel.module.css'

function memberDepth(memberId: string, snapshot: AgentTeamSnapshot) {
  const members = new Map(snapshot.members.map(member => [member.id, member]))
  const seen = new Set<string>()
  let depth = 0
  let cursor = members.get(memberId)?.parent_id
  while (cursor && !seen.has(cursor)) {
    seen.add(cursor)
    depth += 1
    cursor = members.get(cursor)?.parent_id
  }
  return depth
}

function liveStatusLabel(status: SubagentView['status'], t: Translate<'observability'>) {
  return t(`lineage.status.${status}` as const)
}

function readOnlyReason(subagent: SubagentView, t: Translate<'observability'>) {
  if (subagent.supportsFollowup === false) return t('team.readOnly.oneShot')
  if (subagent.supportsFollowup === undefined) return t('team.readOnly.unaddressable')
  return t('team.readOnly.settled')
}

function memberSession(memberId: string | null | undefined, live: SubagentView | undefined, sessions: readonly LocalSession[]) {
  if (!memberId) return null
  if (!live?.sessionId) return sessions.find(session => session.subagent?.subagent_id === memberId) ?? null
  if (live.transcriptKind !== 'conversation') return null
  return addressableSubagentSession(live, sessions)
}

export function AgentTeamRoster({
  snapshot,
  subagents,
  sessions,
  selectedMemberId,
  pendingAction,
  readOnly = false,
  allowStop = !readOnly,
  onOpenSession,
  onFollowup,
  onStop,
  onMessage,
  t,
}: {
  snapshot: AgentTeamSnapshot
  subagents: readonly SubagentView[]
  sessions: readonly LocalSession[]
  selectedMemberId: string | null
  pendingAction: string | null
  readOnly?: boolean
  allowStop?: boolean
  onOpenSession(id: string): void
  onFollowup(subagentId: string, message: string): Promise<boolean>
  onStop(subagentId: string): Promise<boolean>
  onMessage(memberId: string): void
  t: Translate<'observability'>
}) {
  const liveById = React.useMemo(() => new Map(subagents.map(member => [member.id, member])), [subagents])
  const memberNames = React.useMemo(() => new Map(snapshot.members.map(member => [member.id, member.label])), [snapshot.members])
  const [drafts, setDrafts] = React.useState<Record<string, string>>({})
  const [errors, setErrors] = React.useState<Record<string, string>>({})
  const selectedRef = React.useRef<HTMLElement>(null)

  React.useEffect(() => {
    if (!selectedMemberId) return
    const frame = requestAnimationFrame(() => selectedRef.current?.scrollIntoView?.({ block: 'nearest' }))
    return () => cancelAnimationFrame(frame)
  }, [selectedMemberId])

  const act = async (key: string, operation: () => Promise<boolean>) => {
    setErrors(current => ({ ...current, [key]: '' }))
    try { return await operation() }
    catch (cause) {
      setErrors(current => ({ ...current, [key]: cause instanceof Error ? cause.message : String(cause) }))
      return false
    }
  }

  return <div className={css.roster} role="list" aria-label={t('team.roster')}>
    {snapshot.members.map(member => {
      const live = member.subagent_id ? liveById.get(member.subagent_id) : undefined
      const addressed = memberSession(member.subagent_id, live, sessions)
      const current = member.id === snapshot.current_member_id
      const selected = member.id === selectedMemberId
      const canFollowup = !readOnly && live?.supportsFollowup === true && (live.status === 'idle' || live.status === 'failed')
      const canStop = allowStop && live?.status === 'running'
      const followupKey = `followup:${live?.id ?? member.id}`
      const stopKey = `stop:${live?.id ?? member.id}`
      const draft = drafts[member.id] ?? ''
      const parentName = member.parent_id ? memberNames.get(member.parent_id) : undefined
      const assigned = snapshot.tasks.filter(task => task.owner === member.id && task.status !== 'completed' && task.status !== 'cancelled')
      return <article
        ref={selected ? selectedRef : undefined}
        className={css.member}
        data-agent-team-member={member.subagent_id ?? member.id}
        data-status={live?.status ?? (current ? 'current' : 'available')}
        data-selected={selected || undefined}
        style={{ '--team-member-depth': memberDepth(member.id, snapshot) } as React.CSSProperties}
        role="listitem"
        key={member.id}
      >
        <header className={css.memberHeader}>
          <span className={css.avatar}>{member.role === 'lead' ? <UserRound aria-hidden="true" /> : <Bot aria-hidden="true" />}</span>
          <span className={css.identity}>
            <strong>{member.label}{current ? <em>{t('team.member.you')}</em> : null}</strong>
            <small>{member.role === 'lead' ? t('team.role.lead') : member.provider || t('team.providerUnknown')}</small>
          </span>
          <span className={css.dot} data-status={live?.status ?? (current ? 'current' : 'available')} aria-hidden="true" />
          <span className={css.status}>{live ? liveStatusLabel(live.status, t) : current ? t('team.member.current') : t('team.member.available')}</span>
        </header>
        {parentName && <div className={css.memberMeta}>{t('team.member.reportsTo', { name: parentName })}</div>}
        <section className={css.fact}>
          <strong>{t('team.member.assignment')}</strong>
          <p>{assigned.length ? assigned.map(task => task.subject).join(' · ') : t('team.member.unassigned')}</p>
        </section>
        {live?.task && <section className={css.fact}>
          <strong>{t('team.task')}</strong>
          <p>{live.task}</p>
        </section>}
        {live?.output && <section className={css.fact} data-agent-team-output="">
          <strong>{t('team.output')}</strong>
          <pre>{live.output}</pre>
        </section>}
        {live?.error && <section className={`${css.fact} ${css.failure}`} data-agent-team-error="">
          <strong>{t('team.error')}</strong>
          <pre>{live.error}</pre>
        </section>}
        <div className={css.memberActions}>
          {!readOnly && !current && <Button type="button" size="sm" variant="ghost" onClick={() => onMessage(member.id)}>
            <Mail aria-hidden="true" />{t('team.messageMember')}
          </Button>}
          {addressed && <Button type="button" size="sm" variant="ghost" onClick={() => onOpenSession(addressed.identity.session_id)}>
            <ExternalLink aria-hidden="true" />{t('team.openSession')}
          </Button>}
          {member.role === 'subagent' && !addressed && <span>{t(!live || !live.sessionId || live.transcriptKind !== 'conversation' ? 'team.noTranscript' : 'subagent.sessionUnavailable')}</span>}
        </div>
        {canFollowup && live && <div className={css.followup} data-agent-team-followup="">
          <label htmlFor={`agent-followup-${member.id}`}>{t('team.followup')}</label>
          <textarea
            id={`agent-followup-${member.id}`}
            rows={2}
            value={draft}
            disabled={pendingAction !== null}
            placeholder={t('team.followupPlaceholder')}
            onChange={event => setDrafts(currentDrafts => ({ ...currentDrafts, [member.id]: event.target.value }))}
          />
          <Button
            type="button"
            size="sm"
            disabled={pendingAction !== null || !draft.trim()}
            onClick={() => {
              const message = draft.trim()
              if (!message) return
              void act(followupKey, async () => {
                const sent = await onFollowup(live.id, message)
                if (sent) setDrafts(currentDrafts => ({ ...currentDrafts, [member.id]: '' }))
                return sent
              })
            }}
          >
            {pendingAction === followupKey ? <LoaderCircle className={css.spin} aria-hidden="true" /> : <Send aria-hidden="true" />}
            {pendingAction === followupKey ? t('team.sending') : t('team.send')}
          </Button>
        </div>}
        {canStop && live && <div className={css.stopRow}>
          {live.supportsFollowup === false && <span>{t('team.readOnly.oneShot')}</span>}
          <Button type="button" size="sm" variant="outline" data-agent-team-stop="" disabled={pendingAction !== null} onClick={() => void act(stopKey, () => onStop(live.id))}>
            {pendingAction === stopKey ? <LoaderCircle className={css.spin} aria-hidden="true" /> : <Square aria-hidden="true" />}
            {pendingAction === stopKey ? t('team.stopping') : t('team.stop')}
          </Button>
        </div>}
        {live && !canFollowup && !canStop && <p className={css.readOnly} data-agent-team-read-only="">{readOnly ? t('team.readOnly.viewer') : readOnlyReason(live, t)}</p>}
        {errors[followupKey] || errors[stopKey] ? <p className={css.actionError} role="alert">{errors[followupKey] || errors[stopKey]}</p> : null}
      </article>
    })}
  </div>
}
