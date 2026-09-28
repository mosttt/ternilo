import { act, useState } from 'react'
import { createRoot } from 'react-dom/client'
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { selectChoice, setupChoiceSelect } from '@/test/choice-select'
import { LocaleProvider } from '@/i18n/provider'
import type { ModelEntitlement } from './model-service-api'
import type { ModelDeviceScope } from './model-device-types'
import { ModelDeviceScopeEditor } from './model-device-scope-editor'

let dispose = () => {}
beforeEach(setupChoiceSelect)
afterEach(() => { dispose(); vi.unstubAllGlobals(); vi.restoreAllMocks() })

it('requires an explicit restricted scope and preserves independent grant selections', async () => {
  vi.stubGlobal('IS_REACT_ACT_ENVIRONMENT', true)
  const host = document.createElement('div')
  document.body.append(host)
  const root = createRoot(host)
  dispose = () => { act(() => root.unmount()); host.remove() }
  let selected: ModelDeviceScope = { kind: 'account' }
  const grants: ModelEntitlement[] = ['alpha', 'beta'].map(id => ({
    grant: {
      grant_id: id, name: id, subject: { kind: 'user', id: 'owner' }, subject_name: 'Owner',
      allow_resource_sharing: false, model_ids: ['same-model'], expires_at_ms: null, revoked_at_ms: null,
      created_at_ms: 0, updated_at_ms: 0,
      quota: { month: '2026-09', limit_tokens: 10000, used_tokens: 0, reserved_tokens: 0, active_requests: 0, max_concurrent_requests: 2 },
    },
    models: [{ model_id: 'same-model', display_name: 'Same model', protocol: 'openai-responses', defaults: { context_window: 32000, max_output_tokens: 2048 } }],
  }))
  function Editor() {
    const [scope, setScope] = useState<ModelDeviceScope>({ kind: 'account' })
    return <ModelDeviceScopeEditor scope={scope} grants={grants} onChange={value => { selected = value; setScope(value) }} />
  }
  await act(async () => root.render(<LocaleProvider><Editor /></LocaleProvider>))
  expect(host.textContent).toContain('之后新增的模型授权也可在此设备使用')
  await selectChoice(host.querySelector<HTMLElement>('[role="combobox"]')!, 'selected')
  expect(selected).toEqual({ kind: 'selected', grants: [] })
  for (const id of ['alpha', 'beta']) await act(async () => host.querySelector<HTMLInputElement>(`[data-device-grant="${id}"] input`)!.click())
  expect(selected).toEqual({ kind: 'selected', grants: [
    { grant_id: 'alpha', model_ids: ['same-model'] }, { grant_id: 'beta', model_ids: ['same-model'] },
  ] })
  await act(async () => host.querySelector<HTMLInputElement>('[data-device-grant="alpha"] input')!.click())
  expect(selected).toEqual({ kind: 'selected', grants: [{ grant_id: 'beta', model_ids: ['same-model'] }] })
})

it('requires explicit opt-in for future private models and keeps private scopes separate from same-name grants', async () => {
  vi.stubGlobal('IS_REACT_ACT_ENVIRONMENT', true)
  const host = document.createElement('div'); document.body.append(host)
  const root = createRoot(host)
  dispose = () => { act(() => root.unmount()); host.remove() }
  let selected: ModelDeviceScope = { kind: 'account' }
  function Editor() {
    const [scope, setScope] = useState<ModelDeviceScope>({ kind: 'account' })
    return <ModelDeviceScopeEditor scope={scope} grants={[]} providers={[{ provider_id: 'private', provider_name: 'Private', models: [{ model_id: 'same', display_name: 'Same', protocol: 'openai-responses', defaults: { context_window: 32000, max_output_tokens: 2048 } }] }]} onChange={value => { selected = value; setScope(value) }} />
  }
  await act(async () => root.render(<LocaleProvider><Editor /></LocaleProvider>))
  const optIn = host.querySelector<HTMLInputElement>('input[type="checkbox"]')!
  expect(optIn.checked).toBe(false)
  await act(async () => optIn.click())
  expect(selected).toEqual({ kind: 'account', include_account_providers: true })
  await selectChoice(host.querySelector<HTMLElement>('[role="combobox"]')!, 'selected')
  await act(async () => host.querySelector<HTMLInputElement>('[data-device-provider="private"] input')!.click())
  expect(selected).toEqual({ kind: 'selected', grants: [], providers: [{ provider_id: 'private', model_ids: ['same'] }] })
  expect(host.textContent).toContain('不扣平台额度')
  await act(async () => host.querySelectorAll<HTMLInputElement>('[data-device-provider="private"] input')[1].click())
  expect(selected).toEqual({ kind: 'selected', grants: [], providers: [] })
})
