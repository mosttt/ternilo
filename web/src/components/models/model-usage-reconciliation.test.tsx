import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { api, ApiError } from '@/api/client'
import { LocaleProvider } from '@/i18n/provider'
import type { ModelServiceRequest } from './model-service-api'
import { ModelUsageReconciliationDialog } from './model-usage-reconciliation'

const request: ModelServiceRequest = {
  request_id: 'request/one', origin: 'api_key', source: 'platform_grant', key_id: 'key', actor_user_id: 'actor',
  resource_owner_user_id: null, model_beneficiary_user_id: 'actor', workload: null, grant_id: 'grant', grant_name: 'Grant',
  model_id: 'model', protocol: 'openai-chat-completions', state: 'failed', attempted: true, reserved_tokens: 200,
  accounted_tokens: null, usage: null, month: '2026-09', created_at_ms: 1, expires_at_ms: 10, settled_at_ms: 2,
  attempts: [{ attempt: 1, state: 'failed', attempted: true, reserved_tokens: 200, accounted_tokens: null,
    usage: null, created_at_ms: 1, settled_at_ms: 2 }],
}
let host: HTMLDivElement
let root: Root
const saved = vi.fn()
beforeEach(() => {
  vi.stubGlobal('IS_REACT_ACT_ENVIRONMENT', true)
  vi.stubGlobal('ResizeObserver', class { observe() {} unobserve() {} disconnect() {} })
  host = document.createElement('div'); document.body.append(host); root = createRoot(host)
  saved.mockClear()
  vi.spyOn(api, 'request').mockResolvedValue([])
})
afterEach(() => { act(() => root.unmount()); host.remove(); vi.restoreAllMocks(); vi.unstubAllGlobals() })
async function render(editable = true) {
  await act(async () => root.render(<LocaleProvider><ModelUsageReconciliationDialog request={request} attempt={request.attempts[0]} editable={editable} onSaved={saved} onClose={vi.fn()} /></LocaleProvider>))
}
function input(label: string) {
  const element = [...document.querySelectorAll('label')].find(item => item.textContent === label)!
  return document.getElementById(element.htmlFor) as HTMLInputElement | HTMLTextAreaElement
}
function fill(label: string, value: string) {
  const element = input(label)
  const prototype = element instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype
  act(() => {
    Object.getOwnPropertyDescriptor(prototype, 'value')!.set!.call(element, value)
    element.dispatchEvent(new Event('input', { bubbles: true }))
  })
}
async function submit() { await act(async () => document.querySelector('form')!.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true }))) }

it('requires explicit known counters and preserves a real zero without substituting the reservation', async () => {
  await render()
  expect(input('输入').value).toBe('')
  expect(input('输出').value).toBe('')
  fill('核对依据', 'provider/report-one'); fill('核对说明', 'Confirmed upstream usage')
  await submit()
  expect(api.request).toHaveBeenCalledTimes(1)
  expect(document.querySelector('[role="alert"]')?.textContent).toContain('输入与输出用量不能留空')
  fill('输入', '70'); fill('输出', '0')
  vi.mocked(api.request).mockImplementationOnce(async (_path, options) => ({ reconciliation: {
    attempt: 1, actor_user_id: 'owner', reconciled_at_ms: 3, previous_usage: null, input: options?.body,
  } }) as never)
  await submit()
  expect(api.request).toHaveBeenLastCalledWith('/admin/models/requests/request%2Fone/attempts/1/reconcile', {
    method: 'POST', body: { expected_settled_at_ms: 2, reference: 'provider/report-one', note: 'Confirmed upstream usage',
      usage: { input_tokens: 70, output_tokens: 0, cached_input_tokens: null, cache_write_tokens: null, reasoning_tokens: null } },
  })
  expect(saved).toHaveBeenCalledTimes(1)
  expect(document.querySelector('form')).toBeNull()
  expect(document.querySelector('[data-usage-reconciliation]')?.textContent).toContain('provider/report-one')
})

it('stops editing and refreshes the ledger when trusted late usage wins the race', async () => {
  await render()
  fill('输入', '70'); fill('输出', '30'); fill('核对依据', 'provider/report-one'); fill('核对说明', 'Confirmed')
  vi.mocked(api.request).mockRejectedValueOnce(new ApiError('known model usage cannot be overwritten', 409, 'conflict'))
  await submit()
  expect(saved).toHaveBeenCalledTimes(1)
  expect(document.querySelector('form')).toBeNull()
  expect(document.querySelector('[role="alert"]')?.textContent).toContain('known model usage cannot be overwritten')
})

it('keeps the reconciliation form unavailable to read-only administrators', async () => {
  await render(false)
  expect(document.querySelector('form')).toBeNull()
  expect(api.request).toHaveBeenCalledTimes(1)
})
