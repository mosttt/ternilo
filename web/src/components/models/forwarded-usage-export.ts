import { encodeUsageCsv, type UsageTotals } from './computer-usage-export'
export interface ForwardedTotals { requests: number; active_requests: number; usage: UsageTotals }
export interface ForwardedGroup {
  execution_executor_id: string; source_executor_id: string
  execution_computer_name: string; source_computer_name: string
  actor_user_id: string; model_owner_user_id: string; resource_owner_user_id: string
  provider: string; model: string; protocol: string; totals: ForwardedTotals
}
export interface ForwardedSummary {
  source: 'computer_forwarded'; period: string; observed_at_ms: number
  totals: ForwardedTotals; groups: ForwardedGroup[]
}
export function forwardedUsageCsv(report: ForwardedSummary, tenantId: string) {
  const counters = ['input', 'output', 'cached_input', 'cache_write', 'reasoning'] as const
  const header = ['source', 'month_utc', 'observed_at_utc', 'tenant_id', 'row_kind', 'execution_computer_id', 'execution_computer_name', 'source_computer_id', 'source_computer_name', 'actor_user_id', 'model_owner_user_id', 'resource_owner_user_id', 'provider', 'model', 'protocol', 'requests', 'active_requests', 'attempts', 'finished_attempts', 'failed_attempts', 'unfinished_attempts', ...counters.flatMap(key => [`${key}_tokens`, `${key}_reported_attempts`])]
  const row = (totals: ForwardedTotals, group?: ForwardedGroup) => [
    report.source, report.period, new Date(report.observed_at_ms).toISOString(), tenantId, group ? 'model_route' : 'total',
    ...(['execution_executor_id', 'execution_computer_name', 'source_executor_id', 'source_computer_name', 'actor_user_id', 'model_owner_user_id', 'resource_owner_user_id', 'provider', 'model', 'protocol'] as const).map(key => group?.[key] ?? ''),
    totals.requests, totals.active_requests, totals.usage.attempts, totals.usage.completed, totals.usage.failed, totals.usage.attempts - totals.usage.completed,
    ...counters.flatMap(key => [totals.usage[key].tokens, totals.usage[key].reported_attempts]),
  ]
  return encodeUsageCsv([header, row(report.totals), ...report.groups.map(group => row(group.totals, group))])
}
