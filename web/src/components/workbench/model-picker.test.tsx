import { act, useState } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { CredentialInventory, ModelSelection, ProviderProfile, Workspace } from '@/types'
import { invalidateProviderInventory, loadProviderInventory, resetProviderInventoryForTest } from '@/domain/provider-inventory'
import { navigate } from '@/app/navigation'

const fixture = vi.hoisted(() => ({
  request: vi.fn(),
  updateSession: vi.fn(async () => undefined),
  notify: vi.fn(),
  platform: false,
  currentTenantId: undefined as string | undefined,
  hasSession: true,
  placement: undefined as 'cloud' | 'local_node' | undefined,
  model: { provider: 'profile_default' } as Record<string, unknown>,
  workspace: undefined as Workspace | undefined,
}))

vi.mock('@/api/client', () => ({ api: { request: fixture.request } }))
vi.mock('@/state/workbench', () => ({
  useWorkbench: () => ({
    platform: fixture.platform,
    serverIdentity: fixture.platform ? { user: { user_id: 'actor' }, personal_tenant_id: 'personal' } : undefined,
    currentTenantId: fixture.currentTenantId,
    currentSession: fixture.hasSession ? { identity: { session_id: 'session-1' }, model: fixture.model, placement: fixture.placement } : null,
    currentWorkspace: fixture.workspace,
    catalog: {
      revision: 'fixture', plugin_kinds: ['ternilo.model.rule', 'ternilo.model.openai_compatible'],
      plugins: ['ternilo.model.rule', 'ternilo.model.openai_compatible'].map(kind => ({
        kind, description: '', requires: [], provides: ['ternilo/models@3'], config_schema: {},
      })),
    },
    updateSession: fixture.updateSession,
    notify: fixture.notify,
  }),
}))

import { ModelPicker, persistModelSelection, withReasoningEffort, type ModelReadiness } from './model-picker'

let host: HTMLDivElement
let root: Root

const inventory: CredentialInventory = { references: [], records: [] }
const provider: ProviderProfile = {
  id: 'no-key', source: 'user', display_name: 'No key', base_url: 'https://example.test/v1',
  protocol: 'openai-responses', api_key_ref: null,
  defaults: { context_window: 128_000, max_output_tokens: 16_000 },
  models: [{ id: 'model-a', settings: { mode: 'inherit' } }],
  timeout_ms: 30_000, max_attempts: 2, retry_base_delay_ms: 250,
}

async function settle() {
  await act(async () => { await Promise.resolve(); await Promise.resolve() })
}

beforeEach(() => {
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true
  fixture.request.mockReset()
  fixture.updateSession.mockReset().mockResolvedValue(undefined)
  fixture.notify.mockReset()
  resetProviderInventoryForTest()
  fixture.platform = false
  fixture.currentTenantId = undefined
  fixture.hasSession = true
  fixture.placement = undefined
  fixture.model = { provider: 'profile_default' }
  fixture.workspace = undefined
  vi.stubGlobal('ResizeObserver', class {
    observe() {}
    unobserve() {}
    disconnect() {}
  })
  host = document.createElement('div')
  document.body.append(host)
  root = createRoot(host)
})

afterEach(() => {
  act(() => root.unmount())
  host.remove()
  document.querySelectorAll('[data-radix-popper-content-wrapper]').forEach(node => node.remove())
  vi.restoreAllMocks()
  vi.unstubAllGlobals()
  history.replaceState({}, '', '/')
  delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
})

async function chooseProviderModel() {
  act(() => host.querySelector<HTMLButtonElement>('button')!.dispatchEvent(
    new MouseEvent('pointerdown', { bubbles: true, cancelable: true, button: 0 }),
  ))
  const modelMenu = [...document.querySelectorAll<HTMLElement>('[role="menuitem"]')]
    .find(item => item.textContent?.trim().startsWith('模型'))
  expect(modelMenu).toBeDefined()
  modelMenu!.focus()
  act(() => modelMenu!.dispatchEvent(new KeyboardEvent('keydown', {
    key: 'ArrowRight', bubbles: true, cancelable: true,
  })))
  await settle()
  const model = [...document.querySelectorAll<HTMLElement>('[role="menuitem"][data-model-source="delegated"], [data-model-provider] [role="menuitem"]')]
    .find(item => item.textContent?.includes('model-a'))
  expect(model).toBeDefined()
  await act(async () => {
    model!.dispatchEvent(new MouseEvent('click', { bubbles: true, cancelable: true }))
    await Promise.resolve()
  })
}

