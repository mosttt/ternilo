import * as React from 'react'
import {
  Check, CheckCircle2, ChevronDown, ChevronUp, Circle, Goal, LoaderCircle,
  OctagonAlert, Pencil, Play, X,
} from 'lucide-react'
import type { Translate } from '@/i18n/runtime'
import type { SessionProjection } from '@/types'
import css from './composer-projection-dock.module.css'

type Todo = { step: string; status: 'pending' | 'in_progress' | 'completed' }
type GoalSnapshot = { objective: string; status: 'active' | 'blocked' | 'complete' }
type GoalAction = 'edit' | 'resume' | 'complete' | 'blocked'

function projectedTodos(projection: SessionProjection | null): Todo[] {
  const value = projection?.values.todos
  if (!value || typeof value !== 'object' || !Array.isArray((value as { items?: unknown }).items)) return []
  return (value as { items: unknown[] }).items.flatMap(item => {
    if (!item || typeof item !== 'object') return []
    const candidate = item as Partial<Todo>
    if (typeof candidate.step !== 'string' || !['pending', 'in_progress', 'completed'].includes(candidate.status ?? '')) return []
    return [candidate as Todo]
  })
}

function projectedGoal(projection: SessionProjection | null): GoalSnapshot | null {
  const value = projection?.values.goal
  if (!value || typeof value !== 'object') return null
  const candidate = value as Partial<GoalSnapshot>
  if (typeof candidate.objective !== 'string' || !['active', 'blocked', 'complete'].includes(candidate.status ?? '')) return null
  return candidate as GoalSnapshot
}

function TodoGlyph({ status }: { status: Todo['status'] }) {
  if (status === 'completed') return <CheckCircle2 className={css.complete} />
  if (status === 'in_progress') return <LoaderCircle className={css.progressIcon} />
  return <Circle className={css.pending} />
}

export function goalActionCommand(action: GoalAction, objective: string): string {
  return `/goal ${action} ${objective.trim()}`
}

export function ComposerProjectionDock({ projection, busy = false, onGoalCommand, t }: {
  projection: SessionProjection | null
  busy?: boolean
  onGoalCommand?(command: string): Promise<void>
  t: Translate<'conversation'>
}) {
  const todos = projectedTodos(projection)
  const goal = projectedGoal(projection)
  const [expanded, setExpanded] = React.useState(false)
  const [editing, setEditing] = React.useState(false)
  const [draft, setDraft] = React.useState('')
  const [pending, setPending] = React.useState(false)
  const [error, setError] = React.useState('')
  const pendingRef = React.useRef(false)
  const visibleGoal = goal && goal.status !== 'complete' ? goal : null

  React.useEffect(() => {
    setEditing(false)
    setDraft('')
    setError('')
    pendingRef.current = false
    setPending(false)
  }, [visibleGoal?.objective, visibleGoal?.status])

  const runGoalAction = async (action: GoalAction, objective: string) => {
    if (!onGoalCommand || pendingRef.current || busy) return false
    pendingRef.current = true
    setPending(true)
    setError('')
    try {
      await onGoalCommand(goalActionCommand(action, objective))
      return true
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause))
      return false
    } finally {
      pendingRef.current = false
      setPending(false)
    }
  }

  const saveGoal = async () => {
    const objective = draft.trim()
    if (!objective || !visibleGoal) return
    const action = visibleGoal.status === 'blocked' ? 'blocked' : 'edit'
    if (await runGoalAction(action, objective)) setEditing(false)
  }

  if (!visibleGoal && todos.length === 0) return null
  const completed = todos.filter(item => item.status === 'completed').length
  const active = todos.filter(item => item.status === 'in_progress').length
  return (
    <div className={css.stack} data-projection-dock="">
      {visibleGoal && (
        <section className={css.goal} data-status={visibleGoal.status}>
          <Goal />
          {editing ? (
            <>
              <input
                className={css.goalInput}
                value={draft}
                aria-label={t('goal.objective')}
                disabled={pending}
                autoFocus
                onChange={event => setDraft(event.target.value)}
                onKeyDown={event => {
                  if (event.key === 'Enter') void saveGoal()
                  if (event.key === 'Escape') setEditing(false)
                }}
              />
              <div className={css.goalActions}>
                <button type="button" data-goal-action="save" aria-label={t('goal.save')} title={t('goal.save')} disabled={pending || !draft.trim()} onClick={() => void saveGoal()}><Check /></button>
                <button type="button" data-goal-action="cancel" aria-label={t('goal.cancelEdit')} title={t('goal.cancelEdit')} disabled={pending} onClick={() => setEditing(false)}><X /></button>
              </div>
            </>
          ) : (
            <>
              <span className={css.label}>{visibleGoal.status === 'blocked' ? t('goal.blocked') : busy ? t('goal.active') : t('goal.idle')}</span>
              <span className={css.objective}>{visibleGoal.objective}</span>
              {onGoalCommand && (
                <div className={css.goalActions}>
                  {!busy && (
                    <button type="button" data-goal-action="resume" aria-label={t('goal.resume')} title={t('goal.resume')} disabled={pending || busy} onClick={() => void runGoalAction('resume', visibleGoal.objective)}><Play /></button>
                  )}
                  {visibleGoal.status === 'active' && (
                    <button type="button" data-goal-action="blocked" aria-label={t('goal.markBlocked')} title={t('goal.markBlocked')} disabled={pending || busy} onClick={() => void runGoalAction('blocked', visibleGoal.objective)}><OctagonAlert /></button>
                  )}
                  <button type="button" data-goal-action="edit" aria-label={t('goal.edit')} title={t('goal.edit')} disabled={pending || busy} onClick={() => { setDraft(visibleGoal.objective); setEditing(true) }}><Pencil /></button>
                  <button type="button" data-goal-action="complete" aria-label={t('goal.complete')} title={t('goal.complete')} disabled={pending || busy} onClick={() => void runGoalAction('complete', visibleGoal.objective)}><CheckCircle2 /></button>
                </div>
              )}
            </>
          )}
          {error && <span className={css.goalError} role="alert">{error}</span>}
        </section>
      )}
      {todos.length > 0 && (
        <section className={css.todos}>
          <button type="button" className={css.todoHeader} aria-expanded={expanded} onClick={() => setExpanded(value => !value)}>
            <CheckCircle2 />
            <span className={css.label}>{t('todo.title')}</span>
            <span className={css.summary}>{t('todo.summary', { completed, total: todos.length, active })}</span>
            {expanded ? <ChevronUp /> : <ChevronDown />}
          </button>
          {expanded && (
            <ul>
              {todos.map((item, index) => (
                <li key={`${item.step}-${index}`} data-status={item.status}>
                  <TodoGlyph status={item.status} />
                  <span>{item.step}</span>
                </li>
              ))}
            </ul>
          )}
        </section>
      )}
    </div>
  )
}
