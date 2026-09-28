import { act, useEffect } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { LocaleProvider, useLocale } from '@/i18n/provider'
import type { ModelUsageReport } from '@/types'
import { getTenantModelUsage } from './platform-admin-api'
import { PlatformUsageSettings } from './platform-usage-settings'

vi.mock('./platform-admin-api', () => ({ getTenantModelUsage: vi.fn() }))

const storageValues = new Map<string, string>()
Object.defineProperty(globalThis, 'localStorage', {
  configurable: true,
  value: {
    get length() { return storageValues.size },
    clear: () => storageValues.clear(),
    getItem: (key: string) => storageValues.get(key) ?? null,
    key: (index: number) => [...storageValues.keys()][index] ?? null,
    removeItem: (key: string) => { storageValues.delete(key) },
    setItem: (key: string, value: string) => { storageValues.set(key, value) },
  } satisfies Storage,
})

const report: ModelUsageReport = {
  tenant_id: 'tenant-a',
  period: '2026-08',
  period_start_ms: Date.UTC(2026, 7, 1),
  period_end_ms: Date.UTC(2026, 8, 1),
  limit: 200,
  quota: { monthly_limit_tokens: 10_000, settled_tokens: 120, active_reserved_tokens: 200, unknown_reserved_tokens: 80 },
  totals: { requests: 1, attempts: 2, unknown_attempts: 1, input_tokens: 100, output_tokens: 20, cached_input_tokens: 40, cache_write_tokens: 5, reasoning_tokens: 10, total_tokens: 120 },
  groups: [{
    provider: 'provider-a', model: 'model-a',
    usage: { requests: 1, attempts: 2, unknown_attempts: 1, input_tokens: 100, output_tokens: 20, cached_input_tokens: 40, cache_write_tokens: 5, reasoning_tokens: 10, total_tokens: 120 },
  }],
  ledger: [{
    run_id: 'run-a', actor_user_id: 'usr-actor', resource_owner_user_id: 'usr-owner', model_beneficiary_user_id: 'usr-budget', lease_token: 7, request_id: 'request-a', attempt: 1, provider: 'provider-a', model: 'model-a',
    input_tokens: 100, output_tokens: 20, cached_input_tokens: 40, cache_write_tokens: 5, reasoning_tokens: 10, accounted_tokens: 120,
    provider_request_id: 'provider-request-a', recorded_at_ms: Date.UTC(2026, 7, 2),
    reservation_id: 'reservation-a', reservation_state: 'committed',
  }, {
    run_id: 'run-a', actor_user_id: 'usr-actor', resource_owner_user_id: 'usr-owner', model_beneficiary_user_id: 'usr-budget', lease_token: 7, request_id: 'request-a', attempt: 2, provider: 'provider-a', model: 'model-a',
    input_tokens: null, output_tokens: null, cached_input_tokens: null, cache_write_tokens: null, reasoning_tokens: null, accounted_tokens: null,
    provider_request_id: 'provider-request-retry', recorded_at_ms: Date.UTC(2026, 7, 2),
    reservation_id: 'reservation-a', reservation_state: 'committed',
  }],
  ledger_truncated: false,
  reservations: [{
    reservation_id: 'reservation-a', user_id: 'usr-a', run_id: 'run-a',
    reserved_tokens: 200, committed_tokens: 120, state: 'committed',
    created_at_ms: Date.UTC(2026, 7, 2), expires_at_ms: Date.UTC(2026, 7, 3),
    run_state: 'succeeded', ledger_tokens: 120, unknown_tokens: 80, issues: ['unknown_model_usage'],
  }],
  reservations_truncated: false,
  anomalies: [{
    reservation_id: 'reservation-b', user_id: 'usr-a', run_id: 'run-b',
    reserved_tokens: 80, committed_tokens: null, state: 'active',
    created_at_ms: Date.UTC(2026, 7, 2), expires_at_ms: 1,
    run_state: 'failed', ledger_tokens: 0, unknown_tokens: 0,
    issues: ['expired_active', 'terminal_run_active'],
  }],
  anomalies_truncated: false,
}

let host: HTMLDivElement
let root: Root

beforeEach(() => {
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true
  localStorage.clear()
  host = document.createElement('div')
  document.body.append(host)
  root = createRoot(host)
})

afterEach(() => {
  act(() => root.unmount())
  host.remove()
  localStorage.clear()
  vi.clearAllMocks()
  delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
})

function UsageFixture({ english = false }: { english?: boolean }) {
  const { setLocale } = useLocale()
  useEffect(() => { if (english) setLocale('en') }, [english, setLocale])
  return <PlatformUsageSettings tenantId="tenant-a" />
}

async function render(english = false) {
  await act(async () => {
    root.render(<LocaleProvider><UsageFixture english={english} /></LocaleProvider>)
    await Promise.resolve()
  })
}

async function settle() {
  await act(async () => {
    await Promise.resolve()
    await Promise.resolve()
    await Promise.resolve()
  })
}

