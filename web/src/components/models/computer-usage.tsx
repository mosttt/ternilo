import * as React from 'react'
import { RefreshCw } from 'lucide-react'
import { api } from '@/api/client'
import { navigate, useSearch } from '@/app/navigation'
import { Button } from '@/components/ui/button'
import { Field, Input, Label } from '@/components/ui/field'
import { useTranslate } from '@/i18n/provider'
import { useWorkbench } from '@/state/workbench'
import type { InputAuthor } from '@/types'
import type { ModelUsage } from './model-service-api'
import { ModelDirectory, errorMessage, useModelDate, useModelPage } from './model-service-ui'
import { SourcePicker, modelCenterPath } from './model-sources'
import { ComputerModelUsage } from './computer-model-usage'
import { ComputerUsageSummary } from './computer-usage-summary'
import css from './model-service.module.css'

interface Computer {
  executor_id: string
  state: string
  connected: boolean
  management: { name: string }
}
interface Observation {
  session_id: string
  session_title: string
  run_id: string
  started_seq: number
  started_at_ms: number
  step: number
  attempt: number
  route: { provider: string; model: string; protocol: string }
  input_author: InputAuthor | null
  finished_at_ms: number | null
  usage: ModelUsage | null
  error_code: string | null
  upstream_request_id: string | null
}

export function ComputerUsage() {
  const { tenants, serverIdentity } = useWorkbench()
  const t = useTranslate('modelService')
  const search = useSearch()
  const parameters = new URLSearchParams(search)
  const tenantId = parameters.get('space') || serverIdentity?.personal_tenant_id || ''
  const tenant = tenants.find(value => value.tenant_id === tenantId)
  const ownerId = serverIdentity?.user.user_id ?? ''
  const scope = JSON.stringify([ownerId, tenantId])
  const [loaded, setLoaded] = React.useState<{ scope: string; computers: Computer[] } | null>(null)
  const [error, setError] = React.useState('')
  const [loading, setLoading] = React.useState(true)
  const [revision, reload] = React.useReducer(value => value + 1, 0)
  const [month, setMonth] = React.useState(() => new Date().toISOString().slice(0, 7))
  const [forwardedOpen, setForwardedOpen] = React.useState(false)
  const id = React.useId()
  const computers = tenant && loaded?.scope === scope ? loaded.computers : []
  const selectedId = parameters.get('computer') || computers[0]?.executor_id || ''
  const selected = computers.find(value => value.executor_id === selectedId)
  React.useEffect(() => {
    setError(''); setLoading(true)
    if (!tenant) { setLoading(false); return }
    const controller = new AbortController()
    void api.request<{ executors: Computer[] }>(`/tenants/${encodeURIComponent(tenantId)}/my-computers`, { headers: { 'x-ternilo-tenant': tenantId }, signal: controller.signal })
      .then(value => { if (!controller.signal.aborted) setLoaded({ scope, computers: value.executors }) })
      .catch(cause => { if (!controller.signal.aborted) setError(errorMessage(cause)) })
      .finally(() => { if (!controller.signal.aborted) setLoading(false) })
    return () => controller.abort()
  }, [scope, tenantId, Boolean(tenant), revision])
  return <section className={css.page} data-computer-usage="">
    <p className={css.hint}>{t('computerUsageDescription')}</p>
    <div className={css.sourceSelectors}>
      <SourcePicker label={t('sourceSpace')} value={tenant ? tenantId : ''} options={tenants.map(value => ({ id: value.tenant_id, name: value.kind === 'personal' ? t('personalSpace') : value.display_name }))} onChange={space => navigate(modelCenterPath({ space, computer: '' }, search))} />
      <Button type="button" variant="outline" disabled={loading} onClick={reload} aria-label={t('refresh')}><RefreshCw className={loading ? css.spinner : ''} /></Button>
    </div>
    {error && <p role="alert">{error}</p>}
    {loading && !computers.length && <p role="status">{t('loading')}</p>}
    {!loading && !computers.length && <p className={css.state}>{t('computerUsageNoComputers')}</p>}
    {tenant && <div className={css.fields}>
      {computers.length > 0 && <SourcePicker label={t('computerUsageComputer')} value={selected?.executor_id ?? ''} options={computers.map(value => ({ id: value.executor_id, name: `${value.management.name} · ${t(value.connected ? 'computerOnline' : 'computerOffline')}` }))} onChange={computer => navigate(modelCenterPath({ computer }, search))} />}
      <Field><Label htmlFor={id}>{t('computerUsageMonth')}</Label><Input id={id} type="month" min="1970-01" max="9999-12" value={month} onChange={event => setMonth(event.target.value)} /></Field>
    </div>}
    {selected && month && <ComputerUsageRecords key={`${scope}:${selected.executor_id}:${month}:${revision}`} tenantId={tenantId} computer={selected} month={month} />}
    {tenant && <details open={forwardedOpen} onToggle={event => setForwardedOpen(event.currentTarget.open)}>
      <summary>{t('forwardedUsageTitle')}</summary>
      {forwardedOpen && month && <ComputerModelUsage key={`${scope}:${month}:${revision}`} tenantId={tenantId} month={month} />}
    </details>}
  </section>
}

function ComputerUsageRecords({ tenantId, computer, month }: { tenantId: string; computer: Computer; month: string }) {
  const t = useTranslate('modelService')
  const date = useModelDate()
  const records = useModelPage<Observation>(`/model-computers/${encodeURIComponent(computer.executor_id)}/usage?month=${encodeURIComponent(month)}`, 'observations', tenantId)
  const author = (value: InputAuthor | null) => value?.kind === 'account' ? value.username || value.user_id
    : t(value?.kind === 'local' ? 'computerUsageLocalActor' : value?.kind === 'automation' ? 'computerUsageAutomationActor' : 'computerUsageUnknownActor')
  return <>
    {!computer.connected && <p className={css.notice}>{t('computerUsageOffline')}</p>}
    <ComputerUsageSummary key={records.query} tenantId={tenantId} computer={computer} month={month} query={records.query} revision={records.revision} />
    <ModelDirectory state={records} label={t('computerUsageRecords')} empty={t('computerUsageEmpty')}>
      <div className={css.list}>{records.items.map(record => <article className={css.row} key={`${record.session_id}:${record.started_seq}`} data-computer-usage-record={`${record.session_id}:${record.started_seq}`}>
        <div className={css.identity}>
          <strong>{record.route.provider} / {record.route.model}</strong><span className={css.badge}>{t('computerUsageReported')}</span>
          <p>{record.session_title} · {date(record.started_at_ms)} · {t('attempt', { number: record.attempt })}</p>
          <p>{t('actor', { id: author(record.input_author) })} · {t(record.finished_at_ms === null ? 'computerUsageIncomplete' : record.error_code ? 'failed' : 'completed')}</p>
          <dl className={css.facts}>{([
            ['input_tokens', 'inputTokens'], ['output_tokens', 'outputTokens'], ['cached_input_tokens', 'cacheReadTokens'],
            ['cache_write_tokens', 'cacheWriteTokens'], ['reasoning_tokens', 'reasoningTokens'],
          ] as const).map(([field, label]) => <div key={field}><dt>{t(label)}</dt><dd>{record.usage?.[field]?.toLocaleString() ?? t('notReported')}</dd></div>)}</dl>
        </div>
      </article>)}</div>
    </ModelDirectory>
  </>
}
