import { act, type ReactNode } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { LocaleProvider } from '@/i18n/provider'
import type { AuditEntry, TenantQuota } from '@/types'
import {
  parseQuotaDraft,
  PlatformAuditSettings,
  PlatformQuotaSettings,
} from './platform-governance-settings'
import { getTenantQuota, listTenantAudit, updateTenantQuota } from './platform-admin-api'

vi.mock('./platform-admin-api', () => ({
  getTenantQuota: vi.fn(),
  listTenantAudit: vi.fn(),
  updateTenantQuota: vi.fn(),
}))

const quota: TenantQuota = {
  max_nodes: 10,
  max_concurrent_runs: 4,
  monthly_model_tokens: 10_000_000,
  max_secrets: 100,
}

const audit: AuditEntry = {
  audit_id: 'audit-1',
  tenant_id: 'tenant-a',
  actor_user_id: 'usr-owner',
  actor_kind: 'user',
  action: 'quota.update',
  resource_type: 'tenant',
  resource_id: 'tenant-a',
  outcome: 'success',
  metadata: { max_nodes: 12 },
  occurred_at_ms: 1_700_000_000_000,
  entry_hash_hex: 'abcd1234',
}

let host: HTMLDivElement
let root: Root

beforeEach(() => {
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true
  host = document.createElement('div')
  document.body.append(host)
  root = createRoot(host)
  vi.mocked(getTenantQuota).mockResolvedValue(quota)
  vi.mocked(updateTenantQuota).mockResolvedValue(undefined)
  vi.mocked(listTenantAudit).mockResolvedValue([audit])
})

afterEach(() => {
  act(() => root.unmount())
  host.remove()
  document.body.innerHTML = ''
  vi.clearAllMocks()
  delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
})

async function render(node: ReactNode) {
  await act(async () => {
    root.render(<LocaleProvider>{node}</LocaleProvider>)
    await Promise.resolve()
    await Promise.resolve()
  })
}

async function settle(action?: () => void) {
  await act(async () => {
    action?.()
    await Promise.resolve()
    await Promise.resolve()
    await Promise.resolve()
  })
}

function setInput(input: HTMLInputElement, value: string) {
  const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')?.set
  setter?.call(input, value)
  input.dispatchEvent(new Event('input', { bubbles: true }))
}

function button(text: string) {
  const found = [...host.querySelectorAll<HTMLButtonElement>('button')]
    .find(item => item.textContent?.trim() === text)
  if (!found) throw new Error(`missing button ${text}`)
  return found
}

describe('Platform governance settings', () => {
  it('parses only positive integer quota values', () => {
    expect(parseQuotaDraft({ max_nodes: '2', max_concurrent_runs: '3', monthly_model_tokens: '1000', max_secrets: '5' }))
      .toEqual({ max_nodes: 2, max_concurrent_runs: 3, monthly_model_tokens: 1000, max_secrets: 5 })
    expect(parseQuotaDraft({ max_nodes: '0', max_concurrent_runs: '3', monthly_model_tokens: '1000', max_secrets: '5' })).toBeNull()
    expect(parseQuotaDraft({ max_nodes: '2.5', max_concurrent_runs: '3', monthly_model_tokens: '1000', max_secrets: '5' })).toBeNull()
  })

  it('lets an owner edit the real quota while an admin gets an explicit read-only view', async () => {
    await render(<PlatformQuotaSettings tenantId="tenant-a" editable />)
    expect(getTenantQuota).toHaveBeenCalledWith('tenant-a')
    const nodes = host.querySelector<HTMLInputElement>('#platform-quota-max_nodes')!
    await settle(() => setInput(nodes, '12'))
    await settle(() => button('保存配额').click())
    expect(updateTenantQuota).toHaveBeenCalledWith('tenant-a', { ...quota, max_nodes: 12 })
    expect(host.textContent).toContain('空间配额已保存')

    await settle(() => root.render(
      <LocaleProvider><PlatformQuotaSettings tenantId="tenant-a" editable={false} /></LocaleProvider>,
    ))
    await settle()
    expect(host.textContent).toContain('只有空间所有者可以修改')
    expect(host.querySelector<HTMLInputElement>('#platform-quota-max_nodes')?.readOnly).toBe(true)
    expect([...host.querySelectorAll('button')].some(item => item.textContent?.includes('保存配额'))).toBe(false)
  })

  it('shows full audit facts and metadata, with a real limited-list label and retry state', async () => {
    await render(<PlatformAuditSettings tenantId="tenant-a" />)
    expect(listTenantAudit).toHaveBeenCalledWith('tenant-a', 200)
    expect(host.textContent).toContain('最早的 200 条记录')
    const row = host.querySelector<HTMLElement>('[data-platform-audit-entry="audit-1"]')!
    expect(row.textContent).toContain('quota.update')
    expect(row.textContent).toContain('usr-owner')
    expect(row.textContent).toContain('abcd1234')
    const details = row.querySelector<HTMLDetailsElement>('details')!
    await settle(() => { details.open = true; details.dispatchEvent(new Event('toggle', { bubbles: true })) })
    expect(details.textContent).toContain('"max_nodes": 12')

    vi.mocked(listTenantAudit).mockRejectedValueOnce(new Error('offline'))
    await settle(() => button('刷新').click())
    expect(host.querySelector('[data-platform-list-state]')?.getAttribute('data-platform-list-state')).toBe('ready')
    expect(host.querySelector('[role="alert"]')?.textContent).toContain('审计记录加载失败')
    expect(row.isConnected).toBe(true)
  })
})
