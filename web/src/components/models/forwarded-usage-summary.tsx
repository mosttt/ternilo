import * as React from 'react'
import { Download } from 'lucide-react'
import { api } from '@/api/client'
import { Button } from '@/components/ui/button'
import { useTranslate } from '@/i18n/provider'
import { downloadUsageReport } from './computer-usage-export'
import { forwardedUsageCsv, type ForwardedSummary } from './forwarded-usage-export'
import { errorMessage, useModelDate } from './model-service-ui'
import css from './model-service.module.css'

export function ForwardedUsageSummary({ tenantId, month, query, revision }: { tenantId: string; month: string; query: string; revision: number }) {
  const t = useTranslate('modelService'), date = useModelDate()
  const [report, setReport] = React.useState<ForwardedSummary | null>(null), [error, setError] = React.useState('')
  const [retry, reload] = React.useReducer(value => value + 1, 0)
  React.useEffect(() => {
    const controller = new AbortController(); setReport(null); setError('')
    const parameters = new URLSearchParams({ month, query })
    void api.request<ForwardedSummary>(`/computer-model-requests/summary?${parameters}`, { signal: controller.signal, headers: { 'x-ternilo-tenant': tenantId } })
      .then(value => { if (!controller.signal.aborted) setReport(value) })
      .catch(cause => { if (!controller.signal.aborted) setError(errorMessage(cause)) })
    return () => controller.abort()
  }, [tenantId, month, query, revision, retry])
  return <section data-forwarded-usage-summary="">
    <p className={css.hint}>{t('forwardedUsageTotals')}</p>
    {error ? <p role="alert">{error}<Button variant="outline" onClick={reload}>{t('retry')}</Button></p>
      : !report ? <p role="status">{t('loading')}</p> : <>
        <div className={css.summary}>{([
          ['requests', report.totals.requests.toLocaleString()], ['computerUsageMonthlyAttempts', report.totals.usage.attempts.toLocaleString()],
          ['inputTokens', report.totals.usage.input.tokens?.toLocaleString() ?? t('notReported')], ['outputTokens', report.totals.usage.output.tokens?.toLocaleString() ?? t('notReported')],
        ] as const).map(([label, value]) => <article key={label}><span>{t(label)}</span><strong>{value}</strong></article>)}</div>
        <p className={css.hint}>{t('computerUsageCoverage', { input: report.totals.usage.input.reported_attempts, output: report.totals.usage.output.reported_attempts, total: report.totals.usage.attempts, time: date(report.observed_at_ms) })}</p>
        <Button type="button" variant="outline" onClick={() => { try { downloadUsageReport(forwardedUsageCsv(report, tenantId), `ternilo-forwarded-usage-${tenantId}-${report.period}.csv`) } catch (cause) { setError(errorMessage(cause)) } }}><Download />{t('forwardedUsageExport')}</Button>
      </>}
  </section>
}
