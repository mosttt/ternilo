export interface UsageCount { tokens: number | null; reported_attempts: number }
export interface UsageTotals {
  attempts: number; completed: number; failed: number
  input: UsageCount; output: UsageCount; cached_input: UsageCount; cache_write: UsageCount; reasoning: UsageCount
}
export interface UsageSummary {
  source: 'device_reported'; period: string; observed_at_ms: number
  totals: UsageTotals
  groups: Array<{ provider: string; model: string; protocol: string; totals: UsageTotals }>
}

const counters = ['input', 'output', 'cached_input', 'cache_write', 'reasoning'] as const
function cell(value: string | number | null) {
  const text = value === null ? '' : String(value)
  const safe = /^[\t\r\n]|^\s*[=+\-@]/u.test(text) ? `'${text}` : text
  return `"${safe.replaceAll('"', '""')}"`
}
export function usageCsv(report: UsageSummary, computer: { executor_id: string; management: { name: string } }) {
  const header = ['source', 'month_utc', 'observed_at_utc', 'computer_id', 'computer_name', 'row_kind', 'provider', 'model', 'protocol', 'attempts', 'completed', 'failed', 'unfinished', ...counters.flatMap(field => [`${field}_tokens`, `${field}_reported_attempts`])]
  const row = (kind: string, provider: string, model: string, protocol: string, totals: UsageTotals) => [
    report.source, report.period, new Date(report.observed_at_ms).toISOString(), computer.executor_id, computer.management.name,
    kind, provider, model, protocol, totals.attempts, totals.completed, totals.failed, totals.attempts - totals.completed,
    ...counters.flatMap(field => [totals[field].tokens, totals[field].reported_attempts]),
  ]
  return '\uFEFF' + [header, row('total', '', '', '', report.totals), ...report.groups.map(group => row('model', group.provider, group.model, group.protocol, group.totals))].map(values => values.map(cell).join(',')).join('\r\n') + '\r\n'
}
export function downloadUsageCsv(report: UsageSummary, computer: { executor_id: string; management: { name: string } }) {
  const url = URL.createObjectURL(new Blob([usageCsv(report, computer)], { type: 'text/csv;charset=utf-8' }))
  const anchor = document.createElement('a'); anchor.href = url
  anchor.download = `ternilo-usage-${computer.executor_id}-${report.period}.csv`
  document.body.append(anchor); anchor.click(); anchor.remove()
  window.setTimeout(() => URL.revokeObjectURL(url), 0)
}
