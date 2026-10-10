import { Bot, CalendarClock, ChevronRight, ExternalLink, UsersRound } from 'lucide-react'
import { addressableSubagentSession, deriveSubagents, scheduleEventView } from '@/domain/observability'
import { useLocale, useTranslate } from '@/i18n/provider'
import { type BuiltInLocaleId, localeTag } from '@/i18n/runtime'
import type { LocalSession, SessionEvent } from '@/types'
import { useOpenAgentTeam } from '../agent-team-panel'
import css from './observability-event-rows.module.css'

function dateTime(value: number, locale: BuiltInLocaleId) {
  return new Intl.DateTimeFormat(localeTag(locale), {
    dateStyle: 'medium', timeStyle: 'short',
  }).format(new Date(value))
}

function interval(seconds: number, t: ReturnType<typeof useTranslate<'observability'>>) {
  if (seconds % 3_600 === 0) return t('schedule.hours', { hours: seconds / 3_600 })
  if (seconds % 60 === 0) return t('schedule.minutes', { minutes: seconds / 60 })
  return t('schedule.seconds', { seconds })
}

export function ScheduleEventRow({ event, onSelect }: { event: SessionEvent; onSelect(): void }) {
  const t = useTranslate('observability')
  const { locale } = useLocale()
  const view = scheduleEventView(event)
  if (!view) return null
  const operation = t(view.operation === 'create' ? 'schedule.created' : view.operation === 'delete' ? 'schedule.deleted' : 'schedule.dispatched')
  return <article className={css.card} data-schedule-operation={view.operation}>
    <button type="button" className={css.inspect} aria-label={`${t('schedule.title')}: ${operation}`} onClick={onSelect}>
      <span className={css.heading}><CalendarClock aria-hidden="true" /><strong>{t('schedule.title')}</strong><span className={css.badge}>{operation}</span><ChevronRight aria-hidden="true" /></span>
      {view.prompt && <span className={css.body}>{view.prompt}</span>}
      <span className={css.meta}>{t('schedule.id', { id: view.id })}</span>
      {view.operation === 'create' && <span className={css.meta}>
        {view.recurrence === 'every' && view.everySeconds !== undefined
          ? t('schedule.every', { duration: interval(view.everySeconds, t) })
          : t('schedule.once')}
        {view.scheduledAt !== undefined ? ` · ${t('schedule.at', { time: dateTime(view.scheduledAt, locale) })}` : ''}
      </span>}
      {view.operation === 'dispatch' && <span className={css.meta}>
        {view.acceptedAt !== undefined ? t('schedule.accepted', { time: dateTime(view.acceptedAt, locale) }) : ''}
        {view.nextScheduledAt !== undefined ? ` · ${t('schedule.next', { time: dateTime(view.nextScheduledAt, locale) })}` : ''}
      </span>}
    </button>
  </article>
}

export function SubagentEventRow({ event, sessions, onOpenSession, onSelect }: {
  event: SessionEvent
  sessions: readonly LocalSession[]
  onOpenSession(id: string): void
  onSelect(): void
}) {
  const t = useTranslate('observability')
  const openTeam = useOpenAgentTeam()
  const subagent = deriveSubagents([event])[0]
  if (!subagent) return null
  const addressed = addressableSubagentSession(subagent, sessions)
  const hasPublishedConversation = Boolean(subagent.sessionId && subagent.transcriptKind === 'conversation')
  return <article className={css.card} data-subagent-event={subagent.id} data-status={subagent.status}>
    <div className={css.subagentHeading}>
      <div className={css.subagentIdentity}>
        <div className={css.subagentTitle}><Bot aria-hidden="true" /><strong>{t('subagent.title', { name: subagent.label })}</strong></div>
        <span className={css.subagentStatus}><span className={css.dot} data-status={subagent.status} aria-hidden="true" /><span className={css.badge}>{t(`lineage.status.${subagent.status}` as const)}</span></span>
      </div>
      <div className={css.subagentActions}>
        {openTeam && <button type="button" className={css.iconButton} aria-label={t('team.openFor', { name: subagent.label })} onClick={() => openTeam(subagent.id)}><UsersRound aria-hidden="true" /></button>}
        <button type="button" className={css.iconButton} aria-label={t('subagent.title', { name: subagent.label })} onClick={onSelect}><ChevronRight aria-hidden="true" /></button>
      </div>
    </div>
    <p className={css.body}>{subagent.task}</p>
    {subagent.provider && <p className={css.meta}>{t('subagent.provider', { provider: subagent.provider })}</p>}
    {subagent.output && <details className={css.details}><summary>{t('subagent.output')}</summary><pre>{subagent.output}</pre></details>}
    {subagent.error && <details className={`${css.details} ${css.error}`} open><summary>{t('subagent.error')}</summary><pre>{subagent.error}</pre></details>}
    {addressed
      ? <button type="button" className={css.openSession} aria-label={t('subagent.open', { name: subagent.label })} onClick={() => onOpenSession(addressed.identity.session_id)}><ExternalLink aria-hidden="true" />{t('subagent.open', { name: subagent.label })}</button>
      : <p className={css.readOnly}>{t(hasPublishedConversation ? 'subagent.sessionUnavailable' : 'subagent.readOnly')}</p>}
  </article>
}