describe('ModelPicker readiness', () => {
  it('closes a controlled menu on navigation even while its workbench stays mounted', async () => {
    fixture.request.mockImplementation(async (path: string) => path.startsWith('/providers') ? [provider] : inventory)
    function PersistentPicker() {
      const [open, setOpen] = useState(true)
      return <ModelPicker open={open} onOpenChange={setOpen} />
    }
    await act(async () => root.render(<PersistentPicker />))
    await settle()
    expect(document.querySelector('[role="menu"]')).not.toBeNull()
    await act(async () => navigate('/models'))
    expect(document.querySelector('[role="menu"]')).toBeNull()
    await act(async () => navigate('/'))
    expect(document.querySelector('[role="menu"]')).toBeNull()
  })

  it('waits for touch release before exposing menu actions beneath the tapping finger', async () => {
    fixture.request.mockImplementation(async (path: string) => path.startsWith('/providers') ? [provider] : inventory)
    const configure = vi.fn()
    await act(async () => root.render(<ModelPicker onConfigureModels={configure} />))
    await settle()
    const trigger = host.querySelector('button')!
    const touch = new MouseEvent('pointerdown', { bubbles: true, cancelable: true, button: 0 })
    Object.defineProperty(touch, 'pointerType', { value: 'touch' })
    act(() => trigger.dispatchEvent(touch))
    expect(document.querySelector('[role="menu"]')).toBeNull()
    act(() => trigger.click())
    expect(document.querySelector('[role="menu"]')).not.toBeNull()
    expect(configure).not.toHaveBeenCalled()
  })

  it('changes a shared session model without changing its machine default', async () => {
    fixture.request.mockImplementation(async (path: string) => path.startsWith('/providers') ? [provider] : inventory)
    await act(async () => root.render(<ModelPicker rememberDefault={false} />))
    await settle()
    await chooseProviderModel()
    expect(fixture.updateSession).toHaveBeenCalledWith('session-1', { model: { provider: 'named_provider', provider_id: 'no-key', model: 'model-a' } })
    expect(fixture.request.mock.calls.some(([path]) => String(path).startsWith('/default-model'))).toBe(false)
  })

  it('updates the current Session before persisting the shared default model', async () => {
    const order: string[] = []
    const selection = { provider: 'named_provider' as const, provider_id: 'no-key', model: 'model-a' }
    const update = vi.fn(async () => { order.push('session') })
    const request = vi.fn(async () => { order.push('default') })

    await persistModelSelection('session/one', selection, update, request)

    expect(order).toEqual(['session', 'default'])
    expect(update).toHaveBeenCalledWith('session/one', { model: selection })
    expect(request).toHaveBeenCalledWith('/default-model?session_id=session%2Fone', {
      method: 'PUT', body: selection,
    })
  })

  it('reports when the Session changes but saving the shared default fails', async () => {
    fixture.request.mockImplementation(async (path: string) => {
      if (path.startsWith('/providers?')) return [provider]
      if (path.startsWith('/credentials?')) return inventory
      if (path.startsWith('/default-model?')) throw new Error('default offline')
      throw new Error(`unexpected request ${path}`)
    })
    act(() => root.render(<ModelPicker />))
    await settle()

    await chooseProviderModel()

    expect(fixture.updateSession).toHaveBeenCalledWith('session-1', {
      model: { provider: 'named_provider', provider_id: 'no-key', model: 'model-a' },
    })
    expect(fixture.notify).toHaveBeenCalledWith(
      '当前会话已更新，但默认模型保存失败：default offline',
      'error',
    )
  })

  it('keeps the original error when the Session model update fails', async () => {
    fixture.updateSession.mockRejectedValue(new Error('session offline'))
    fixture.request.mockImplementation(async (path: string) => {
      if (path.startsWith('/providers?')) return [provider]
      if (path.startsWith('/credentials?')) return inventory
      if (path.startsWith('/default-model?')) throw new Error('default should not run')
      throw new Error(`unexpected request ${path}`)
    })
    act(() => root.render(<ModelPicker />))
    await settle()

    await chooseProviderModel()

    expect(fixture.notify).toHaveBeenCalledWith('session offline', 'error')
    expect(fixture.request).not.toHaveBeenCalledWith(
      expect.stringContaining('/default-model?'),
      expect.anything(),
    )
  })

  it('exposes error and retry before settling on a distinct empty state', async () => {
    fixture.model = { provider: 'named_provider', provider_id: 'missing', model: 'missing' }
    let fail = true
    fixture.request.mockImplementation(async (path: string) => {
      if (fail) throw new Error('catalog offline')
      return path.startsWith('/providers?') ? [] : inventory
    })
    const observed: ModelReadiness[] = []
    act(() => root.render(<ModelPicker onReadinessChange={value => observed.push(value)} />))
    await settle()
    expect(observed.at(-1)?.status).toBe('error')
    expect(observed.at(-1)?.error).toBe('catalog offline')
    fail = false
    act(() => observed.at(-1)?.retry())
    await settle()
    expect(observed.at(-1)?.status).toBe('empty')
    expect(observed.at(-1)?.canSubmit).toBe(false)
  })

  it('marks a no-key Provider ready without relying on api_key_ref truthiness', async () => {
    fixture.model = { provider: 'named_provider', provider_id: 'no-key', model: 'model-a' }
    fixture.request.mockImplementation(async (path: string) => path.startsWith('/providers?') ? [provider] : inventory)
    const observed: ModelReadiness[] = []
    act(() => root.render(<ModelPicker onReadinessChange={value => observed.push(value)} />))
    await settle()
    expect(observed.at(-1)?.status).toBe('ready')
    expect(observed.at(-1)?.canSubmit).toBe(true)
  })

  it.each([false, true])('reloads models when the selected Node reconnects (offline read pending: %s)', async pending => {
    fixture.model = { provider: 'named_provider', provider_id: 'no-key', model: 'model-a' }
    fixture.workspace = {
      workspace_id: 'workspace-1', path: '/project', title: 'Project',
      created_at_ms: 0, updated_at_ms: 0, placement: 'local_node', node_id: 'node-1', status: 'offline',
    }
    let rejectOffline!: (cause: Error) => void
    const offline = pending
      ? new Promise((_, reject) => { rejectOffline = reject })
      : null
    fixture.request.mockImplementation(async () => {
      if (offline) return offline
      throw new Error('selected Ternilo node is offline')
    })
    const observed: ModelReadiness[] = []
    const onReadinessChange = (value: ModelReadiness) => { observed.push(value) }
    act(() => root.render(<ModelPicker onReadinessChange={onReadinessChange} />))
    await settle()
    expect(observed.at(-1)?.canSubmit).toBe(false)
    if (!pending) expect(observed.at(-1)?.status).toBe('error')

    fixture.request.mockImplementation(async (path: string) => path.startsWith('/providers?') ? [provider] : inventory)
    fixture.workspace = { ...fixture.workspace, status: 'online' }
    act(() => root.render(<ModelPicker onReadinessChange={onReadinessChange} />))
    await settle()
    if (pending) {
      await act(async () => { rejectOffline(new Error('selected Ternilo node is offline')) })
    }
    expect(observed.at(-1)).toMatchObject({ status: 'ready', canSubmit: true, error: '' })
    expect(fixture.request).toHaveBeenCalledTimes(4)

    fixture.workspace = { ...fixture.workspace, updated_at_ms: 1 }
    act(() => root.render(<ModelPicker onReadinessChange={onReadinessChange} />))
    await settle()
    expect(fixture.request).toHaveBeenCalledTimes(4)
  })

  it('updates an already mounted picker after Provider settings refresh the shared inventory', async () => {
    fixture.model = { provider: 'named_provider', provider_id: 'no-key', model: 'model-a' }
    fixture.request.mockImplementation(async (path: string) => path.startsWith('/providers?') ? [provider] : inventory)
    act(() => root.render(<ModelPicker />))
    await settle()
    expect(host.textContent).toContain('model-a')

    const refreshed = {
      ...provider,
      models: [{ id: 'model-a', display_name: 'Model A', settings: { mode: 'inherit' as const } }],
    }
    fixture.request.mockImplementation(async (path: string) => path.startsWith('/providers?') ? [refreshed] : inventory)
    await act(async () => {
      invalidateProviderInventory({ sessionId: 'session-1' })
      await loadProviderInventory(true, { sessionId: 'session-1' })
    })

    expect(host.textContent).toContain('Model A')
  })

  it.each([false, true])('checks a remotely selected model missing from its cached catalog once (model exists: %s)', async exists => {
    fixture.model = { provider: 'named_provider', provider_id: 'no-key', model: 'model-a' }
    fixture.request.mockImplementation(async (path: string) => path.startsWith('/providers?') ? [provider] : inventory)
    const observed: ModelReadiness[] = []
    const onReadinessChange = (value: ModelReadiness) => { observed.push(value) }
    act(() => root.render(<ModelPicker onReadinessChange={onReadinessChange} />))
    await settle()
    expect(observed.at(-1)?.canSubmit).toBe(true)

    const refreshed = { ...provider, models: exists ? [...provider.models, { id: 'model-b', display_name: 'Shared model B', settings: { mode: 'inherit' as const } }] : provider.models }
    fixture.request.mockImplementation(async (path: string) => path.startsWith('/providers?') ? [refreshed] : inventory)
    fixture.model = { provider: 'named_provider', provider_id: 'no-key', model: 'model-b' }
    act(() => root.render(<ModelPicker onReadinessChange={onReadinessChange} />))
    await settle()
    expect(observed.at(-1)?.canSubmit).toBe(exists)
    expect(host.textContent).toContain(exists ? 'Shared model B' : 'model-b')
    expect(fixture.request.mock.calls.map(([path]) => path)).toEqual([
      '/providers?session_id=session-1', '/credentials?session_id=session-1',
      '/providers?session_id=session-1', '/credentials?session_id=session-1',
    ])
    for (let index = 0; index < 3; index++) {
      act(() => root.render(<ModelPicker onReadinessChange={onReadinessChange} />))
      await settle()
    }
    expect(fixture.request).toHaveBeenCalledTimes(4)
    expect(fixture.updateSession).not.toHaveBeenCalled()
  })

  it('keeps the host-owned startup Profile runnable while its Provider catalog loads or fails', async () => {
    let reject!: (cause: Error) => void
    fixture.request.mockImplementation(() => new Promise((_, fail) => { reject = fail }))
    const observed: ModelReadiness[] = []
    act(() => root.render(<ModelPicker effectiveProfile={{ plugins: [{
      id: 'model', kind: 'ternilo.model.openai_compatible', enabled: true,
      config: { base_url: 'http://127.0.0.1:1234/v1', model: 'fixture' },
    }] }} onReadinessChange={value => observed.push(value)} />))
    expect(observed.at(-1)).toMatchObject({ status: 'ready', canSubmit: true })
    await act(async () => { reject(new Error('catalog offline')); await Promise.resolve() })
    expect(observed.at(-1)).toMatchObject({ status: 'ready', canSubmit: true })
  })

  it('shows model setup for the default rule model and becomes ready after choosing a configured provider', async () => {
    fixture.request.mockImplementation(async (path: string) => path.startsWith('/providers?') ? [] : inventory)
    const observed: ModelReadiness[] = []
    const profile = { plugins: [{ id: 'model', kind: 'ternilo.model.rule', enabled: true, config: {} }] }
    await act(async () => root.render(<ModelPicker effectiveProfile={profile} onReadinessChange={value => observed.push(value)} />))
    await settle()
    expect(observed.at(-1)).toMatchObject({ status: 'empty', canSubmit: false })
    expect(host.textContent).toContain('配置模型')
    fixture.request.mockImplementation(async (path: string) => path.startsWith('/providers?') ? [provider] : inventory)
    await act(async () => { invalidateProviderInventory({ sessionId: 'session-1' }); await loadProviderInventory(true, { sessionId: 'session-1' }) })
    fixture.model = { provider: 'named_provider', provider_id: 'no-key', model: 'model-a' }
    await act(async () => root.render(<ModelPicker effectiveProfile={profile} onReadinessChange={value => observed.push(value)} />))
    await settle()
    expect(observed.at(-1)).toMatchObject({ status: 'ready', canSubmit: true })
  })
})

