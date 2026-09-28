import * as React from 'react'
import { api } from '@/api/client'
import { Button } from '@/components/ui/button'
import { useTranslate } from '@/i18n/provider'
import { modelAccessPath, modelAdminPath, type ModelServiceRequest, type ModelUsageReport } from './model-service-api'
import { ModelDirectory, errorMessage, useModelDate, useModelPage } from './model-service-ui'
import css from './model-service.module.css'

export function ModelUsage({ admin = false, source }: { admin?: boolean; source?: 'user_provider' | 'platform_grant' }) {
  const t = useTranslate('modelService')
  const date = useModelDate()
  const errorLabels: Record<string, string> = {
    access_denied: t('errorAccessDenied'),
    quota_exceeded: t('errorQuotaExceeded'),
    model_busy: t('errorModelBusy'),
    cancelled: t('errorCancelled'),
    request_expired: t('errorExpired'),
    stream_interrupted: t('errorStreamInterrupted'),
    model_configuration_changed: t('errorConfigurationChanged'),
    invalid_request: t('errorInvalidRequest'),
    request_conflict: t('errorRequestConflict'),
    upstream_failed: t('errorUpstreamFailed'),
  }
  const path = admin ? modelAdminPath : modelAccessPath
  const filter = source ? `?source=${source}` : ''
  const directory = useModelPage<ModelServiceRequest>(`${path}/requests${filter}`, 'requests')
  const [report, setReport] = React.useState<ModelUsageReport | null>(null)
  const [error, setError] = React.useState('')
  const [revision, reload] = React.useReducer(value => value + 1, 0)
  React.useEffect(() => {
    const controller = new AbortController()
    setError('')
    setReport(null)
    void api.request<ModelUsageReport>(`${path}/usage${filter}`, { signal: controller.signal })
      .then(value => { if (!controller.signal.aborted) setReport(value) })
      .catch(cause => { if (!controller.signal.aborted) setError(errorMessage(cause)) })
    return () => controller.abort()
  }, [path, filter, revision])
  const refresh = () => { reload(); directory.reload() }
  return <div className={css.page}>
    {error && <p role="alert">{error}<Button variant="ghost" onClick={refresh}>{t('retry')}</Button></p>}
    {report && <>
      <p className={css.hint}>{t('usageDescription', { month: report.month })}</p>
      <div className={css.summary}>{([
        ['requestCount', report.request_count], ['usedTokens', report.used_tokens], ['reservedTokens', report.reserved_tokens], ['unknownRequests', report.unknown_requests],
      ] as const).map(([label, value]) => <article key={label}><span>{t(label)}</span><strong>{value.toLocaleString()}</strong></article>)}</div>
      <dl className={css.facts}>{([
        ['inputTokens', report.input_tokens], ['outputTokens', report.output_tokens], ['cacheReadTokens', report.cached_input_tokens], ['cacheWriteTokens', report.cache_write_tokens], ['reasoningTokens', report.reasoning_tokens],
      ] as const).map(([label, value]) => <div key={label}><dt>{t(label)}</dt><dd>{value.toLocaleString()}</dd></div>)}</dl>
    </>}
    <ModelDirectory state={directory} label={t('requests')} onRefresh={refresh}>
      <div className={css.list}>{directory.items.map(request => <article className={css.row} key={request.request_id} data-model-request={request.request_id}>
        <div className={css.identity}><strong>{request.model_id}</strong><span className={css.badge}>{t(request.state)}</span><p>{request.source === 'user_provider' ? t('sourceByok') : t('sourceGrant', { name: request.grant_name ?? request.grant_id ?? '' })} · {date(request.created_at_ms)}</p><p>{t(request.origin === 'workload' ? 'originWorkload' : request.origin === 'client_device' ? 'originClientDevice' : 'originApiKey')}</p><code>{request.request_id}</code>
          <p>{request.accounted_tokens !== null ? t('requestUsage', { count: request.accounted_tokens.toLocaleString() }) : !request.attempted ? t('notAttempted') : request.state === 'pending' ? t('requestReserved', { count: request.attempts.filter(attempt => attempt.accounted_tokens === null).reduce((sum, attempt) => sum + attempt.reserved_tokens, 0).toLocaleString() }) : t('unknown')}</p>
          {request.usage && <p>{t('inputTokens')}: {request.usage.input_tokens ?? t('notReported')} · {t('outputTokens')}: {request.usage.output_tokens ?? t('notReported')} · {t('reasoningTokens')}: {request.usage.reasoning_tokens ?? t('notReported')}</p>}
          {(admin || request.workload) && <p>{t('actor', { id: request.actor_user_id })}{request.resource_owner_user_id && <> · {t('resourceOwner', { id: request.resource_owner_user_id })}</>} · {t('beneficiary', { id: request.model_beneficiary_user_id })}</p>}
          {request.workload && <code>{request.workload.session_id} · {request.workload.run_id}</code>}
          {request.attempts.length > 0 && <details className={css.attempts}><summary>{t('attempts', { count: request.attempts.length })}</summary><p>{t('attemptsDescription')}</p><ol>{request.attempts.map(attempt => <li key={attempt.attempt}>
            <strong>{t('attempt', { number: attempt.attempt })} · {t(attempt.state)}</strong>
            <p>{attempt.accounted_tokens !== null ? t('requestUsage', { count: attempt.accounted_tokens.toLocaleString() }) : !attempt.attempted ? t('notAttempted') : attempt.state === 'pending' ? t('requestReserved', { count: attempt.reserved_tokens.toLocaleString() }) : t('unknown')}</p>
            {attempt.usage && <p>{t('inputTokens')}: {attempt.usage.input_tokens ?? t('notReported')} · {t('outputTokens')}: {attempt.usage.output_tokens ?? t('notReported')} · {t('reasoningTokens')}: {attempt.usage.reasoning_tokens ?? t('notReported')}</p>}
            {attempt.error_code && <code>{errorLabels[attempt.error_code] ?? attempt.error_code}</code>}
          </li>)}</ol></details>}
          {request.error_code && <code>{errorLabels[request.error_code] ?? request.error_code}</code>}
        </div>
      </article>)}</div>
    </ModelDirectory>
  </div>
}
