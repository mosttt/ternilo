import { expect, it } from 'vitest'
import { usageCsv, type UsageSummary } from './computer-usage-export'

it('exports unknown counters as empty, reported zeros as zero, and neutralizes spreadsheet formulas', () => {
  const totals = { attempts: 101, completed: 100, failed: 2, input: { tokens: 0, reported_attempts: 99 }, output: { tokens: null, reported_attempts: 0 }, cached_input: { tokens: null, reported_attempts: 0 }, cache_write: { tokens: null, reported_attempts: 0 }, reasoning: { tokens: null, reported_attempts: 0 } }
  const report: UsageSummary = { source: 'device_reported', period: '2026-10', observed_at_ms: 0, totals, groups: [{ provider: '=SUM(A1:A2)', model: '  +危险,"model"\nline', protocol: 'openai-responses', totals }] }
  const csv = usageCsv(report, { executor_id: 'ter_pc_example', management: { name: '\t@formula' } })
  expect(csv.startsWith('\uFEFF"source"')).toBe(true)
  expect(csv).toContain('"\'=SUM(A1:A2)"')
  expect(csv).toContain('"\'  +危险,""model""\nline"')
  expect(csv).toContain('"\'\t@formula"')
  expect(csv).toContain('"101","100","2","1","0","99","","0"')
  expect(csv).toContain('"total","","",""')
  expect(csv).toContain('"1970-01-01T00:00:00.000Z"')
})
