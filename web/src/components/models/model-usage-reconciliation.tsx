import * as React from 'react'
import { api, ApiError } from '@/api/client'
import { Button } from '@/components/ui/button'
import { Field, Input, Label, Textarea } from '@/components/ui/field'
import { useTranslate } from '@/i18n/provider'
import { modelAdminPath, type ModelServiceAttempt, type ModelServiceRequest, type ModelUsage } from './model-service-api'
import { ModelModal, errorMessage, useModelDate } from './model-service-ui'
import css from './model-service.module.css'

interface ReconciliationInput {
  expected_settled_at_ms: number
  usage: ModelUsage
  reference: string
  note: string
}
interface Reconciliation {
  attempt: number
  actor_user_id: string
  reconciled_at_ms: number
  input: ReconciliationInput
  previous_usage: ModelUsage | null
}
const counters = [
  ['input_tokens', 'inputTokens'], ['output_tokens', 'outputTokens'], ['cached_input_tokens', 'cacheReadTokens'],
  ['cache_write_tokens', 'cacheWriteTokens'], ['reasoning_tokens', 'reasoningTokens'],
] as const

export function ModelUsageReconciliationDialog({ request, attempt, editable, onSaved, onClose }: {
  request: ModelServiceRequest; attempt?: ModelServiceAttempt; editable: boolean; onSaved(): void; onClose(): void
}) {
  const t = useTranslate('modelService')
  const date = useModelDate()
  const id = React.useId()
  const [values, setValues] = React.useState(() => Object.fromEntries(counters.map(([key]) => [key, attempt?.usage?.[key]?.toString() ?? ''])) as Record<keyof ModelUsage, string>)
  const [reference, setReference] = React.useState('')
  const [note, setNote] = React.useState('')
  const [records, setRecords] = React.useState<Reconciliation[]>([])
  const [loading, setLoading] = React.useState(true)
  const [busy, setBusy] = React.useState(false)
  const [conflicted, setConflicted] = React.useState(false)
  const [error, setError] = React.useState('')
  const [revision, retry] = React.useReducer(value => value + 1, 0)
  React.useEffect(() => {
    const controller = new AbortController()
    setLoading(true); setError('')
    void api.request<Reconciliation[]>(`${modelAdminPath}/requests/${encodeURIComponent(request.request_id)}/reconciliations`, { signal: controller.signal })
      .then(result => { if (!controller.signal.aborted) setRecords(result) })
      .catch(cause => { if (!controller.signal.aborted) setError(errorMessage(cause)) })
      .finally(() => { if (!controller.signal.aborted) setLoading(false) })
    return () => controller.abort()
  }, [request.request_id, revision])
  const reconciled = records.some(record => record.attempt === attempt?.attempt)
  const maySubmit = !conflicted && editable && attempt && request.state !== 'pending' && attempt.state !== 'pending'
    && attempt.attempted && attempt.accounted_tokens === null && attempt.settled_at_ms !== null && !reconciled
  const save = async () => {
    const settledAt = attempt?.settled_at_ms
    if (!maySubmit || busy || settledAt == null) return
    setError('')
    const usage: ModelUsage = { input_tokens: null, output_tokens: null, cached_input_tokens: null, cache_write_tokens: null, reasoning_tokens: null }
    for (const [key] of counters) usage[key] = values[key].trim() === '' ? null : Number(values[key])
    if (usage.input_tokens === null || usage.output_tokens === null
      || Object.values(usage).some(value => value !== null && (!Number.isSafeInteger(value) || value < 0))) {
      setError(t('reconcileInvalidTokens')); return
    }
    setBusy(true)
    try {
      const result = await api.request<{ reconciliation: Reconciliation }>(`${modelAdminPath}/requests/${encodeURIComponent(request.request_id)}/attempts/${attempt.attempt}/reconcile`, {
        method: 'POST', body: { expected_settled_at_ms: settledAt, usage, reference: reference.trim(), note: note.trim() } satisfies ReconciliationInput,
      })
      setRecords(current => [...current.filter(record => record.attempt !== result.reconciliation.attempt), result.reconciliation])
      onSaved()
    } catch (cause) {
      setError(errorMessage(cause))
      if (cause instanceof ApiError && cause.status === 409) { setConflicted(true); onSaved() }
    }
    finally { setBusy(false) }
  }
  return <ModelModal title={t(attempt ? 'reconcileTitle' : 'reconcileRecords')} description={t('reconcileDescription')} busy={busy} onClose={onClose}>
    <p><code>{request.request_id}</code> · {request.model_id} · {request.month}</p>
    {loading ? <p role="status">{t('loading')}</p> : <>
      {records.map(record => <article key={record.attempt} data-usage-reconciliation={record.attempt}>
        <strong>{t('reconciledAttempt', { number: record.attempt })}</strong>
        <p>{t('actor', { id: record.actor_user_id })} · {date(record.reconciled_at_ms)}</p>
        <p>{t('reconcileReference')}: {record.input.reference}</p><p>{record.input.note}</p>
        <dl className={css.facts}>{counters.map(([key, label]) => <div key={key}><dt>{t(label)}</dt><dd>{record.input.usage[key] ?? t('notReported')}</dd></div>)}</dl>
      </article>)}
      {maySubmit && <form className={css.form} onSubmit={event => { event.preventDefault(); void save() }}>
        <p>{t('attempt', { number: attempt.attempt })} · {t('reconcileHint')}</p>
        <fieldset className={css.fields} disabled={busy}>
          {counters.map(([key, label]) => <Field key={key}><Label htmlFor={`${id}-${key}`}>{t(label)}</Label>
            <Input id={`${id}-${key}`} type="number" min={0} max={Number.MAX_SAFE_INTEGER} step={1} required={key === 'input_tokens' || key === 'output_tokens'} value={values[key]} onChange={event => setValues(current => ({ ...current, [key]: event.target.value }))} />
          </Field>)}
        </fieldset>
        <Field><Label htmlFor={`${id}-reference`}>{t('reconcileReference')}</Label><Input id={`${id}-reference`} value={reference} onChange={event => setReference(event.target.value)} required maxLength={512} disabled={busy} /></Field>
        <Field><Label htmlFor={`${id}-note`}>{t('reconcileNote')}</Label><Textarea id={`${id}-note`} value={note} onChange={event => setNote(event.target.value)} required maxLength={1024} disabled={busy} /></Field>
        <div className={css.formFooter}><Button type="button" variant="outline" onClick={onClose} disabled={busy}>{t('cancel')}</Button><Button type="submit" disabled={busy || !reference.trim() || !note.trim()}>{t(busy ? 'saving' : 'reconcileConfirm')}</Button></div>
      </form>}
      {!maySubmit && !records.length && !error && <p>{t('reconcileNone')}</p>}
    </>}
    {error && <p role="alert">{error}<Button type="button" variant="ghost" onClick={retry} disabled={busy}>{t('retry')}</Button></p>}
    {!maySubmit && <div className={css.formFooter}><Button type="button" variant="outline" onClick={onClose}>{t('close')}</Button></div>}
  </ModelModal>
}
