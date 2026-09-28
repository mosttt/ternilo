import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { LocaleProvider } from '@/i18n/provider'
import { resetProviderInventoryForTest } from '@/domain/provider-inventory'
import type { CredentialInventory, ProviderProfile } from '@/types'
import { ModelsSettings } from './models-settings'

const mocks = vi.hoisted(() => ({ request: vi.fn() }))

vi.mock('./model-connections-settings', () => ({ ModelConnectionsSettings: () => null }))

vi.mock('@/api/client', () => ({ api: { request: mocks.request } }))
vi.mock('@/state/workbench', () => ({
  useWorkbench: () => ({
    currentSession: null,
    updateSession: vi.fn(),
    notify: vi.fn(),
    refresh: vi.fn(),
  }),
}))
vi.mock('@/components/workbench/model-picker', () => ({
  ModelPicker: () => null,
  modelLabel: () => 'Default',
  persistModelSelection: vi.fn(),
}))

const provider: ProviderProfile = {
  id: 'fixture', source: 'user', display_name: 'Fixture Provider', base_url: 'https://example.test/v1',
  protocol: 'openai-responses', api_key_ref: null,
  defaults: { context_window: 128_000, max_output_tokens: 16_000 },
  models: [{ id: 'fixture-model', settings: { mode: 'inherit' } }],
  timeout_ms: 30_000, max_attempts: 1, retry_base_delay_ms: 100,
}
const credentials: CredentialInventory = { references: [], records: [] }

let host: HTMLDivElement
let root: Root

beforeEach(() => {
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true
  resetProviderInventoryForTest()
  window.__TERNILO_BOOT__ = { providerAuthoring: true }
  host = document.createElement('div')
  document.body.append(host)
  root = createRoot(host)
})

afterEach(() => {
  act(() => root.unmount())
  host.remove()
  resetProviderInventoryForTest()
  mocks.request.mockReset()
  delete window.__TERNILO_BOOT__
  delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
})

function state() {
  return host.querySelector('[data-models-state]')?.getAttribute('data-models-state')
}

describe('ModelsSettings data states', () => {
  it('moves from an explicit loading state to ready data', async () => {
    let resolveProviders!: (value: ProviderProfile[]) => void
    let resolveCredentials!: (value: CredentialInventory) => void
    mocks.request
      .mockReturnValueOnce(new Promise(resolve => { resolveProviders = resolve }))
      .mockReturnValueOnce(new Promise(resolve => { resolveCredentials = resolve }))
    act(() => root.render(<LocaleProvider><ModelsSettings /></LocaleProvider>))
    expect(state()).toBe('loading')

    await act(async () => {
      resolveProviders([provider])
      resolveCredentials(credentials)
      await Promise.resolve()
      await Promise.resolve()
    })
    expect(state()).toBe('ready')
    expect(host.textContent).toContain('Fixture Provider')
  })

  it('distinguishes empty and error instead of presenting failed loads as empty', async () => {
    mocks.request.mockResolvedValueOnce([]).mockResolvedValueOnce(credentials)
    await act(async () => {
      root.render(<LocaleProvider><ModelsSettings /></LocaleProvider>)
      await Promise.resolve()
      await Promise.resolve()
    })
    expect(state()).toBe('empty')
    expect(host.textContent).toContain('尚未添加手动 Provider')

    act(() => root.unmount())
    resetProviderInventoryForTest()
    root = createRoot(host)
    mocks.request.mockRejectedValueOnce(new Error('provider catalog offline')).mockResolvedValueOnce(credentials)
    await act(async () => {
      root.render(<LocaleProvider><ModelsSettings /></LocaleProvider>)
      await Promise.resolve()
      await Promise.resolve()
    })
    expect(state()).toBe('error')
    expect(host.querySelector('[role="alert"]')?.textContent).toContain('provider catalog offline')
  })
})
