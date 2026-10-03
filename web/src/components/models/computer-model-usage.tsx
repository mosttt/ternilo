import { ForwardedUsageSummary } from './forwarded-usage-summary'
import { useTranslate } from '@/i18n/provider'
import type { ModelUsage } from './model-service-api'
import { ModelDirectory, useModelDate, useModelPage } from './model-service-ui'
import css from './model-service.module.css'

interface ForwardedRequest {
  request_id: string
  session_id: string
  execution_computer_name: string
  source_computer_name: string
  actor_user_id: string
  model_owner_user_id: string
  resource_owner_user_id: string
  snapshot: { display_name: string }
  state: 'pending' | 'completed' | 'failed' | 'cancelled'
  error_code: string | null
  created_at_ms: number
  attempts: Array<{ attempt: number; report: { usage: ModelUsage | null; error_code: string | null; http_status: number | null } | null }>
}

export function ComputerModelUsage({ tenantId, month }: { tenantId: string; month: string }) {
  const t = useTranslate('modelService')
  const date = useModelDate()
  const page = useModelPage<ForwardedRequest>(`/computer-model-requests?month=${encodeURIComponent(month)}`, 'requests', tenantId)
  return <div data-forwarded-model-usage="">
    <p className={css.hint}>{t('forwardedUsageDescription')}</p>
    <ForwardedUsageSummary key={page.query} tenantId={tenantId} month={month} query={page.query} revision={page.revision} />
    <ModelDirectory state={page} label={t('requests')}>
      <div className={css.list}>{page.items.map(request => <article className={css.row} key={request.request_id} data-forwarded-model-request={request.request_id}>
        <div className={css.identity}>
          <strong>{request.snapshot.display_name}</strong><span className={css.badge}>{t(request.state)}</span>
          <p>{t('forwardedExecution', { name: request.execution_computer_name })} · {t('forwardedSource', { name: request.source_computer_name })}</p>
          <p>{date(request.created_at_ms)} · {t('computerUsageReported')}</p>
          <details className={css.attempts}><summary>{t('attempts', { count: request.attempts.length })}</summary>
            <p>{t('actor', { id: request.actor_user_id })} · {t('beneficiary', { id: request.model_owner_user_id })} · {t('resourceOwner', { id: request.resource_owner_user_id })}</p>
            <code>{request.session_id} · {request.request_id}</code>
            <ol>{request.attempts.map(attempt => <li key={attempt.attempt}>
              <strong>{t('attempt', { number: attempt.attempt })}</strong>
              <p>{t('inputTokens')}: {attempt.report?.usage?.input_tokens?.toLocaleString() ?? t('notReported')} · {t('outputTokens')}: {attempt.report?.usage?.output_tokens?.toLocaleString() ?? t('notReported')}</p>
              {attempt.report?.error_code && <code>{attempt.report.error_code}</code>}
            </li>)}</ol>
          </details>
          {request.error_code && <code>{request.error_code}</code>}
        </div>
      </article>)}</div>
    </ModelDirectory>
  </div>
}
