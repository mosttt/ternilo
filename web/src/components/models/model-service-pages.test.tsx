import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { api } from '@/api/client'
import { navigate } from '@/app/navigation'
import { LocaleProvider } from '@/i18n/provider'
import { ModelAccessShell, ModelAdminPage } from './model-service-pages'
import { ModelProviders } from './model-providers'
import { ModelGrants } from './model-grants'
import { ModelUsage } from './model-usage'
import type { ModelEntitlement, ModelKey, ModelProvider } from './model-service-api'

const workbench = vi.hoisted(() => ({ platform: true, authRequired: false, loading: false, serverIdentity: { platform_role: 'user', user: { user_id: 'alice', username: 'alice' } }, logout: vi.fn() }))
vi.mock('@/state/workbench', () => ({ useWorkbench: () => workbench }))
vi.mock('@/api/client', () => ({ api: { request: vi.fn() } }))
vi.mock('@/components/workbench/model-picker', () => ({ ModelPicker: () => null, modelLabel: () => '', persistModelSelection: vi.fn() }))

const entitlement: ModelEntitlement = { grant: { grant_id: 'grant-one', allow_resource_sharing: true, name: 'Development shared budget', subject: { kind: 'group', id: 'global-group' }, subject_name: 'Developers', model_ids: ['alpha', 'beta'], quota: { month: '2026-09', limit_tokens: 10_000, used_tokens: 500, reserved_tokens: 100, active_requests: 0, max_concurrent_requests: 2 }, expires_at_ms: null, revoked_at_ms: null, created_at_ms: 1, updated_at_ms: 1 }, models: ['alpha', 'beta'].map(id => ({ model_id: id, display_name: id.toUpperCase(), protocol: 'openai-chat-completions', defaults: { context_window: 64_000, max_output_tokens: 4_000 } })) }
const key: ModelKey = { key_id: 'key-one', user_id: 'alice', name: 'Laptop', token_prefix: 'knm_fixture', grant_id: 'grant-one', grant_name: 'Development shared budget', model_ids: ['alpha'], monthly_tokens: null, max_concurrent_requests: null, expires_at_ms: null, revoked_at_ms: null, created_at_ms: 1, last_used_at_ms: null }
const provider: ModelProvider = { profile: { id: 'upstream', display_name: 'Private upstream', base_url: 'https://upstream.example/v1', protocol: 'openai-chat-completions', api_key_ref: null, defaults: { context_window: 64_000, max_output_tokens: 4_000 }, models: [{ id: 'alpha', settings: { mode: 'inherit' } }], timeout_ms: 30_000, max_attempts: 1, retry_base_delay_ms: 250 }, enabled: true, has_api_key: true, created_at_ms: 1, updated_at_ms: 1 }
const secret = 'knm_only-visible-once-fixture'
let host: HTMLDivElement
let root: Root

