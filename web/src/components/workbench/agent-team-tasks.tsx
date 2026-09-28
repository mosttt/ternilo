import * as React from 'react'
import { Check, CirclePlus, LoaderCircle, Pencil, Trash2, X } from 'lucide-react'
import type { Translate } from '@/i18n/runtime'
import type {
  AgentTeamMember, AgentTeamTask, AgentTeamTaskDraft, AgentTeamTaskStatus,
} from '@/types'
import { Button } from '@/components/ui/button'
import { Field, Input, Label, Select, Textarea } from '@/components/ui/field'
import css from './agent-team-panel.module.css'

const statuses: AgentTeamTaskStatus[] = ['pending', 'in_progress', 'blocked', 'completed', 'cancelled']

function statusLabel(status: AgentTeamTaskStatus, t: Translate<'observability'>) {
  switch (status) {
    case 'pending': return t('team.tasks.status.pending')
    case 'in_progress': return t('team.tasks.status.in_progress')
    case 'blocked': return t('team.tasks.status.blocked')
    case 'completed': return t('team.tasks.status.completed')
    case 'cancelled': return t('team.tasks.status.cancelled')
  }
}

function emptyDraft(): AgentTeamTaskDraft {
  return { subject: '', description: '', status: 'pending', dependencies: [], owner: null }
}

function taskDraft(task: AgentTeamTask): AgentTeamTaskDraft {
  return {
    subject: task.subject,
    description: task.description,
    status: task.status,
    dependencies: task.dependencies,
    owner: task.owner ?? null,
  }
}

function TaskForm({
  task,
  tasks,
  members,
  busy,
  onCancel,
  onSave,
  t,
}: {
  task?: AgentTeamTask
  tasks: readonly AgentTeamTask[]
  members: readonly AgentTeamMember[]
  busy: boolean
  onCancel(): void
  onSave(draft: AgentTeamTaskDraft): Promise<boolean>
  t: Translate<'observability'>
}) {
  const [draft, setDraft] = React.useState<AgentTeamTaskDraft>(() => task ? taskDraft(task) : emptyDraft())
  const [validation, setValidation] = React.useState('')
  const availableDependencies = tasks.filter(candidate => candidate.id !== task?.id)

  React.useEffect(() => {
    if (!task) return
    setDraft(taskDraft(task))
    setValidation('')
  }, [task?.revision])

  const submit = async (event: React.FormEvent) => {
    event.preventDefault()
    if (!draft.subject.trim()) {
      setValidation(t('team.tasks.subjectRequired'))
      return
    }
    setValidation('')
    const saved = await onSave({ ...draft, subject: draft.subject.trim(), description: draft.description.trim() })
    if (saved) onCancel()
  }

  return <form className={css.taskForm} onSubmit={event => void submit(event)} data-agent-team-task-form={task ? 'edit' : 'create'}>
    <div className={css.taskFormGrid}>
      <Field>
        <Label htmlFor={`team-task-subject-${task?.id ?? 'new'}`}>{t('team.tasks.subject')}</Label>
        <Input id={`team-task-subject-${task?.id ?? 'new'}`} autoFocus value={draft.subject} disabled={busy} onChange={event => setDraft(current => ({ ...current, subject: event.target.value }))} />
      </Field>
      <Field>
        <Label htmlFor={`team-task-status-${task?.id ?? 'new'}`}>{t('team.tasks.status')}</Label>
        <Select id={`team-task-status-${task?.id ?? 'new'}`} value={draft.status} disabled={busy} onValueChange={nextValue => setDraft(current => ({ ...current, status: nextValue as AgentTeamTaskStatus }))}>
          {statuses.map(status => <option key={status} value={status}>{statusLabel(status, t)}</option>)}
        </Select>
      </Field>
      <Field className={css.taskDescriptionField}>
        <Label htmlFor={`team-task-description-${task?.id ?? 'new'}`}>{t('team.tasks.description')}</Label>
        <Textarea id={`team-task-description-${task?.id ?? 'new'}`} rows={3} value={draft.description} disabled={busy} onChange={event => setDraft(current => ({ ...current, description: event.target.value }))} />
      </Field>
      <Field>
        <Label htmlFor={`team-task-owner-${task?.id ?? 'new'}`}>{t('team.tasks.owner')}</Label>
        <Select id={`team-task-owner-${task?.id ?? 'new'}`} value={draft.owner ?? ''} disabled={busy} onValueChange={nextValue => setDraft(current => ({ ...current, owner: nextValue || null }))}>
          <option value="">{t('team.tasks.ownerUnassigned')}</option>
          {members.map(member => <option key={member.id} value={member.id}>{member.label}</option>)}
        </Select>
      </Field>
    </div>
    <fieldset className={css.dependencies} disabled={busy || availableDependencies.length === 0}>
      <legend>{t('team.tasks.dependencies')}</legend>
      {availableDependencies.length === 0
        ? <span>{t('team.tasks.noDependencies')}</span>
        : availableDependencies.map(candidate => <label key={candidate.id}>
          <input
            type="checkbox"
            checked={draft.dependencies.includes(candidate.id)}
            onChange={event => setDraft(current => ({
              ...current,
              dependencies: event.target.checked
                ? [...current.dependencies, candidate.id]
                : current.dependencies.filter(id => id !== candidate.id),
            }))}
          />
          <span>{candidate.subject}</span>
        </label>)}
    </fieldset>
    {validation && <p className={css.actionError} role="alert">{validation}</p>}
    <div className={css.formActions}>
      <Button type="button" size="sm" variant="ghost" disabled={busy} onClick={onCancel}><X />{t('team.cancel')}</Button>
      <Button type="submit" size="sm" disabled={busy || !draft.subject.trim()}>
        {busy ? <LoaderCircle className={css.spin} aria-hidden="true" /> : <Check aria-hidden="true" />}
        {busy ? t('team.saving') : t('team.save')}
      </Button>
    </div>
  </form>
}

