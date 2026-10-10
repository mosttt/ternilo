import * as React from 'react'
import { AlertTriangle, LoaderCircle, RefreshCw } from 'lucide-react'
import { ApiError } from '@/api/client'
import { Button } from '@/components/ui/button'
import { Input, Label } from '@/components/ui/field'
import { useLocale, useTranslate } from '@/i18n/provider'
import { localeTag } from '@/i18n/runtime'
import type { Translate } from '@/i18n/runtime'
import type {
  ModelUsageAnomalyKind,
  ModelUsageReport,
  ModelUsageReservation,
} from '@/types'
import { getTenantModelUsage } from './platform-admin-api'
import { GroupHeader } from './settings-ui'
import styles from './platform-settings.module.css'

const REPORT_LIMIT = 200

function currentUtcMonth() {
  return new Date().toISOString().slice(0, 7)
}

function stateLabel(state: string, t: Translate<'settings'>) {
  switch (state) {
    case 'active': return t('usage.state.active')
    case 'committed': return t('usage.state.committed')
    case 'released': return t('usage.state.released')
    case 'expired': return t('usage.state.expired')
    case 'queued': return t('usage.state.queued')
    case 'leased': return t('usage.state.leased')
    case 'running': return t('usage.state.running')
    case 'cancel_requested': return t('usage.state.cancel_requested')
    case 'succeeded': return t('usage.state.succeeded')
    case 'failed': return t('usage.state.failed')
    case 'cancelled': return t('usage.state.cancelled')
    case 'indeterminate': return t('usage.state.indeterminate')
    default: return state
  }
}

function issueLabel(issue: ModelUsageAnomalyKind, t: Translate<'settings'>) {
  switch (issue) {
    case 'expired_active': return t('usage.issue.expired_active')
    case 'terminal_run_active': return t('usage.issue.terminal_run_active')
    case 'missing_committed_tokens': return t('usage.issue.missing_committed_tokens')
    case 'committed_usage_mismatch': return t('usage.issue.committed_usage_mismatch')
    case 'unknown_model_usage': return t('usage.issue.unknown_model_usage')
  }
}

function ReservationIssues({ reservation, t }: {
  reservation: ModelUsageReservation
  t: Translate<'settings'>
}) {
  return reservation.issues.length ? (
    <ul className={styles.usageIssues}>
      {reservation.issues.map(issue => <li key={issue}>{issueLabel(issue, t)}</li>)}
    </ul>
  ) : null
}