beforeEach(() => {
  vi.stubGlobal('ResizeObserver', class { observe() {} disconnect() {} })
  history.replaceState({}, '', '/models?source=platform')
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true
  workbench.serverIdentity.platform_role = 'user'
  workbench.loading = false
  host = document.createElement('div'); document.body.append(host); root = createRoot(host)
  vi.mocked(api.request).mockImplementation(async (path, options) => {
    if (path.startsWith('/model-access/catalog')) return { entitlements: [entitlement], next_cursor: null }
    if (path === '/model-access/keys' && options?.method === 'POST') return { key, token: secret }
    if (path.startsWith('/admin/models/providers')) return options?.method === 'PUT' ? provider : { providers: [provider], next_cursor: null }
    if (path.startsWith('/admin/models/publications')) return { models: [], next_cursor: null }
    if (path.includes('/usage')) return { month: '2026-09', request_count: 1, active_requests: 0, unknown_requests: 1, used_tokens: 0, reserved_tokens: 800, input_tokens: 0, output_tokens: 0, cached_input_tokens: 0, cache_write_tokens: 0, reasoning_tokens: 0 }
    if (path.includes('/requests')) return { requests: [{ request_id: 'unknown-call', origin: 'api_key', source: 'platform_grant', actor_user_id: 'alice', resource_owner_user_id: null, model_beneficiary_user_id: 'alice', workload: null, attempts: [], key_id: 'key-one', grant_id: 'grant-one', grant_name: 'Development', model_id: 'alpha', protocol: 'openai-chat-completions', state: 'failed', attempted: true, reserved_tokens: 800, accounted_tokens: null, usage: null, error_code: 'upstream_disconnected', month: '2026-09', created_at_ms: 1, expires_at_ms: 100, settled_at_ms: 50 }], next_cursor: null }
    return { keys: [key], next_cursor: null }
  })
})
afterEach(() => { act(() => root.unmount()); host.remove(); vi.clearAllMocks(); vi.unstubAllGlobals(); history.replaceState({}, '', '/') })
async function render(element: React.ReactNode) { await act(async () => root.render(<LocaleProvider>{element}</LocaleProvider>)) }
function button(text: string, scope: ParentNode = document) {
  const found = [...scope.querySelectorAll<HTMLButtonElement>('button')].find(element => element.textContent === text)
  expect(found, `button ${text}`).toBeTruthy()
  return found!
}
async function click(element: HTMLElement) { await act(async () => element.click()) }
async function input(selector: string, value: string) {
  const element = document.querySelector<HTMLInputElement>(selector)!
  await act(async () => { Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')!.set!.call(element, value); element.dispatchEvent(new Event('input', { bubbles: true })) })
}

it('waits for initial account scope restoration without unmounting the model page on later refreshes', async () => {
  workbench.loading = true
  await render(<ModelAccessShell />)
  expect(api.request).not.toHaveBeenCalled()
  expect(host.querySelector('[data-model-access]')).toBeNull()
  workbench.loading = false
  await render(<ModelAccessShell />)
  const page = host.querySelector('[data-model-access]')
  expect(page).not.toBeNull()
  workbench.loading = true
  await render(<ModelAccessShell />)
  expect(host.querySelector('[data-model-access]')).toBe(page)
})

it('filters usage in the Server API and does not invent device-local totals', async () => {
  history.replaceState({}, '', '/models?tab=usage&usage_source=account')
  await render(<ModelAccessShell />)
  expect(api.request).toHaveBeenCalledWith('/model-access/requests?source=user_provider&limit=25', expect.anything())
  expect(api.request).toHaveBeenCalledWith('/model-access/usage?source=user_provider', expect.anything())
  vi.mocked(api.request).mockClear()
  await click(button('设备本地'))
  expect(host.textContent).toContain('设备本地用量尚未汇总')
  expect(host.querySelector('[data-model-request]')).toBeNull()
  expect(api.request).not.toHaveBeenCalled()
  await act(async () => navigate('/models?tab=access'))
  expect(host.querySelector('aside nav [aria-current="page"]')?.textContent).toBe('授权与接入')
  expect(host.querySelector('[role="tab"][aria-selected="true"]')?.textContent).toBe('接入密钥')
  expect(host.querySelector('#settings-panel-keys')).not.toBeNull()
})

it('lets ordinary users create a key for one grant and selected models, without retaining its plaintext', async () => {
  await render(<ModelAccessShell />)
  expect(document.querySelector('a[href="/admin/models"]')).toBeNull()
  expect(host.textContent).toContain('整组共享额度')
  await click(button('创建接入密钥'))
  await input('#model-key-name', 'Laptop')
  const dialog = document.querySelector('[role="dialog"]')!
  const beta = [...dialog.querySelectorAll<HTMLLabelElement>('label')].find(label => label.textContent?.includes('BETA'))!.querySelector<HTMLInputElement>('input')!
  await click(beta)
  await act(async () => dialog.querySelector('form')!.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true })))
  expect(api.request).toHaveBeenCalledWith('/model-access/keys', { method: 'POST', body: expect.objectContaining({ name: 'Laptop', grant_id: 'grant-one', model_ids: ['alpha'], monthly_tokens: null, max_concurrent_requests: null, expires_at_ms: expect.any(Number) }) })
  expect(document.querySelector('[data-model-secret]')?.textContent).toBe(secret)
  await click(button('关闭', dialog))
  expect(document.body.textContent).not.toContain(secret)
  await click(button('创建接入密钥'))
  expect(document.querySelector('[data-model-secret]')).toBeNull()
  expect(document.querySelector<HTMLInputElement>('#model-key-name')?.value).toBe('')
})

