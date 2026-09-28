import * as React from 'react'
import { Check, ChevronDown, ChevronUp, Pencil, Send, Trash2, X } from 'lucide-react'
import type { SessionSubmission } from '@/types'
import { ApiError } from '@/api/client'
import { Button } from '@/components/ui/button'
import type { Translate } from '@/i18n/runtime'
import css from './queue-dock.module.css'
import { SubmissionReferenceChips } from './reference-chips'
import { InputIdentity } from './input-identity'

function preview(item: SessionSubmission) {
  const name = item.content.kind === 'skill' ? item.content.name
    : item.content.kind === 'regenerate' ? item.content.skill_name : undefined
  return name ? `/skill ${name}${item.content.input ? ` ${item.content.input}` : ''}` : item.content.input
}

export function QueueDock({
  items,
  running,
  canRemove = true,
  onEdit,
  onLoad,
  onRemove,
  onSteer,
  onError,
  t,
}: {
  items: SessionSubmission[]
  running: boolean
  canRemove?: boolean
  onEdit(id: string, input: string, expectedUpdatedAtMs: number): Promise<void>
  onLoad(id: string): Promise<SessionSubmission | undefined>
  onRemove(id: string): Promise<void>
  onSteer(id: string): Promise<void>
  onError(message: string): void
  t: Translate<'conversation'>
}) {
  const queued = React.useMemo(() => items.filter(item => item.placement === 'queued'), [items])
  const steering = React.useMemo(() => items.filter(item => item.placement === 'steering'), [items])
  const [collapsed, setCollapsed] = React.useState(false)
  const [editing, setEditing] = React.useState<{
    id: string; input: string; original: SessionSubmission; failed?: boolean; conflict?: boolean
  } | null>(null)
  const [mutating, setMutating] = React.useState<string | null>(null)
  const listId = React.useId()

  React.useEffect(() => {
    if (!queued.length && collapsed) setCollapsed(false)
    if (editing && !editing.failed && mutating === null && !queued.some(item => item.id === editing.id)) setEditing(null)
  }, [collapsed, editing, mutating, queued])

  if (!queued.length && !steering.length && !editing) return null
  const latest = editing ? queued.find(item => item.id === editing.id) : undefined
  const conflict = !!editing && (editing.conflict || !!latest && latest.updated_at_ms !== editing.original.updated_at_ms)
  const rows = editing && !latest ? [...queued, editing.original] : queued
  const interactionActive = editing !== null || mutating !== null
  const expanded = queued.length <= 1 || !collapsed || interactionActive

  const mutate = async (id: string, action: () => Promise<void>) => {
    setMutating(id)
    try { await action() }
    catch (cause) { onError(cause instanceof Error ? cause.message : String(cause)) }
    finally { setMutating(current => current === id ? null : current) }
  }

  const save = async () => {
    if (!editing?.input.trim() || mutating !== null || conflict || !latest) return
    const current = editing
    await mutate(current.id, async () => {
      try {
        await onEdit(current.id, current.input.trim(), current.original.updated_at_ms)
        setEditing(null)
      } catch (cause) {
        setEditing({ ...current, failed: true, conflict: cause instanceof ApiError && cause.status === 409 })
        throw cause
      }
    })
  }

  const loadLatest = async () => {
    if (!editing || mutating !== null) return
    const current = editing
    await mutate(current.id, async () => {
      const loaded = await onLoad(current.id)
      setEditing(loaded
        ? { id: loaded.id, input: loaded.content.input, original: loaded }
        : { ...current, failed: true })
    })
  }

  return (
    <section className={css.root} data-queue-dock aria-label={t('queue.aria')}>
      {queued.length > 1 && (
        <button
          type="button"
          className={css.header}
          aria-expanded={expanded}
          aria-controls={listId}
          disabled={interactionActive}
          onClick={() => setCollapsed(value => !value)}
        >
          <span>{t('queue.count', { n: queued.length })}</span>
          {expanded ? <ChevronDown aria-hidden /> : <ChevronUp aria-hidden />}
        </button>
      )}
      <ul id={listId} className={css.list} hidden={!expanded || !rows.length}>
        {expanded && rows.map(item => (
          <li key={item.id} className={css.row} data-queued-submission={item.id}>
            <div className={css.previewStack}>
              <InputIdentity author={item.provenance?.author} compact />
              {editing?.id === item.id ? (<>
                <input
                  autoFocus
                  className={css.editor}
                  aria-label={t('queue.edit')}
                  value={editing.input}
                  disabled={mutating !== null}
                  onChange={event => setEditing({ ...editing, input: event.target.value })}
                  onKeyDown={event => {
                    if (event.key === 'Escape' && mutating === null) setEditing(null)
                    if (event.key === 'Enter' && !event.nativeEvent.isComposing) {
                      event.preventDefault(); void save()
                    }
                  }}
                />
                {conflict && <span role="alert">{t('queue.conflict')}</span>}
                {!latest && <span role="alert">{t('queue.unavailable')}</span>}
                {conflict && (
                  <Button type="button" size="sm" variant="ghost" disabled={mutating !== null}
                    onClick={() => void loadLatest()}>
                    {t('queue.loadLatest')}
                  </Button>
                )}
              </>
              ) : <>
                {preview(item) && <span className={css.preview}>{preview(item)}</span>}
                <SubmissionReferenceChips references={item.references} compact />
              </>}
            </div>
            <div className={css.actions}>
              {editing?.id === item.id ? (
                <>
                  <Button type="button" size="icon-xs" variant="ghost" aria-label={t('queue.save')} disabled={mutating !== null || !editing.input.trim() || conflict || !latest} onClick={() => void save()}><Check /></Button>
                  <Button type="button" size="icon-xs" variant="ghost" aria-label={t('queue.cancelEdit')} disabled={mutating !== null} onClick={() => setEditing(null)}><X /></Button>
                </>
              ) : (
                <>
                  <Button type="button" size="icon-xs" variant="ghost" aria-label={t('queue.edit')} disabled={interactionActive} onClick={() => setEditing({ id: item.id, input: item.content.input, original: item })}><Pencil /></Button>
                  <Button type="button" size="icon-xs" variant="ghost" aria-label={t('queue.remove')} disabled={mutating !== null || !canRemove} onClick={() => void mutate(item.id, () => onRemove(item.id))}><Trash2 /></Button>
                </>
              )}
            </div>
          </li>
        ))}
      </ul>
      {queued.length > 0 && (
        <Button type="button" size="sm" variant="ghost" className={css.sendBatch} aria-label={t(running ? 'queue.steer' : 'queue.send')} disabled={interactionActive || !canRemove}
          onClick={() => void mutate(queued[0]!.id, () => onSteer(queued[0]!.id))}>
          <Send />{t(running ? 'queue.steer' : 'queue.send')}
        </Button>
      )}
      {steering.map(item => (
        <div key={item.id} className={`${css.row} ${css.steering}`} data-pending-steering>
          <div className={css.previewStack}>
            <InputIdentity author={item.provenance?.author} compact />
            {preview(item) && <span className={css.preview}>{preview(item)}</span>}
            <SubmissionReferenceChips references={item.references} compact />
          </div>
          <span className={css.state}>{t('queue.steered')}</span>
        </div>
      ))}
    </section>
  )
}
