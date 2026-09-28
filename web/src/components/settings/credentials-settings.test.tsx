import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { LocaleProvider } from '@/i18n/provider'
import {
  loadProviderInventory,
  resetProviderInventoryForTest,
  subscribeProviderInventory,
} from '@/domain/provider-inventory'
import type { CredentialInventory, ProviderProfile } from '@/types'

const mocks = vi.hoisted(() => ({
  request: vi.fn(),
  notify: vi.fn(),
  logout: vi.fn(),
  workbench: { remote: false, platform: false },
}))

vi.mock('@/api/client', () => ({ api: { request: mocks.request } }))
vi.mock('@/state/workbench', () => ({ useWorkbench: () => ({
  notify: mocks.notify,
  logout: mocks.logout,
  remote: mocks.workbench.remote,
  platform: mocks.workbench.platform,
}) }))

import { CredentialsSettings } from './credentials-settings'

const credentials: CredentialInventory = { references: [], records: [] }
const provider: ProviderProfile = {
  id: 'fixture', source: 'user', display_name: 'Fixture', base_url: 'https://example.test/v1',
  protocol: 'openai-responses', api_key_ref: 'FIXTURE_KEY',
  defaults: { context_window: 128_000, max_output_tokens: 16_000 },
  models: [{ id: 'fixture-model', settings: { mode: 'inherit' } }],
  timeout_ms: 30_000, max_attempts: 1, retry_base_delay_ms: 100,
}

let host: HTMLDivElement
let root: Root
let authorization: {
  entries: Array<{
    key: { space: string; key: string }
    label: string
    methods: Array<{ id: string; label: string }>
    in_flight: boolean
    configured: boolean
    writable: boolean
  }>
  attempts: never[]
  notices: never[]
  prompts: never[]
} = { entries: [], attempts: [], notices: [], prompts: [] }
let providerReads = 0

function setInput(input: HTMLInputElement, value: string) {
  Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')?.set?.call(input, value)
  input.dispatchEvent(new Event('input', { bubbles: true }))
}

async function settle() {
  await act(async () => { await Promise.resolve(); await Promise.resolve() })
}

beforeEach(() => {
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true
  resetProviderInventoryForTest()
  providerReads = 0
  authorization = { entries: [], attempts: [], notices: [], prompts: [] }
  mocks.request.mockReset()
  mocks.notify.mockReset()
  mocks.logout.mockReset()
  mocks.workbench.remote = false
  mocks.workbench.platform = false
  mocks.request.mockImplementation(async (path: string, options?: { method?: string }) => {
    if (path === '/providers') {
      providerReads += 1
      return [provider]
    }
    if (path === '/credentials') return options?.method === 'POST' ? undefined : credentials
    if (path.startsWith('/authorizations?')) return authorization
    throw new Error(`unexpected request: ${options?.method ?? 'GET'} ${path}`)
  })
  host = document.createElement('div')
  document.body.append(host)
  root = createRoot(host)
})

afterEach(() => {
  act(() => root.unmount())
  host.remove()
  resetProviderInventoryForTest()
  vi.restoreAllMocks()
  delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
})

describe('CredentialsSettings Provider readiness synchronization', () => {
  it('offers remote token logout from Credentials settings only for Relay connections', async () => {
    mocks.workbench.remote = true
    act(() => root.render(<LocaleProvider><CredentialsSettings /></LocaleProvider>))
    await settle()
    const signOut = Array.from(host.querySelectorAll('button')).find(button => button.textContent === '注销远程连接')
    expect(signOut).toBeDefined()

    act(() => signOut!.click())

    expect(mocks.logout).toHaveBeenCalledOnce()
  })

  it('keeps the failure surface mounted while a retry is pending', async () => {
    let authorizationRead = 0
    let resolveRetry!: (value: typeof authorization) => void
    const retry = new Promise<typeof authorization>(resolve => { resolveRetry = resolve })
    mocks.request.mockImplementation(async (path: string, options?: { method?: string }) => {
      if (path === '/credentials') return options?.method === 'POST' ? undefined : credentials
      if (path.startsWith('/authorizations?')) {
        authorizationRead += 1
        if (authorizationRead === 1) throw new Error('authorization unavailable')
        return retry
      }
      throw new Error(`unexpected request: ${options?.method ?? 'GET'} ${path}`)
    })
    act(() => root.render(<LocaleProvider><CredentialsSettings /></LocaleProvider>))
    await vi.waitFor(() => expect(host.querySelector('[role="alert"]')?.textContent).toContain('authorization unavailable'))

    const retryButton = Array.from(host.querySelectorAll('button')).find(button => button.textContent === '重试')!
    act(() => retryButton.click())

    expect(host.querySelector('[role="alert"]')?.textContent).toContain('authorization unavailable')
    expect(retryButton.disabled).toBe(true)
    expect(retryButton.textContent).toBe('加载中…')

    await act(async () => {
      resolveRetry(authorization)
      await retry
    })
    expect(host.querySelector('[role="alert"]')).toBeNull()
  })

  it('refreshes the shared Provider inventory immediately after saving a credential', async () => {
    await loadProviderInventory()
    const observed: Array<ProviderProfile[] | null> = []
    const unsubscribe = subscribeProviderInventory(snapshot => observed.push(snapshot?.providers ?? null))
    act(() => root.render(<LocaleProvider><CredentialsSettings /></LocaleProvider>))
    await settle()

    act(() => {
      setInput(host.querySelector<HTMLInputElement>('#credential-name')!, 'FIXTURE_KEY')
      setInput(host.querySelector<HTMLInputElement>('#credential-value')!, 'secret')
    })
    await act(async () => {
      host.querySelector<HTMLButtonElement>('form button[type="submit"]')!.click()
      await vi.waitFor(() => expect(providerReads).toBe(2))
    })

    expect(observed).toEqual([null, [provider]])
    expect(mocks.notify).toHaveBeenCalled()
    unsubscribe()
  })

  it('refreshes Provider readiness when an interactive authorization finishes', async () => {
    let poll: (() => void) | undefined
    vi.spyOn(window, 'setInterval').mockImplementation(handler => {
      poll = handler as () => void
      return {} as ReturnType<typeof window.setInterval>
    })
    authorization = {
      entries: [{
        key: { space: 'provider', key: 'fixture' }, label: 'Fixture login', methods: [],
        in_flight: true, configured: false, writable: true,
      }],
      attempts: [], notices: [], prompts: [],
    }
    act(() => root.render(<LocaleProvider><CredentialsSettings /></LocaleProvider>))
    await settle()
    expect(poll).toBeTypeOf('function')

    authorization = {
      ...authorization,
      entries: authorization.entries.map(entry => ({ ...entry, in_flight: false, configured: true })),
    }
    await act(async () => {
      poll?.()
      await vi.waitFor(() => expect(providerReads).toBe(1))
    })

    expect(providerReads).toBe(1)
  })
})