it('preserves an upstream key when editing without a replacement and saves managed retry settings with an explicit public API boundary', async () => {
  await render(<ModelProviders />)
  await click(button('编辑'))
  expect(document.querySelector<HTMLInputElement>('[id$="-provider-key"]')?.value).toBe('')
  await input('[id$="-provider-attempts"]', '3')
  await input('[id$="-provider-retry-delay"]', '500')
  expect(document.body.textContent).toContain('/v1 请求只调用上游一次')
  await click(button('保存', document.querySelector('[role="dialog"]')!))
  const request = vi.mocked(api.request).mock.calls.find(([path, options]) => path === '/admin/models/providers/upstream' && options?.method === 'PUT')!
  expect(request[1]?.body).toMatchObject({ enabled: true, clear_api_key: false, profile: { max_attempts: 3, retry_base_delay_ms: 500 } })
  expect(request[1]?.body).not.toHaveProperty('api_key')
})

it('keeps audit users read-only in model administration', async () => {
  workbench.serverIdentity.platform_role = 'auditor'
  await render(<ModelAdminPage />)
  expect([...host.querySelectorAll('button')].some(item => item.textContent === '发布模型')).toBe(false)
  await click(button('上游接入'))
  expect(host.textContent).toContain('Private upstream')
  expect([...host.querySelectorAll('button')].some(item => item.textContent === '编辑' || item.textContent === '添加上游')).toBe(false)
})

it('labels unknown request usage without inventing a zero-token settlement', async () => {
  await render(<ModelUsage />)
  const row = host.querySelector('[data-model-request="unknown-call"]')!
  expect(row.textContent).toContain('用量待核对')
  expect(row.textContent).not.toContain('已结算 0 tok')
  expect(row.textContent).not.toContain('undefined')
})

it('separates BYOK from platform budgets and exposes attempt totals without double-counting', async () => {
  const attempt = { attempt: 1, state: 'failed', attempted: true, reserved_tokens: 4000, accounted_tokens: 12, usage: { input_tokens: 10, output_tokens: 2, reasoning_tokens: null }, upstream_request_id: null, error_code: 'retryable_failure', created_at_ms: 1, settled_at_ms: 2 }
  vi.mocked(api.request).mockImplementation(async path => path.includes('/requests') ? { requests: [{
    request_id: 'managed-retry', origin: 'workload', source: 'user_provider', key_id: null, actor_user_id: 'collaborator', resource_owner_user_id: 'project-owner', model_beneficiary_user_id: 'project-owner', grant_id: null, grant_name: null, workload: { session_id: 'shared-session', run_id: 'shared-run' }, model_id: 'private-model', protocol: 'openai-responses', state: 'completed', attempted: true, reserved_tokens: 0, accounted_tokens: 32, usage: { input_tokens: 26, output_tokens: 6, reasoning_tokens: null }, error_code: null, month: '2026-09', created_at_ms: 1, expires_at_ms: 100, settled_at_ms: 5, attempts: [attempt, { ...attempt, attempt: 2, state: 'completed', accounted_tokens: 20, usage: { input_tokens: 16, output_tokens: 4, reasoning_tokens: null }, error_code: null }],
  }], next_cursor: null } : { month: '2026-09', request_count: 1, active_requests: 0, unknown_requests: 0, used_tokens: 32, reserved_tokens: 0, input_tokens: 26, output_tokens: 6, cached_input_tokens: 0, cache_write_tokens: 0, reasoning_tokens: 0 })
  await render(<ModelUsage admin />)
  const row = host.querySelector('[data-model-request="managed-retry"]')!
  expect(row.textContent).toContain('自备模型（BYOK）')
  expect(row.textContent).toContain('发起人：collaborator')
  expect(row.textContent).toContain('资源所有者：project-owner')
  expect(row.textContent).toContain('已结算 32 tok')
  expect(row.textContent).toContain('已结算 12 tok')
  expect(row.textContent).toContain('已结算 20 tok')
  expect(row.textContent).toContain('不会再次扣除')
  expect(row.textContent).not.toContain('平台预算')
  expect(row.textContent).not.toContain('undefined')
})