describe('Platform usage settings', () => {
  it('moves from loading to an authoritative ready report without inventing money', async () => {
    let resolve!: (value: ModelUsageReport) => void
    vi.mocked(getTenantModelUsage).mockReturnValue(new Promise(value => { resolve = value }))
    await render()
    expect(host.querySelector('[data-platform-list-state]')?.getAttribute('data-platform-list-state')).toBe('loading')

    resolve(report)
    await settle()
    expect(host.querySelector('[data-platform-list-state]')?.getAttribute('data-platform-list-state')).toBe('ready')
    expect(host.textContent).toContain('这里记录 token，不是金额账单')
    expect(host.textContent).toContain('provider-a')
    expect(host.textContent).toContain('provider-request-a')
    expect(host.textContent).toContain('其中缓存输入（已包含在输入）')
    expect(host.textContent).toContain('其中推理（已包含在输出）')
    expect(host.textContent).toContain('调用预留（用量待确认）')
    const firstCall = host.querySelector('[data-platform-usage-ledger="request-a:1"]')!
    expect(firstCall.textContent).toContain('usr-actor')
    expect(firstCall.textContent).toContain('资源所有者: usr-owner')
    expect(firstCall.textContent).toContain('模型额度归属: usr-budget')
    const unknownCall = host.querySelector('[data-platform-usage-ledger="request-a:2"]')!
    expect(unknownCall.textContent).toContain('尚未上报')
    expect(unknownCall.textContent).not.toMatch(/缓存写入: 0|输出: 0/)
    expect(host.textContent).toContain('跨月完成或重试不会改变归属月份')
    expect(host.textContent).toContain('已过期但仍为 active')
    expect(host.textContent).toContain('tenant-a')
    expect(host.textContent).toContain('2026-08')
    expect(host.querySelector('[data-platform-usage-reservation="reservation-a"]')).not.toBeNull()
    expect(host.querySelector('[data-platform-usage-ledger="request-a:1"]')).not.toBeNull()
    expect(host.querySelector('[data-platform-usage-ledger="request-a:2"]')).not.toBeNull()
    expect(host.querySelectorAll('[role="region"][tabindex="0"]')).toHaveLength(3)
    for (const table of ['groups', 'reservations', 'ledger']) {
      expect(host.querySelector(`[data-platform-usage-table="${table}"][tabindex="0"]`)).not.toBeNull()
    }
    expect([...host.querySelectorAll('th')].every(header => header.scope === 'col')).toBe(true)
    expect(host.querySelectorAll('caption.sr-only')).toHaveLength(3)
    expect(getTenantModelUsage).toHaveBeenCalledWith('tenant-a', expect.stringMatching(/^\d{4}-\d{2}$/), 200)
  })

  it('keeps the selected month empty while showing all-time anomalies in English', async () => {
    vi.mocked(getTenantModelUsage).mockResolvedValue({
      ...report,
      totals: { requests: 0, attempts: 0, unknown_attempts: 0, input_tokens: 0, output_tokens: 0, cached_input_tokens: 0, cache_write_tokens: 0, reasoning_tokens: 0, total_tokens: 0 },
      groups: [], ledger: [], reservations: [],
    })
    await render(true)
    await settle()
    expect(host.querySelector('[data-platform-list-state]')?.getAttribute('data-platform-list-state')).toBe('empty')
    expect(host.textContent).toContain('This is a token ledger, not a monetary invoice')
    expect(host.textContent).toContain('There is no model usage or reservation in this month')
    expect(host.textContent).toContain('All-time unsettled anomalies')
    expect(host.textContent).toContain('not affected by the UTC month filter')
    expect(host.querySelector('[data-platform-usage-anomaly="reservation-b"]')).not.toBeNull()
  })

  it('drops an older month response after the user switches periods', async () => {
    const requests: Array<{ resolve: (value: ModelUsageReport) => void }> = []
    vi.mocked(getTenantModelUsage).mockImplementation(() => new Promise(resolve => requests.push({ resolve })))
    await render()
    expect(requests).toHaveLength(1)

    const input = host.querySelector<HTMLInputElement>('#platform-usage-period')!
    const firstPeriod = vi.mocked(getTenantModelUsage).mock.calls[0][1]
    const nextPeriod = firstPeriod === '2026-07' ? '2026-06' : '2026-07'
    await act(async () => {
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')?.set?.call(input, nextPeriod)
      input.dispatchEvent(new Event('input', { bubbles: true }))
      await Promise.resolve()
    })
    expect(requests).toHaveLength(2)
    expect(host.querySelector('[data-platform-list-state]')?.getAttribute('data-platform-list-state')).toBe('loading')

    requests[1].resolve({ ...report, tenant_id: 'tenant-new', period: nextPeriod })
    await settle()
    expect(host.textContent).toContain('tenant-new')
    expect(host.textContent).toContain(nextPeriod)

    requests[0].resolve({ ...report, tenant_id: 'tenant-stale', period: firstPeriod })
    await settle()
    expect(host.textContent).toContain('tenant-new')
    expect(host.textContent).not.toContain('tenant-stale')
  })

  it('shows a retryable error instead of an empty ledger', async () => {
    vi.mocked(getTenantModelUsage).mockRejectedValue(new Error('offline'))
    await render()
    await settle()
    expect(host.querySelector('[data-platform-list-state]')?.getAttribute('data-platform-list-state')).toBe('error')
    expect(host.querySelector('[role="alert"]')?.textContent).toContain('模型用量与结算记录加载失败')
    expect([...host.querySelectorAll('button')].some(button => button.textContent?.includes('重试'))).toBe(true)
  })
})
