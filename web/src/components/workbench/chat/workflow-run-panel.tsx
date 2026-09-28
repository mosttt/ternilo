import * as React from 'react'
import { ChevronRight, CircleAlert, Workflow } from 'lucide-react'
import type { WorkflowPhaseView, WorkflowRunView, WorkflowStatus } from '@/domain/observability'
import { useTranslate } from '@/i18n/provider'
import type { LocalSession } from '@/types'
import css from './workflow-run-panel.module.css'

function statusLabel(status: WorkflowStatus, t: ReturnType<typeof useTranslate<'observability'>>) {
  const keys = {
    running: 'workflow.status.running',
    completed: 'workflow.status.completed',
    failed: 'workflow.status.failed',
    cancelled: 'workflow.status.cancelled',
  } as const
  return t(keys[status])
}

function memberSummary(phase: WorkflowPhaseView, t: ReturnType<typeof useTranslate<'observability'>>) {
  const counts = new Map<WorkflowStatus, number>()
  for (const member of phase.members) counts.set(member.status, (counts.get(member.status) ?? 0) + 1)
  return (['running', 'failed', 'cancelled', 'completed'] as const)
    .filter(status => counts.has(status))
    .map(status => `${counts.get(status)} ${statusLabel(status, t)}`)
    .join(' · ')
}

export function WorkflowRunPanel({ run, sessions, onOpenSession }: {
  run: WorkflowRunView
  sessions: readonly LocalSession[]
  onOpenSession(id: string): void
}) {
  const t = useTranslate('observability')
  const active = run.status === 'running' || run.status === 'failed' || run.status === 'cancelled'
  const [open, setOpen] = React.useState(active)
  const [openPhases, setOpenPhases] = React.useState<Set<string>>(() => new Set(
    run.phases.filter(phase => phase.members.some(member => member.status !== 'completed')).map(phase => phase.key),
  ))
  const count = run.phases.reduce((total, phase) => total + phase.members.length, 0)

  React.useEffect(() => {
    if (run.status !== 'completed') setOpen(true)
  }, [run.status])

  return <section className={css.root} data-workflow-run={run.id} data-status={run.status}>
    <button type="button" className={css.runHeader} aria-expanded={open} onClick={() => setOpen(value => !value)}>
      <ChevronRight data-open={open || undefined} aria-hidden="true" />
      <Workflow aria-hidden="true" />
      <strong>{t('workflow.title', { name: run.name })}</strong>
      <span className={css.summary}>{t(count === 1 ? 'workflow.members.one' : 'workflow.members.other', { count })}</span>
      <span className={css.status}><span className={css.dot} data-status={run.status} aria-hidden="true" />{statusLabel(run.status, t)}</span>
    </button>
    {open && <div className={css.body}>
      {run.description && <p className={css.description}>{run.description}</p>}
      {run.currentPhase !== null && <p className={css.current}>{t('workflow.currentPhase', { phase: run.currentPhase })}</p>}
      <div className={css.phases}>
        {run.phases.map(phase => {
          const phaseOpen = openPhases.has(phase.key)
          const title = phase.title === null ? t('workflow.phase.unassigned') : phase.title || t('workflow.phase.empty')
          return <section className={css.phase} key={phase.key}>
            <button type="button" className={css.phaseHeader} aria-expanded={phaseOpen} onClick={() => setOpenPhases(current => {
              const next = new Set(current)
              if (phaseOpen) next.delete(phase.key)
              else next.add(phase.key)
              return next
            })}>
              <ChevronRight data-open={phaseOpen || undefined} aria-hidden="true" />
              <strong>{title}</strong>
              <span>{phase.members.length ? memberSummary(phase, t) : phase.detail ?? ''}</span>
            </button>
            {phaseOpen && <div className={css.members} role="list">
              {phase.members.map(member => {
                const addressed = sessions.find(session => session.identity.session_id === member.subagentId && session.archived_at_ms == null)
                const content = <>
                  <span className={css.dot} data-status={member.status} aria-hidden="true" />
                  <span className={css.memberLabel}>{member.label}</span>
                  <span className={css.memberStatus}>{statusLabel(member.status, t)}</span>
                </>
                return addressed
                  ? <button type="button" className={css.member} role="listitem" aria-label={t('workflow.openMember', { name: member.label })} key={member.sequence} onClick={() => onOpenSession(addressed.identity.session_id)}>{content}</button>
                  : <div className={css.member} role="listitem" aria-label={`${member.label}. ${t('workflow.memberReadOnly')}`} key={member.sequence}>{content}</div>
              })}
            </div>}
          </section>
        })}
      </div>
      {run.logs.length > 0 && <details className={css.logs}><summary>{t('workflow.logs')}</summary><ol>{run.logs.map((log, index) => <li key={`${index}-${log}`}>{log}</li>)}</ol></details>}
      {run.error && <p className={css.error}><CircleAlert aria-hidden="true" />{t('workflow.error', { error: run.error })}</p>}
    </div>}
  </section>
}