describe('Cloud model selection', () => {
  const publicModel = { model_id: 'same-model', display_name: 'Published model', protocol: 'openai-responses', defaults: { context_window: 100_000, max_output_tokens: 4000, reasoning: { default_effort: 'medium', efforts: { low: 'low', medium: 'medium', high: 'high' } } } }
  const option = (grant: string) => ({ grant_id: grant, grant_name: `Budget ${grant}`, model: publicModel })
  function cloud(current: ModelSelection = { provider: 'platform_model', grant_id: 'grant-a', model_id: 'same-model' }) {
    fixture.platform = true
    fixture.placement = 'cloud'
    fixture.model = current
  }
  async function openPlatformDirectory() {
    act(() => host.querySelector<HTMLButtonElement>('button')!.dispatchEvent(new MouseEvent('pointerdown', { bubbles: true, cancelable: true, button: 0 })))
    await settle()
    const models = [...document.querySelectorAll<HTMLElement>('[role="menuitem"]')].find(item => item.textContent?.startsWith('模型'))!
    models.focus()
    act(() => models.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowRight', bubbles: true, cancelable: true })))
    await settle()
    expect(document.querySelector('[role="dialog"]')).toBeNull()
    return document.querySelector('[data-model-source="platform"]')!
  }

  it.each([true, false])('uses Server authorization readiness on an attached computer (available: %s)', async available => {
    cloud()
    fixture.placement = 'local_node'
    fixture.request.mockImplementation(async (path: string) => path.startsWith('/model-options')
      ? { current: { selection: fixture.model, model: publicModel, selectable_reasoning: publicModel.defaults.reasoning, source_name: 'Node budget', available }, options: [option('grant-b')], next_cursor: null }
      : path.startsWith('/providers') ? [] : inventory)
    const observed: ModelReadiness[] = []
    await act(async () => root.render(<ModelPicker onReadinessChange={value => observed.push(value)} />))
    await settle()
    expect(observed.at(-1)?.canSubmit).toBe(available)
    expect(host.textContent).toContain('平台授权')
    expect(host.textContent).not.toContain('设备本地')
    const dialog = await openPlatformDirectory()
    const choice = [...dialog.querySelectorAll<HTMLElement>('[data-model-grant="grant-b"] [role="menuitem"]')].find(button => button.textContent?.includes('Published model'))!
    await act(async () => choice.click())
    expect(fixture.updateSession).toHaveBeenCalledWith('session-1', { model: { provider: 'platform_model', grant_id: 'grant-b', model_id: 'same-model' } })
    expect(fixture.request.mock.calls.some(([path]) => String(path).startsWith('/default-model'))).toBe(false)
  })

  it('uses the resource owner current model outside the page and does not require a collaborator key', async () => {
    cloud()
    fixture.request.mockImplementation(async (path: string) => path.startsWith('/model-options') ? { current: { selection: fixture.model, model: publicModel, selectable_reasoning: publicModel.defaults.reasoning, source_name: 'Owner project budget', available: true }, options: [], next_cursor: null } : path.startsWith('/providers') ? [] : inventory)
    const observed: ModelReadiness[] = []
    await act(async () => root.render(<ModelPicker rememberDefault={false} onReadinessChange={value => observed.push(value)} />))
    await settle()
    expect(observed.at(-1)).toMatchObject({ status: 'ready', canSubmit: true })
    expect(host.textContent).toContain('Published model · Owner project budget · medium')
    expect(fixture.request.mock.calls.some(([path]) => String(path).startsWith('/model-access'))).toBe(false)
    expect(fixture.request.mock.calls.filter(([path]) => String(path).startsWith('/model-options'))).toHaveLength(1)
  })

  it('selects the actor account catalog without reading execution-space Providers or credentials', async () => {
    cloud({ provider: 'profile_default' })
    fixture.request.mockImplementation(async (path: string, options: { headers?: Record<string, string> }) => {
      if (path.startsWith('/model-options')) return { current: null, options: [], next_cursor: null, providers: [], credentials: inventory }
      expect(options.headers?.['x-ternilo-tenant']).toBe('personal')
      expect(path).not.toContain('session_id')
      return path === '/providers' ? [provider] : inventory
    })
    await act(async () => root.render(<ModelPicker rememberDefault={false} />))
    await chooseProviderModel()
    expect(fixture.updateSession).toHaveBeenCalledWith('session-1', { model: { provider: 'account_provider', owner_user_id: 'actor', provider_id: 'no-key', model: 'model-a' } })
    expect(fixture.notify).not.toHaveBeenCalledWith(expect.anything(), 'error')
  })

  it('separates delegated sources for identical session IDs in different spaces', async () => {
    const selected = (team: string) => ({ provider: 'account_provider' as const, owner_user_id: `${team}-owner`, provider_id: 'same-provider', model: 'model-a' })
    cloud(selected('first-team'))
    fixture.currentTenantId = 'first-team'
    fixture.request.mockImplementation(async (path: string, options: { headers?: Record<string, string> }) => {
      if (!path.startsWith('/model-options')) return path === '/providers' ? [] : inventory
      expect(path).toContain('session_id=session-1')
      const team = options.headers?.['x-ternilo-tenant'] ?? ''
      return { current: { selection: selected(team), owner_user_id: `${team}-owner`, source_name: team, available: true,
        model: { ...publicModel, model_id: 'model-a', display_name: 'model-a' }, selectable_reasoning: null },
        options: [], next_cursor: null, providers: [], credentials: inventory }
    })
    await act(async () => root.render(<ModelPicker rememberDefault={false} />))
    await chooseProviderModel()
    expect(fixture.updateSession).toHaveBeenLastCalledWith('session-1', { model: selected('first-team') })
    fixture.currentTenantId = 'second-team'
    fixture.model = selected('second-team')
    await act(async () => root.render(<ModelPicker rememberDefault={false} />))
    await chooseProviderModel()
    expect(fixture.updateSession).toHaveBeenLastCalledWith('session-1', { model: selected('second-team') })
    expect(fixture.request.mock.calls.some(([, options]) => options.headers?.['x-ternilo-tenant'] === 'second-team')).toBe(true)
  })

  it('rechecks revoked access without substituting another grant for an identical model', async () => {
    cloud()
    let available = true
    fixture.request.mockImplementation(async (path: string) => path.startsWith('/model-options') ? { current: { selection: fixture.model, model: publicModel, selectable_reasoning: available ? publicModel.defaults.reasoning : null, source_name: 'Budget grant-a', available, unavailable_reason: available ? null : 'Grant revoked' }, options: [option('grant-b')], next_cursor: null } : path.startsWith('/providers') ? [] : inventory)
    const observed: ModelReadiness[] = []
    await act(async () => root.render(<ModelPicker onReadinessChange={value => observed.push(value)} />))
    available = false
    await act(async () => invalidateProviderInventory({ sessionId: 'session-1' }))
    await settle()
    expect(observed.at(-1)).toMatchObject({ canSubmit: false, error: '当前模型或授权已不可用，请重新选择。' })
    expect(host.textContent).toContain('Budget grant-a')
    expect(fixture.updateSession).not.toHaveBeenCalled()
  })

  it('paginates and explicitly selects the correct budget without changing a shared default', async () => {
    cloud()
    fixture.request.mockImplementation(async (path: string) => {
      if (!path.startsWith('/model-options')) return path.startsWith('/providers') ? [] : inventory
      const params = new URLSearchParams(path.split('?')[1])
      expect(params.getAll('limit')).toEqual(['25'])
      expect(params.get('session_id')).toBe('session-1')
      return { current: { selection: fixture.model, model: publicModel, selectable_reasoning: publicModel.defaults.reasoning, source_name: 'Budget grant-a', available: true }, options: [option(params.get('cursor') ? 'grant-b' : 'grant-a')], next_cursor: params.get('cursor') ? null : 'second-page' }
    })
    await act(async () => root.render(<ModelPicker rememberDefault={false} />))
    const dialog = await openPlatformDirectory()
    expect(dialog.textContent).toContain('Budget grant-a')
    const next = [...dialog.querySelectorAll<HTMLButtonElement>('button')].find(button => button.textContent?.includes('下一页'))!
    await act(async () => next.click())
    await settle()
    const pick = [...dialog.querySelectorAll<HTMLElement>('[data-model-grant="grant-b"] [role="menuitem"]')].find(button => button.textContent?.includes('Published model'))!
    await act(async () => pick.click())
    expect(fixture.updateSession).toHaveBeenCalledWith('session-1', { model: { provider: 'platform_model', grant_id: 'grant-b', model_id: 'same-model' } })
    expect(fixture.request.mock.calls.some(([path]) => String(path).startsWith('/default-model'))).toBe(false)
  })

  it('does not infer a runnable model from a cloud profile or automatically pick the first grant', async () => {
    cloud({ provider: 'profile_default' })
    fixture.request.mockImplementation(async (path: string) => path.startsWith('/model-options') ? { current: null, options: [option('grant-a')], next_cursor: null } : path.startsWith('/providers') ? [] : inventory)
    const observed: ModelReadiness[] = []
    await act(async () => root.render(<ModelPicker effectiveProfile={{ plugins: [{ id: 'model', kind: 'ternilo.model.openai_compatible', enabled: true, config: { base_url: 'https://unused.test/v1', model: 'unused' } }] }} onReadinessChange={value => observed.push(value)} />))
    expect(observed.at(-1)).toMatchObject({ status: 'empty', canSubmit: false })
    expect(host.textContent).toContain('选择模型')
    expect(fixture.updateSession).not.toHaveBeenCalled()
  })

  it.each([false, true])('saves an explicit Cloud default without creating or modifying a chat (explicit account=%s)', async explicitAccount => {
    cloud({ provider: 'profile_default' })
    fixture.hasSession = explicitAccount
    if (explicitAccount) fixture.placement = 'local_node'
    let saved: unknown = null
    fixture.request.mockImplementation(async (path: string, options?: { method?: string; body?: unknown }) => {
      if (path === '/default-model' && options?.method === 'PUT') { saved = options.body; return }
      if (path.startsWith('/model-options')) return { current: saved ? { selection: saved, model: publicModel, selectable_reasoning: publicModel.defaults.reasoning, source_name: 'Budget grant-a', available: true } : null, options: [option('grant-a')], next_cursor: null }
      return path.startsWith('/providers') ? [] : inventory
    })
    await act(async () => root.render(<ModelPicker defaultTenantId={explicitAccount ? 'personal' : undefined} />))
    const dialog = await openPlatformDirectory()
    const pick = [...dialog.querySelectorAll<HTMLElement>('[data-model-grant="grant-a"] [role="menuitem"]')].find(button => button.textContent?.includes('Published model'))!
    await act(async () => pick.click())
    await settle()
    expect(saved).toEqual({ provider: 'platform_model', grant_id: 'grant-a', model_id: 'same-model' })
    expect(fixture.updateSession).not.toHaveBeenCalled()
    expect(fixture.request).toHaveBeenCalledWith('/default-model', { headers: explicitAccount ? { 'x-ternilo-tenant': 'personal' } : undefined, method: 'PUT', body: saved })
    if (explicitAccount) {
      for (const [path, options] of fixture.request.mock.calls) {
        expect(path).not.toContain('session_id')
        expect(options?.headers).toEqual({ 'x-ternilo-tenant': 'personal' })
      }
    }
    expect(fixture.notify).toHaveBeenCalledWith('默认模型已保存，新会话将使用此选择')
  })

  it('keeps exact provider and budget tuples when changing or clearing reasoning', () => {
    const platform = { provider: 'platform_model' as const, grant_id: 'grant-b', model_id: 'same-model', reasoning_effort: 'low' as const }
    expect(withReasoningEffort(platform, 'high')).toEqual({ ...platform, reasoning_effort: 'high' })
    expect(withReasoningEffort(platform)).toEqual({ provider: 'platform_model', grant_id: 'grant-b', model_id: 'same-model' })
    const byok = { provider: 'named_provider' as const, provider_id: 'private', model: 'same-model' }
    expect(withReasoningEffort(byok, 'high')).toEqual({ ...byok, reasoning_effort: 'high' })
  })

  it('offers platform reasoning in the same menu and saves the exact authorization tuple', async () => {
    cloud()
    fixture.request.mockImplementation(async (path: string) => path.startsWith('/model-options')
      ? { current: { selection: fixture.model, model: publicModel, selectable_reasoning: publicModel.defaults.reasoning, source_name: 'Budget grant-a', available: true }, options: [option('grant-a')], next_cursor: null }
      : path.startsWith('/providers') ? [] : inventory)
    await act(async () => root.render(<ModelPicker rememberDefault={false} />))
    await settle()
    act(() => host.querySelector<HTMLButtonElement>('button')!.dispatchEvent(new MouseEvent('pointerdown', { bubbles: true, cancelable: true, button: 0 })))
    const effort = [...document.querySelectorAll<HTMLElement>('[role="menuitem"]')].find(item => item.textContent?.startsWith('推理强度'))!
    effort.focus()
    act(() => effort.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowRight', bubbles: true, cancelable: true })))
    await settle()
    await act(async () => [...document.querySelectorAll<HTMLElement>('[role="menuitem"]')].find(item => item.textContent === 'high')!.click())
    expect(fixture.updateSession).toHaveBeenCalledWith('session-1', { model: { provider: 'platform_model', grant_id: 'grant-a', model_id: 'same-model', reasoning_effort: 'high' } })
    expect(fixture.request.mock.calls.some(([path]) => String(path).startsWith('/default-model'))).toBe(false)
  })

  it('explains undeclared platform reasoning instead of inventing unsupported levels', async () => {
    cloud()
    fixture.request.mockResolvedValue({ current: { selection: fixture.model, model: { ...publicModel, defaults: { ...publicModel.defaults, reasoning: null } }, selectable_reasoning: null, source_name: 'Budget grant-a', available: true }, options: [], next_cursor: null })
    await act(async () => root.render(<ModelPicker rememberDefault={false} />))
    await settle()
    act(() => host.querySelector<HTMLButtonElement>('button')!.dispatchEvent(new MouseEvent('pointerdown', { bubbles: true, cancelable: true, button: 0 })))
    expect(document.querySelector('[data-reasoning-unavailable]')?.textContent).toContain('管理员')
    expect([...document.querySelectorAll<HTMLElement>('[role="menuitem"]')].some(item => item.textContent?.startsWith('推理强度'))).toBe(false)
  })

  it.each<ModelSelection>([
    { provider: 'platform_model', grant_id: 'grant-a', model_id: 'same-model' },
    { provider: 'account_provider', owner_user_id: 'owner', provider_id: 'private', model: 'same-model' },
  ])('offers newly declared reasoning without rewriting the saved $provider defaults', async selection => {
    cloud(selection)
    fixture.placement = 'local_node'
    fixture.request.mockImplementation(async (path: string) => path.startsWith('/model-options')
      ? { current: { selection, model: { ...publicModel, defaults: { ...publicModel.defaults, reasoning: null } }, selectable_reasoning: publicModel.defaults.reasoning, source_name: 'Original source', available: true }, options: [], next_cursor: null }
      : path.startsWith('/providers') ? [] : inventory)
    await act(async () => root.render(<ModelPicker rememberDefault={false} />))
    await settle()
    expect(host.textContent).not.toContain('medium')
    act(() => host.querySelector<HTMLButtonElement>('button')!.dispatchEvent(new MouseEvent('pointerdown', { bubbles: true, cancelable: true, button: 0 })))
    const effort = [...document.querySelectorAll<HTMLElement>('[role="menuitem"]')].find(item => item.textContent?.startsWith('推理强度'))!
    effort.focus()
    act(() => effort.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowRight', bubbles: true, cancelable: true })))
    await settle()
    const inherited = [...document.querySelectorAll<HTMLElement>('[role="menuitem"]')].find(item => item.textContent?.includes('medium') && !item.textContent?.startsWith('推理强度'))!
    expect(inherited.querySelector('svg')).toBeNull()
    expect(fixture.updateSession).not.toHaveBeenCalled()
    await act(async () => [...document.querySelectorAll<HTMLElement>('[role="menuitem"]')].find(item => item.textContent === 'high')!.click())
    expect(fixture.updateSession).toHaveBeenCalledWith('session-1', { model: { ...selection, reasoning_effort: 'high' } })
  })
})
