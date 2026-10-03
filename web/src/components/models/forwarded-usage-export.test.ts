import { expect, it } from 'vitest'
import { forwardedUsageCsv, type ForwardedSummary } from './forwarded-usage-export'

it('keeps request and attempt totals separate and preserves route identity and unknown coverage in CSV', () => {
  const unknown = { tokens: null, reported_attempts: 0 }
  const totals = { requests: 1, active_requests: 0, usage: { attempts: 3, completed: 2, failed: 1, input: { tokens: 10, reported_attempts: 2 }, output: { tokens: 0, reported_attempts: 1 }, cached_input: unknown, cache_write: unknown, reasoning: unknown } }
  const report: ForwardedSummary = { source: 'computer_forwarded', period: '2026-10', observed_at_ms: 0, totals, groups: [{ execution_executor_id: 'ter_pc_a', source_executor_id: 'ter_pc_b', execution_computer_name: '=formula', source_computer_name: 'B,"name"\nnext', actor_user_id: 'ter_sa_actor', model_owner_user_id: 'model-owner', resource_owner_user_id: 'workspace-owner', provider: 'provider', model: 'model', protocol: 'anthropic-messages', totals }] }
  const csv = forwardedUsageCsv(report, 'tenant')
  expect(csv).toContain('"\'=formula"')
  expect(csv).toContain('"B,""name""\nnext"')
  expect(csv).toContain('"ter_sa_actor","model-owner","workspace-owner"')
  expect(csv).toContain('"1","0","3","2","1","1","10","2","0","1","","0"')
  expect(csv).toContain('"model_route"')
  expect(csv).toContain('"total"')
  expect(csv.startsWith('\uFEFF')).toBe(true)
})