export function PlatformUsageSettings({ tenantId }: { tenantId: string }) {
  const t = useTranslate('settings')
  const { locale } = useLocale()
  const [period, setPeriod] = React.useState(currentUtcMonth)
  const [report, setReport] = React.useState<ModelUsageReport | null>(null)
  const [loading, setLoading] = React.useState(true)
  const [error, setError] = React.useState('')
  const requestGeneration = React.useRef(0)

  const load = React.useCallback(async () => {
    const generation = ++requestGeneration.current
    setLoading(true)
    setError('')
    setReport(null)
    try {
      const nextReport = await getTenantModelUsage(tenantId, period, REPORT_LIMIT)
      if (requestGeneration.current === generation) setReport(nextReport)
    } catch (cause) {
      if (requestGeneration.current === generation) {
        setError(cause instanceof ApiError && cause.status === 403
          ? t('platform.permission')
          : t('usage.loadError'))
      }
    } finally {
      if (requestGeneration.current === generation) setLoading(false)
    }
  }, [period, t, tenantId])

  React.useEffect(() => {
    void load()
    return () => { requestGeneration.current += 1 }
  }, [load])

  const number = React.useMemo(() => new Intl.NumberFormat(localeTag(locale)), [locale])
  const time = React.useMemo(() => new Intl.DateTimeFormat(
    localeTag(locale),
    { dateStyle: 'medium', timeStyle: 'short', timeZone: 'UTC' },
  ), [locale])
  const format = (value: number | null | undefined) => value == null ? '—' : number.format(value)
  const hasPeriodData = Boolean(report && (report.totals.requests || report.reservations.length))
  const listState = loading
    ? 'loading'
    : error
      ? 'error'
      : hasPeriodData
        ? 'ready'
        : 'empty'

  return (
    <div data-platform-usage="" data-platform-list-state={listState}>
      <div className={styles.usageHeader}>
        <GroupHeader title={t('usage.title')} description={t('usage.description')} />
        <div className={styles.usageControls}>
          <Label htmlFor="platform-usage-period">{t('usage.period')}</Label>
          <Input
            id="platform-usage-period"
            type="month"
            value={period}
            onChange={event => setPeriod(event.target.value)}
          />
          <Button type="button" variant="ghost" size="sm" disabled={loading} onClick={() => void load()}>
            <RefreshCw className={loading ? styles.spinner : ''} />{t('platform.refresh')}
          </Button>
        </div>
      </div>
      <p className={styles.notice}>{t('usage.notInvoice')}</p>
      <p className={styles.usageSemantics}>{t('usage.periodSemantics')}</p>

      {loading ? (
        <div className={styles.statePanel} role="status"><div><LoaderCircle className={styles.spinner} />{t('platform.loading')}</div></div>
      ) : error ? (
        <div className={styles.statePanel} role="alert"><div><span>{error}</span><Button type="button" variant="outline" onClick={() => void load()}>{t('platform.retry')}</Button></div></div>
      ) : report ? (
        <div className={styles.usageBody}>
          <dl className={styles.usageContext}>
            <div><dt>{t('usage.tenant')}</dt><dd><code>{report.tenant_id}</code></dd></div>
            <div><dt>{t('usage.reportPeriod')}</dt><dd>{report.period}</dd></div>
          </dl>
          <div className={styles.usageSummary}>
            <article><span>{t('usage.settled')}</span><strong>{format(report.quota.settled_tokens)}</strong></article>
            <article><span>{t('usage.currentLimit')}</span><strong>{format(report.quota.monthly_limit_tokens)}</strong></article>
            <article><span>{t('usage.activeReserved')}</span><strong>{format(report.quota.active_reserved_tokens)}</strong></article>
            <article><span>{t('usage.unknownReserved')}</span><strong>{format(report.quota.unknown_reserved_tokens)}</strong></article>
            <article><span>{t('usage.requests')}</span><strong>{format(report.totals.requests)}</strong></article>
            <article><span>{t('usage.attempts')}</span><strong>{format(report.totals.attempts)}</strong></article>
            <article><span>{t('usage.unknownAttempts')}</span><strong>{format(report.totals.unknown_attempts)}</strong></article>
            <article><span>{t('usage.total')}</span><strong>{format(report.totals.total_tokens)}</strong></article>
          </div>
          <dl className={styles.usageTokenFacts}>
            <div><dt>{t('usage.input')}</dt><dd>{format(report.totals.input_tokens)}</dd></div>
            <div><dt>{t('usage.cached')}</dt><dd>{format(report.totals.cached_input_tokens)}</dd></div>
            <div><dt>{t('usage.cacheWrite')}</dt><dd>{format(report.totals.cache_write_tokens)}</dd></div>
            <div><dt>{t('usage.output')}</dt><dd>{format(report.totals.output_tokens)}</dd></div>
            <div><dt>{t('usage.reasoning')}</dt><dd>{format(report.totals.reasoning_tokens)}</dd></div>
          </dl>

          {!hasPeriodData ? <div className={styles.statePanel}>{t('usage.empty')}</div> : null}

          <section className={styles.usageSection} aria-labelledby="platform-usage-anomalies">
            <h3 id="platform-usage-anomalies">{t('usage.anomalies')}</h3>
            <p className={styles.usageSectionDescription}>{t('usage.anomaliesDescription')}</p>
            {report.anomalies.length ? (
              <div className={styles.usageAnomalies}>
                {report.anomalies.map(reservation => (
                  <article key={reservation.reservation_id} data-platform-usage-anomaly={reservation.reservation_id}>
                    <AlertTriangle />
                    <div>
                      <strong>{reservation.reservation_id}</strong>
                      <span>{t('usage.user')}: {reservation.user_id} · {t('usage.run')}: {reservation.run_id ?? '—'} · {stateLabel(reservation.state, t)}</span>
                      <ReservationIssues reservation={reservation} t={t} />
                    </div>
                  </article>
                ))}
              </div>
            ) : <p className={styles.usageEmpty}>{t('usage.noAnomalies')}</p>}
            {report.anomalies_truncated ? <p className={styles.usageTruncated}>{t('usage.anomaliesTruncated', { limit: report.limit })}</p> : null}
          </section>

          <section className={styles.usageSection} aria-labelledby="platform-usage-groups">
            <h3 id="platform-usage-groups">{t('usage.groups')}</h3>
            {report.groups.length ? (
              <div className={styles.usageTableScroll} role="region" aria-labelledby="platform-usage-groups" tabIndex={0} data-platform-usage-table="groups">
                <table className={styles.usageTable}>
                  <caption className="sr-only">{t('usage.groups')}</caption>
                  <thead><tr>
                    <th scope="col">{t('usage.provider')}</th><th scope="col">{t('usage.model')}</th>
                    <th scope="col">{t('usage.requests')}</th><th scope="col">{t('usage.attempts')}</th>
                    <th scope="col">{t('usage.unknownAttempts')}</th><th scope="col">{t('usage.input')}</th>
                    <th scope="col">{t('usage.cached')}</th><th scope="col">{t('usage.output')}</th>
                    <th scope="col">{t('usage.total')}</th>
                  </tr></thead>
                  <tbody>{report.groups.map(group => (
                    <tr key={`${group.provider}\0${group.model}`} data-platform-usage-group="">
                      <td>{group.provider}</td><td>{group.model}</td><td>{format(group.usage.requests)}</td>
                      <td>{format(group.usage.attempts)}</td><td>{format(group.usage.unknown_attempts)}</td>
                      <td>{format(group.usage.input_tokens)}<small>{t('usage.cacheWrite')}: {format(group.usage.cache_write_tokens)}</small></td><td>{format(group.usage.cached_input_tokens)}</td>
                      <td>{format(group.usage.output_tokens)}<small>{t('usage.reasoning')}: {format(group.usage.reasoning_tokens)}</small></td><td>{format(group.usage.total_tokens)}</td>
                    </tr>
                  ))}</tbody>
                </table>
              </div>
            ) : <p className={styles.usageEmpty}>{t('usage.noLedger')}</p>}
          </section>

          <section className={styles.usageSection} aria-labelledby="platform-usage-reservations">
            <h3 id="platform-usage-reservations">{t('usage.reservations')}</h3>
            {report.reservations.length ? (
              <div className={styles.usageTableScroll} role="region" aria-labelledby="platform-usage-reservations" tabIndex={0} data-platform-usage-table="reservations">
                <table className={styles.usageTable}>
                  <caption className="sr-only">{t('usage.reservations')}</caption>
                  <thead><tr>
                    <th scope="col">{t('usage.reservation')}</th><th scope="col">{t('usage.user')}</th>
                    <th scope="col">{t('usage.run')}</th><th scope="col">{t('usage.state')}</th>
                    <th scope="col">{t('usage.reserved')}</th><th scope="col">{t('usage.committed')}</th>
                    <th scope="col">{t('usage.unknownReserved')}</th><th scope="col">{t('usage.created')}</th>
                  </tr></thead>
                  <tbody>{report.reservations.map(reservation => (
                    <tr key={reservation.reservation_id} data-platform-usage-reservation={reservation.reservation_id}>
                      <td><code>{reservation.reservation_id}</code><ReservationIssues reservation={reservation} t={t} /></td>
                      <td><code>{reservation.user_id}</code></td>
                      <td><code>{reservation.run_id ?? '—'}</code>{reservation.run_state ? <small>{stateLabel(reservation.run_state, t)}</small> : null}</td>
                      <td>{stateLabel(reservation.state, t)}</td><td>{format(reservation.reserved_tokens)}</td>
                      <td>{format(reservation.committed_tokens)}</td><td>{format(reservation.unknown_tokens)}</td><td>{time.format(reservation.created_at_ms)}</td>
                    </tr>
                  ))}</tbody>
                </table>
              </div>
            ) : <p className={styles.usageEmpty}>{t('usage.noReservations')}</p>}
            {report.reservations_truncated ? <p className={styles.usageTruncated}>{t('usage.truncated', { limit: report.limit })}</p> : null}
          </section>

          <section className={styles.usageSection} aria-labelledby="platform-usage-ledger">
            <h3 id="platform-usage-ledger">{t('usage.ledger')}</h3>
            {report.ledger.length ? (
              <div className={styles.usageTableScroll} role="region" aria-labelledby="platform-usage-ledger" tabIndex={0} data-platform-usage-table="ledger">
                <table className={styles.usageTable}>
                  <caption className="sr-only">{t('usage.ledger')}</caption>
                  <thead><tr>
                    <th scope="col">{t('usage.recorded')}</th><th scope="col">{t('usage.actor')}</th>
                    <th scope="col">{t('usage.provider')}</th><th scope="col">{t('usage.model')}</th>
                    <th scope="col">{t('usage.input')}</th><th scope="col">{t('usage.cached')}</th>
                    <th scope="col">{t('usage.output')}</th><th scope="col">{t('usage.accounted')}</th><th scope="col">{t('usage.request')}</th>
                    <th scope="col">{t('usage.reservation')}</th>
                  </tr></thead>
                  <tbody>{report.ledger.map(entry => {
                    const requestIdentity = `${entry.request_id}:${entry.attempt}`
                    return (
                      <tr
                        key={requestIdentity}
                        data-platform-usage-ledger={requestIdentity}
                      >
                        <td>{time.format(entry.recorded_at_ms)}</td><td><code>{entry.actor_user_id}</code>
                          <small>{t('usage.resourceOwner')}: {entry.resource_owner_user_id}</small>
                          <small>{t('usage.beneficiary')}: {entry.model_beneficiary_user_id}</small>
                        </td>
                        <td>{entry.provider}</td><td>{entry.model}</td><td>{format(entry.input_tokens)}<small>{t('usage.cacheWrite')}: {format(entry.cache_write_tokens)}</small></td>
                        <td>{format(entry.cached_input_tokens)}</td><td>{format(entry.output_tokens)}<small>{t('usage.reasoning')}: {format(entry.reasoning_tokens)}</small></td>
                        <td>{entry.accounted_tokens === null ? t('usage.notReported') : format(entry.accounted_tokens)}</td>
                        <td><code>{entry.request_id}</code><small>{t('usage.attemptNumber', { number: entry.attempt })}</small><small>{t('usage.run')}: {entry.run_id}</small><small>{entry.provider_request_id ?? '—'}</small></td>
                        <td><code>{entry.reservation_id}</code><small>{stateLabel(entry.reservation_state, t)}</small></td>
                      </tr>
                    )
                  })}</tbody>
                </table>
              </div>
            ) : <p className={styles.usageEmpty}>{t('usage.noLedger')}</p>}
            {report.ledger_truncated ? <p className={styles.usageTruncated}>{t('usage.truncated', { limit: report.limit })}</p> : null}
          </section>
        </div>
      ) : null}
    </div>
  )
}