it('shows only unaccounted attempt reservations while a retried request is still pending', async () => {
  const attempt = { attempt: 1, state: 'failed', attempted: true, reserved_tokens: 4000, accounted_tokens: 12, usage: { input_tokens: 10, output_tokens: 2 }, error_code: null, created_at_ms: 1, settled_at_ms: 2 }
  vi.mocked(api.request).mockImplementation(async path => path.includes('/requests') ? { requests: [{
    request_id: 'pending-retry', origin: 'workload', source: 'platform_grant', actor_user_id: 'collaborator', resource_owner_user_id: 'owner', model_beneficiary_user_id: 'owner', grant_id: 'grant-one', grant_name: 'Shared budget', workload: { session_id: 'shared-session', run_id: 'shared-run' }, model_id: 'published', state: 'pending', attempted: true, reserved_tokens: 8000, accounted_tokens: null, usage: null, created_at_ms: 1,
    attempts: [attempt, { ...attempt, attempt: 2, state: 'pending', accounted_tokens: null, usage: null, settled_at_ms: null }],
  }], next_cursor: null } : { month: '2026-09', request_count: 1, active_requests: 1, unknown_requests: 0, used_tokens: 12, reserved_tokens: 4000, input_tokens: 10, output_tokens: 2, cached_input_tokens: 0, cache_write_tokens: 0, reasoning_tokens: 0 })
  await render(<ModelUsage />)
  const row = host.querySelector('[data-model-request="pending-retry"]')!
  expect(row.textContent).toContain('预留 4,000 tok')
  expect(row.textContent).not.toContain('预留 8,000 tok')
  expect(row.textContent).toContain('已结算 12 tok')
  expect(row.textContent).toContain('发起人：collaborator')
})


it('lets an administrator restrict a model budget to its recipient without changing its subject or quota', async () => {
  vi.mocked(api.request).mockImplementation(async path => path.startsWith('/admin/models/grants')
    ? { grants: [entitlement.grant], next_cursor: null }
    : path.startsWith('/admin/models/groups') ? { groups: [], next_cursor: null } : { models: entitlement.models, next_cursor: null })
  await render(<ModelGrants />)
  await click(button('编辑'))
  const dialog = document.querySelector('[role="dialog"]')!
  const sharing = [...dialog.querySelectorAll<HTMLLabelElement>('label')].find(label => label.textContent?.includes('允许协作者使用'))?.querySelector<HTMLInputElement>('input')
  expect(sharing).toBeDefined()
  expect(sharing!.checked).toBe(true)
  await click(sharing!)
  await act(async () => dialog.querySelector('form')!.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true })))
  expect(api.request).toHaveBeenCalledWith('/admin/models/grants/grant-one', {
    method: 'PUT', body: expect.objectContaining({ allow_resource_sharing: false, subject: { kind: 'group', id: 'global-group' }, monthly_tokens: 10_000, model_ids: ['alpha', 'beta'] }),
  })
})


it('keeps an administrator personal model keys outside the platform management navigation', async () => {
  workbench.serverIdentity.platform_role = 'owner'
  await render(<ModelAccessShell />)
  expect(host.querySelector('a[href="/admin/models"]')).toBeNull()
  expect(host.querySelector('a[href="/settings"]')).not.toBeNull()
  expect(host.textContent).toContain('用户设置')
  expect(host.textContent).toContain('创建接入密钥')
})
