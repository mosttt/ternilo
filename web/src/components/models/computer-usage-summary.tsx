import * as React from 'react'
import { Download } from 'lucide-react'
import { api } from '@/api/client'
import { Button } from '@/components/ui/button'
import { useTranslate } from '@/i18n/provider'
import { downloadUsageCsv, type UsageSummary } from './computer-usage-export'
import { errorMessage, useModelDate } from './model-service-ui'
import css from './model-service.module.css'

export function ComputerUsageSummary({ tenantId, computer, month, query, revision }: {
  tenantId: string; computer: { executor_id: string; management: { name: string } }; month: string; query: string; revision: number
}) {
  const t = useTranslate('modelService'), date = useModelDate()
  const [report, setReport] = React.useState<UsageSummary | null>(null), [error, setError] = React.useState('')
  const [retry, reload] = React.useReducer(value => value + 1, 0)
  React.useEffect(() => {
    const controller = new AbortController(); setReport(null); setError('')
    const parameters = new URLSearchParams({ month, query })
    void api.request<UsageSummary>(`/model-computers/${encodeURIComponent(computer.executor_id)}/usage/summary?${parameters}`, { signal: controller.signal, headers: { 'x-ternilo-tenant': tenantId } })
      .then(value => { if (!controller.signal.aborted) setReport(value) })
      .catch(cause => { if (!controller.signal.aborted) setError(errorMessage(cause)) })
    return () => controller.abort()
  }, [tenantId, computer.executor_id, month, query, revision, retry])
  const total = (field: 'input' | 'output') => report?.totals[field].tokens?.toLocaleString() ?? t('notReported')
  return <section data-computer-usage-summary="">
    <p className={css.hint}>{t('computerUsageMonthTotals')}</p>
    {error ? <p role="alert">{error}<Button variant="outline" onClick={reload}>{t('retry')}</Button></p>
      : !report ? <p role="status">{t('loading')}</p> : <>
        <div className={css.summary}>{([
          ['computerUsageMonthlyAttempts', report.totals.attempts.toLocaleString()], ['inputTokens', total('input')], ['outputTokens', total('output')],
          ['failed', report.totals.failed.toLocaleString()], ['computerUsageIncomplete', (report.totals.attempts - report.totals.completed).toLocaleString()],
        ] as const).map(([label, value]) => <article key={label}><span>{t(label)}</span><strong>{value}</strong></article>)}</div>
        <p className={css.hint}>{t('computerUsageCoverage', { input: report.totals.input.reported_attempts, output: report.totals.output.reported_attempts, total: report.totals.attempts, time: date(report.observed_at_ms) })}</p>
        <Button variant="outline" type="button" onClick={() => { try { downloadUsageCsv(report, computer) } catch (cause) { setError(errorMessage(cause)) } }}><Download />{t('computerUsageExport')}</Button>
      </>}
  </section>
}