export function AgentTeamTasks({
  tasks,
  members,
  pending,
  readOnly = false,
  onCreate,
  onReplace,
  onDelete,
  t,
}: {
  tasks: readonly AgentTeamTask[]
  members: readonly AgentTeamMember[]
  pending: string | null
  readOnly?: boolean
  onCreate(draft: AgentTeamTaskDraft): Promise<boolean>
  onReplace(task: AgentTeamTask, draft: AgentTeamTaskDraft): Promise<boolean>
  onDelete(task: AgentTeamTask): Promise<boolean>
  t: Translate<'observability'>
}) {
  const [creating, setCreating] = React.useState(false)
  const [editing, setEditing] = React.useState<string | null>(null)
  const [confirmingDelete, setConfirmingDelete] = React.useState<string | null>(null)
  const memberNames = React.useMemo(() => new Map(members.map(member => [member.id, member.label])), [members])
  const taskNames = React.useMemo(() => new Map(tasks.map(task => [task.id, task.subject])), [tasks])
  const busy = pending !== null

  return <div className={css.tasksView} data-agent-team-tasks="">
    <div className={css.viewIntro}>
      <div><strong>{t('team.tasks.title')}</strong><p>{t('team.tasks.descriptionText')}</p></div>
      {!readOnly && <Button type="button" size="sm" disabled={busy || creating} onClick={() => { setEditing(null); setCreating(true) }}><CirclePlus />{t('team.tasks.create')}</Button>}
    </div>
    {!readOnly && creating && <TaskForm tasks={tasks} members={members} busy={pending === 'create'} onCancel={() => setCreating(false)} onSave={onCreate} t={t} />}
    {!tasks.length && !creating && <div className={css.emptyState}><strong>{t('team.tasks.empty')}</strong><p>{t('team.tasks.emptyHint')}</p></div>}
    <div className={css.taskList} role="list" aria-label={t('team.tasks.list')}>
      {tasks.map(task => !readOnly && editing === task.id
        ? <TaskForm key={task.id} task={task} tasks={tasks} members={members} busy={pending === 'replace'} onCancel={() => setEditing(null)} onSave={draft => onReplace(task, draft)} t={t} />
        : <article className={css.taskCard} data-agent-team-task="" data-status={task.status} role="listitem" key={task.id}>
          <header>
            <span className={css.taskStatus}>{statusLabel(task.status, t)}</span>
            <strong>{task.subject}</strong>
            {!readOnly && <div className={css.taskActions}>
              <Button type="button" size="icon-xs" variant="ghost" aria-label={t('team.tasks.edit', { name: task.subject })} disabled={busy} onClick={() => { setCreating(false); setEditing(task.id) }}><Pencil /></Button>
              <Button type="button" size="icon-xs" variant="ghost" aria-label={t('team.tasks.delete', { name: task.subject })} disabled={busy} onClick={() => setConfirmingDelete(task.id)}><Trash2 /></Button>
            </div>}
          </header>
          {task.description && <p className={css.taskDescription}>{task.description}</p>}
          <dl className={css.taskFacts}>
            <div><dt>{t('team.tasks.owner')}</dt><dd>{task.owner ? memberNames.get(task.owner) ?? t('team.tasks.ownerUnknown') : t('team.tasks.ownerUnassigned')}</dd></div>
            <div><dt>{t('team.tasks.dependencies')}</dt><dd>{task.dependencies.length ? task.dependencies.map(id => taskNames.get(id)).filter(Boolean).join(' · ') : t('team.tasks.none')}</dd></div>
          </dl>
          {!readOnly && confirmingDelete === task.id && <div className={css.deleteConfirm} role="alert">
            <span>{t('team.tasks.deleteConfirm', { name: task.subject })}</span>
            <Button type="button" size="xs" variant="ghost" disabled={busy} onClick={() => setConfirmingDelete(null)}>{t('team.cancel')}</Button>
            <Button type="button" size="xs" variant="destructive" disabled={busy} onClick={() => void onDelete(task).then(deleted => { if (deleted) setConfirmingDelete(null) })}>
              {pending === 'delete' && <LoaderCircle className={css.spin} aria-hidden="true" />}{t('team.delete')}
            </Button>
          </div>}
        </article>)}
    </div>
  </div>
}
