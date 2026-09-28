import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { api } from '@/api/client'
import { peekProviderInventory, resetProviderInventoryForTest } from '@/domain/provider-inventory'
import { LocaleProvider } from '@/i18n/provider'
import type {
  ApplicationCatalog,
  ExtensionInventory,
  LocalSession,
  ProviderProfile,
  SignedExtensionBundle,
} from '@/types'
import { PluginsSettings } from './plugins-settings'

vi.mock('@/api/client', () => ({ api: { request: vi.fn() } }))

const storageValues = new Map<string, string>()
Object.defineProperty(globalThis, 'localStorage', {
  configurable: true,
  value: {
    get length() {
      return storageValues.size
    },
    clear: () => storageValues.clear(),
    getItem: (key: string) => storageValues.get(key) ?? null,
    key: (index: number) => [...storageValues.keys()][index] ?? null,
    removeItem: (key: string) => {
      storageValues.delete(key)
    },
    setItem: (key: string, value: string) => {
      storageValues.set(key, value)
    },
  } satisfies Storage,
})

const workbench = vi.hoisted(() => ({
  currentSession: null as LocalSession | null,
  catalog: { revision: 'test', plugin_kinds: [], plugins: [] } as ApplicationCatalog,
  updateSession: vi.fn(),
  notify: vi.fn(),
}))
vi.mock('@/state/workbench', () => ({
  useWorkbench: () => ({
    currentSession: workbench.currentSession,
    catalog: workbench.catalog,
    updateSession: workbench.updateSession,
    notify: workbench.notify,
  }),
}))

const toolPresentation = {
  title: 'Example report',
  icon_kind: 'sparkles' as const,
  input_summary: [{ label: 'Subject', path: ['subject'] }],
  result: { kind: 'table' as const, columns: [] },
}

const rhaiBundle: SignedExtensionBundle = {
  manifest: {
    schema_version: 1,
    package_id: 'tools.example',
    version: '1.0.0',
    description: 'Configure how the example tools respond.',
    source: 'https://extensions.example.test/tools.example',
    publisher_key_id: 'publisher-a',
    payload_sha256: 'a'.repeat(64),
    runtime: {
      kind: 'rhai',
      limits: {
        max_operations: 1000,
        max_wall_ms: 1000,
        max_input_bytes: 16_384,
        max_output_bytes: 16_384,
        max_string_bytes: 16_384,
        max_collection_items: 1000,
        max_call_levels: 32,
        max_expr_depth: 32,
        max_variables: 256,
        max_functions: 64,
        max_workspace_read_bytes: 16_384,
      },
    },
    config_schema: {
      type: 'object',
      additionalProperties: false,
      properties: {
        style: { type: 'string', title: 'Response style', default: 'brief' },
        attempts: { type: 'integer', default: 3, minimum: 1 },
        optional_note: { type: 'string' },
        policy: {
          type: 'object',
          properties: { mode: { type: 'string', default: 'safe' } },
        },
      },
    },
    contributions: {
      tools: [
        {
          handler: 'lookup',
          spec: { name: 'example_lookup', description: 'Look up an example', input_schema: { type: 'object' } },
          output_schema: { type: 'object' },
          effect: 'read_only',
          presentation: toolPresentation,
        },
        {
          handler: 'save',
          spec: { name: 'example_save', description: 'Save an example', input_schema: { type: 'object' } },
          output_schema: { type: 'object' },
          effect: 'mutating',
        },
      ],
      prompt_sections: [
        {
          id: 'example-guidance',
          order: 700,
          content: 'Always cite the signed example source.\nKeep the response concise.',
        },
      ],
      skills: [
        {
          name: 'example-default-skill',
          description: 'Default invocation policy example.',
          content: 'Default signed Skill content.\nFollow every required step.',
        },
        {
          name: 'example-explicit-skill',
          description: 'Explicit invocation policy example.',
          when_to_use: 'Use for explicit policy acceptance.',
          invocation: { model_invocable: false, user_invocable: true },
          content: 'Explicit signed Skill content.',
        },
      ],
      hooks: [
        {
          id: 'guard-lookup',
          point: 'pre_tool_use',
          handler: 'guard_lookup',
          matcher: { kind: 'tool_names', names: ['example_lookup'] },
        },
      ],
      commands: [
        {
          name: 'lookup-example',
          description: 'Look up an example through the signed Tool.',
          tool: 'example_lookup',
          input: { hint: '<request>', field: 'request', images: false },
          fixed_arguments: {},
        },
      ],
      providers: [
        {
          id: 'example',
          display_name: 'Example Provider',
          base_url: 'https://api.example.test/v1',
          protocol: 'openai-responses',
          defaults: { context_window: 128_000, max_output_tokens: 16_384 },
          models: [{ id: 'example-model', settings: { mode: 'inherit' } }],
          timeout_ms: 120_000,
          max_attempts: 3,
          retry_base_delay_ms: 250,
          credential: { required: true, suggested_ref: 'EXAMPLE_API_KEY' },
        },
      ],
    },
    requested_capabilities: ['log', 'workspace_read'],
  },
  payload: { kind: 'utf8', content: 'fn lookup(input) { input }\nfn save(input) { input }' },
  signature_base64: 'signature',
}

const inventory: ExtensionInventory = {
  publishers: [
    {
      trust: { key_id: 'publisher-a', allowed_sources: ['internal'] },
      revoked: false,
      added_at_ms: 1,
      updated_at_ms: 1,
    },
  ],
  extensions: [
    {
      manifest: rhaiBundle.manifest,
      granted_capabilities: ['log', 'workspace_read'],
      enabled: true,
      revoked: false,
      installed_at_ms: 1,
      updated_at_ms: 1,
    },
  ],
}

let host: HTMLDivElement
let root: Root

beforeEach(() => {
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true
  host = document.createElement('div')
  document.body.append(host)
  root = createRoot(host)
  vi.mocked(api.request).mockImplementation(async (path) => (path === '/extensions' ? inventory : (undefined as never)))
})

afterEach(() => {
  act(() => root.unmount())
  host.remove()
  document.body.innerHTML = ''
  vi.clearAllMocks()
  workbench.currentSession = null
  workbench.catalog = { revision: 'test', plugin_kinds: [], plugins: [] }
  resetProviderInventoryForTest()
  localStorage.clear()
  delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
})

async function settle(action?: () => void) {
  await act(async () => {
    action?.()
    await Promise.resolve()
    await Promise.resolve()
    await Promise.resolve()
  })
}

function button(label: string, root: ParentNode = document) {
  const value = [...root.querySelectorAll<HTMLButtonElement>('button')].find(
    (item) => item.textContent?.trim() === label || item.getAttribute('aria-label') === label,
  )
  if (!value) throw new Error(`missing button ${label}`)
  return value
}

function changeValue(element: HTMLInputElement | HTMLTextAreaElement, value: string) {
  const prototype = element instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype
  Object.getOwnPropertyDescriptor(prototype, 'value')?.set?.call(element, value)
  element.dispatchEvent(new Event('input', { bubbles: true }))
}

async function chooseJson(input: HTMLInputElement, value: unknown) {
  Object.defineProperty(input, 'files', {
    configurable: true,
    value: [{ name: 'bundle.json', text: async () => JSON.stringify(value) }],
  })
  await settle(() => input.dispatchEvent(new Event('change', { bubbles: true })))
}

describe('extension package management', () => {
  it('shows generic runtime, digest, tools and capabilities with distinct lifecycle actions', async () => {
    await settle(() =>
      root.render(
        <LocaleProvider>
          <PluginsSettings onChanged={async () => undefined} />
        </LocaleProvider>,
      ),
    )
    expect(host.querySelector('[data-plugin-scope="session"]')?.textContent).toContain('当前会话覆盖')
    expect(api.request).toHaveBeenCalledWith('/extensions')
    await settle(() => button('扩展包', host).click())
    expect(host.querySelector('[data-plugin-scope="target"]')?.textContent).toContain('当前配置目标的扩展库')
    const packageRow = host.querySelector('[data-extension-package="tools.example@1.0.0"]')!
    expect(packageRow.querySelector('[data-extension-runtime]')?.textContent).toBe('rhai')
    expect(packageRow.textContent).toContain('a'.repeat(64))
    expect(packageRow.textContent).toContain('example_lookup')
    expect(packageRow.textContent).toContain('example_save')
    const promptSection = packageRow.querySelector('[data-extension-prompt-section="example-guidance"]')
    expect(promptSection?.textContent).toContain('example-guidance')
    expect(promptSection?.textContent).toContain('顺序 700')
    expect(promptSection?.textContent).toContain('Always cite the signed example source.\nKeep the response concise.')
    const defaultSkill = packageRow.querySelector('[data-extension-skill="example-default-skill"]')
    expect(defaultSkill?.textContent).toContain('Default invocation policy example.')
    expect(defaultSkill?.textContent).toContain('模型调用: 允许')
    expect(defaultSkill?.textContent).toContain('用户调用: 允许')
    expect(defaultSkill?.textContent).toContain('Default signed Skill content.\nFollow every required step.')
    const explicitSkill = packageRow.querySelector('[data-extension-skill="example-explicit-skill"]')
    expect(explicitSkill?.textContent).toContain('适用场景: Use for explicit policy acceptance.')
    expect(explicitSkill?.textContent).toContain('模型调用: 不允许')
    expect(explicitSkill?.textContent).toContain('用户调用: 允许')
    expect(explicitSkill?.textContent).toContain('Explicit signed Skill content.')
    expect(packageRow.querySelector('[data-extension-hook="guard-lookup"]')?.textContent).toContain(
      'pre_tool_use · guard_lookup',
    )
    expect(packageRow.querySelector('[data-extension-hook="guard-lookup"]')?.textContent).toContain(
      '匹配工具: example_lookup',
    )
    expect(packageRow.querySelector('[data-extension-command="lookup-example"]')?.textContent).toContain(
      '/lookup-example',
    )
    expect(packageRow.querySelector('[data-extension-command="lookup-example"]')?.textContent).toContain(
      'example_lookup',
    )
    expect(packageRow.querySelector('[data-extension-command="lookup-example"]')?.textContent).toContain(
      '<request> → request',
    )
    const provider = packageRow.querySelector('[data-extension-provider="example"]')
    expect(provider?.textContent).toContain('Example Provider')
    expect(provider?.textContent).toContain('openai-responses')
    expect(provider?.textContent).toContain('example-model')
    expect(provider?.textContent).toContain('EXAMPLE_API_KEY')
    expect(packageRow.textContent).toContain('log · workspace_read')
    expect(packageRow.querySelector('[data-extension-presentation]')?.textContent).toContain('Example report')
    const disabledSettings = packageRow.querySelector('[data-extension-settings-disabled]')
    expect(disabledSettings?.getAttribute('aria-disabled')).toBe('true')
    expect(disabledSettings?.textContent).toContain('请先选择会话')

    await settle(() => button('卸载扩展包 tools.example', host).click())
    expect(document.body.textContent).toContain('卸载这个扩展包？')
    await settle(() => button('卸载', document.querySelector('[data-settings-dialog]')!).click())
    expect(api.request).toHaveBeenCalledWith('/extensions/tools.example/1.0.0', { method: 'DELETE' })

    await settle(() => button('永久撤销扩展包 tools.example', host).click())
    expect(document.body.textContent).toContain('永久撤销这个扩展包版本？')
    await settle(() => button('撤销', document.querySelector('[data-settings-dialog]')!).click())
    expect(api.request).toHaveBeenCalledWith('/extensions/tools.example/1.0.0/revoke', { method: 'POST' })

    await settle(() => button('撤销发布者 publisher-a', host).click())
    expect(document.body.textContent).toContain('撤销这个发布者？')
    await settle(() => button('撤销', document.querySelector('[data-settings-dialog]')!).click())
    expect(api.request).toHaveBeenCalledWith('/extensions/publishers/publisher-a/revoke', { method: 'POST' })
  })

  it('mounts the generic package profile and keeps recovery unmount enabled after revocation', async () => {
    workbench.currentSession = {
      identity: { session_id: 'session-a', agent_id: 'agent' },
      profile_plugins: [],
    } as unknown as LocalSession
    vi.mocked(api.request).mockImplementation(async (path) => {
      if (path === '/extensions?session_id=session-a') return inventory
      if (path === '/sessions/session-a/plugins') return { plugins: workbench.currentSession?.profile_plugins ?? [] }
      return undefined as never
    })

    await settle(() =>
      root.render(
        <LocaleProvider>
          <PluginsSettings onChanged={async () => undefined} />
        </LocaleProvider>,
      ),
    )
    await settle(() => button('扩展包', host).click())
    const mount = host.querySelector<HTMLInputElement>('[data-extension-mount]')!
    await settle(() => mount.click())
    expect(workbench.updateSession).toHaveBeenCalledWith('session-a', {
      profile_plugins: [
        {
          id: 'extension:tools.example@1.0.0',
          kind: 'ternilo.extension.package',
          enabled: true,
          config: {
            package_id: 'tools.example',
            version: '1.0.0',
            settings: { style: 'brief', attempts: 3, policy: { mode: 'safe' } },
          },
        },
      ],
    })

    workbench.currentSession = {
      identity: { session_id: 'session-a', agent_id: 'agent' },
      profile_plugins: [
        {
          id: 'extension:tools.example@1.0.0',
          kind: 'ternilo.extension.package',
          enabled: true,
          config: { package_id: 'tools.example', version: '1.0.0', settings: { style: 'brief' } },
        },
      ],
    } as unknown as LocalSession
    vi.mocked(api.request).mockImplementation(async (path) => {
      if (path === '/extensions?session_id=session-a')
        return {
          ...inventory,
          extensions: [{ ...inventory.extensions[0], enabled: false, revoked: true }],
        }
      if (path === '/sessions/session-a/plugins') return { plugins: workbench.currentSession?.profile_plugins ?? [] }
      return undefined as never
    })
    await settle(() =>
      root.render(
        <LocaleProvider>
          <PluginsSettings onChanged={async () => undefined} />
        </LocaleProvider>,
      ),
    )
    const unmount = host.querySelector<HTMLInputElement>('[data-extension-mount]')
    expect(unmount?.getAttribute('aria-label')).toBe('从当前会话移除扩展包 tools.example')
    expect(unmount?.disabled).toBe(false)
    await settle(() => unmount?.click())
    expect(workbench.updateSession).toHaveBeenCalledWith('session-a', { profile_plugins: [] })
  })

  it('opens required settings and mounts once instead of submitting incomplete defaults', async () => {
    const requiredInventory: ExtensionInventory = {
      ...inventory,
      extensions: [
        {
          ...inventory.extensions[0],
          manifest: {
            ...inventory.extensions[0].manifest,
            config_schema: {
              type: 'object',
              additionalProperties: false,
              required: ['token'],
              properties: {
                token: { type: 'string', title: 'Required token' },
              },
            },
          },
        },
      ],
    }
    workbench.currentSession = {
      identity: { session_id: 'session-a', agent_id: 'agent' },
      preset_plugins: [],
      profile_plugins: [],
    } as unknown as LocalSession
    vi.mocked(api.request).mockImplementation(async (path) => {
      if (path === '/extensions?session_id=session-a') return requiredInventory
      if (path === '/sessions/session-a/plugins') return { plugins: [] }
      return undefined as never
    })

    await settle(() =>
      root.render(
        <LocaleProvider>
          <PluginsSettings onChanged={async () => undefined} />
        </LocaleProvider>,
      ),
    )
    await settle(() => button('扩展包', host).click())
    const packageRow = host.querySelector('[data-extension-package="tools.example@1.0.0"]')!
    const mount = packageRow.querySelector<HTMLElement>('[data-extension-mount]')!
    await settle(() => mount.click())
    expect(workbench.updateSession).not.toHaveBeenCalled()

    const settings = packageRow.querySelector('[data-extension-settings="tools.example@1.0.0"]')!
    expect(settings.querySelector('[aria-expanded="true"]')).not.toBeNull()
    const token = settings.querySelector<HTMLInputElement>('input[id$="-token"]')!
    await settle(() => changeValue(token, 'configured-token'))
    await settle(() => button('保存', settings).click())
    expect(workbench.updateSession).toHaveBeenCalledWith('session-a', {
      profile_plugins: [
        {
          id: 'extension:tools.example@1.0.0',
          kind: 'ternilo.extension.package',
          enabled: true,
          config: {
            package_id: 'tools.example',
            version: '1.0.0',
            settings: { token: 'configured-token' },
          },
        },
      ],
    })
  })

  it('edits only the mounted package settings and preserves its mount identity', async () => {
    const otherEntry = { id: 'other', kind: 'ternilo.other', enabled: true, config: { value: 1 } }
    const mountEntry = {
      id: 'extension:tools.example@1.0.0',
      kind: 'ternilo.extension.package',
      enabled: true,
      config: {
        package_id: 'tools.example',
        version: '1.0.0',
        settings: { style: 'brief', attempts: 3, policy: { mode: 'safe' } },
        future_field: 'preserved',
      },
    }
    workbench.currentSession = {
      identity: { session_id: 'session-a', agent_id: 'agent' },
      profile_plugins: [otherEntry, mountEntry],
    } as unknown as LocalSession
    vi.mocked(api.request).mockImplementation(async (path) => {
      if (path === '/extensions?session_id=session-a') return inventory
      if (path === '/sessions/session-a/plugins') return { plugins: workbench.currentSession?.profile_plugins ?? [] }
      return undefined as never
    })

    await settle(() =>
      root.render(
        <LocaleProvider>
          <PluginsSettings onChanged={async () => undefined} />
        </LocaleProvider>,
      ),
    )
    await settle(() => button('扩展包', host).click())
    const settings = host.querySelector('[data-extension-settings="tools.example@1.0.0"]')!
    expect(settings.textContent).toContain('Configure how the example tools respond.')
    await settle(() => button('展开: 会话设置：tools.example', settings).click())
    const style = settings.querySelector<HTMLInputElement>('input[id$="-style"]')!
    expect(style.value).toBe('brief')
    await settle(() => changeValue(style, 'detailed'))
    await settle(() => button('保存', settings).click())

    expect(workbench.updateSession).toHaveBeenCalledWith('session-a', {
      profile_plugins: [
        otherEntry,
        {
          ...mountEntry,
          config: {
            ...mountEntry.config,
            settings: { style: 'detailed', attempts: 3, policy: { mode: 'safe' } },
          },
        },
      ],
    })
    expect(workbench.notify).toHaveBeenCalledWith('扩展包会话设置已保存')
  })

  it('shows a preset-inherited mount and disables it with an in-place session override', async () => {
    const otherEntry = { id: 'other', kind: 'ternilo.other', enabled: true, config: { value: 1 } }
    const inheritedMount = {
      id: 'preset-example-extension',
      kind: 'ternilo.extension.package',
      enabled: true,
      config: {
        package_id: 'tools.example',
        version: '1.0.0',
        settings: { style: 'preset', attempts: 3, policy: { mode: 'safe' } },
        preset_marker: 'preserved',
      },
    }
    workbench.currentSession = {
      identity: { session_id: 'session-a', agent_id: 'agent' },
      preset_plugins: [inheritedMount],
      profile_plugins: [otherEntry],
    } as unknown as LocalSession
    vi.mocked(api.request).mockImplementation(async (path) => {
      if (path === '/extensions?session_id=session-a') return inventory
      if (path === '/sessions/session-a/plugins') return { plugins: [inheritedMount] }
      return undefined as never
    })

    await settle(() =>
      root.render(
        <LocaleProvider>
          <PluginsSettings onChanged={async () => undefined} />
        </LocaleProvider>,
      ),
    )
    await settle(() => button('扩展包', host).click())
    const packageRow = host.querySelector('[data-extension-package="tools.example@1.0.0"]')!
    const mount = packageRow.querySelector<HTMLElement>('[data-extension-mount]')!
    expect(mount.getAttribute('data-state')).toBe('checked')
    const settings = packageRow.querySelector('[data-extension-settings="tools.example@1.0.0"]')!
    await settle(() => button('展开: 会话设置：tools.example', settings).click())
    expect(settings.querySelector<HTMLInputElement>('input[id$="-style"]')?.value).toBe('preset')

    await settle(() => mount.click())
    expect(workbench.updateSession).toHaveBeenCalledWith('session-a', {
      profile_plugins: [otherEntry, { ...inheritedMount, enabled: false }],
    })
  })

  it('disables a base-inherited effective mount even when no preset row exists', async () => {
    const baseMount = {
      id: 'base-example-extension',
      kind: 'ternilo.extension.package',
      enabled: true,
      config: {
        package_id: 'tools.example',
        version: '1.0.0',
        settings: { style: 'base' },
        base_marker: 'preserved',
      },
    }
    workbench.currentSession = {
      identity: { session_id: 'session-a', agent_id: 'agent' },
      preset_plugins: [],
      profile_plugins: [],
    } as unknown as LocalSession
    vi.mocked(api.request).mockImplementation(async (path) => {
      if (path === '/extensions?session_id=session-a') return inventory
      if (path === '/sessions/session-a/plugins') return { plugins: [baseMount] }
      return undefined as never
    })

    await settle(() =>
      root.render(
        <LocaleProvider>
          <PluginsSettings onChanged={async () => undefined} />
        </LocaleProvider>,
      ),
    )
    await settle(() => button('扩展包', host).click())
    const mount = host.querySelector<HTMLElement>('[data-extension-mount]')!
    expect(mount.getAttribute('data-state')).toBe('checked')

    await settle(() => mount.click())
    expect(workbench.updateSession).toHaveBeenCalledWith('session-a', {
      profile_plugins: [{ ...baseMount, enabled: false }],
    })
  })

  it('re-enables a disabled preset mount without moving other session overrides', async () => {
    const otherEntry = { id: 'other', kind: 'ternilo.other', enabled: true, config: { value: 1 } }
    const presetMount = {
      id: 'preset-example-extension',
      kind: 'ternilo.extension.package',
      enabled: true,
      config: {
        package_id: 'tools.example',
        version: '1.0.0',
        settings: { style: 'preset' },
        preset_marker: 'preserved',
      },
    }
    const disabledOverride = {
      ...presetMount,
      enabled: false,
      config: { ...presetMount.config, settings: { style: 'session' } },
    }
    workbench.currentSession = {
      identity: { session_id: 'session-a', agent_id: 'agent' },
      preset_plugins: [presetMount],
      profile_plugins: [otherEntry, disabledOverride],
    } as unknown as LocalSession
    vi.mocked(api.request).mockImplementation(async (path) => {
      if (path === '/extensions?session_id=session-a') return inventory
      if (path === '/sessions/session-a/plugins') return { plugins: [disabledOverride] }
      return undefined as never
    })

    await settle(() =>
      root.render(
        <LocaleProvider>
          <PluginsSettings onChanged={async () => undefined} />
        </LocaleProvider>,
      ),
    )
    await settle(() => button('扩展包', host).click())
    const packageRow = host.querySelector('[data-extension-package="tools.example@1.0.0"]')!
    const mount = packageRow.querySelector<HTMLElement>('[data-extension-mount]')!
    expect(mount.getAttribute('data-state')).toBe('unchecked')
    expect(packageRow.querySelector('[data-extension-settings]')?.textContent).toContain(
      '填写并保存会话设置时，将同时把此扩展包加入当前会话。',
    )

    await settle(() => mount.click())
    expect(workbench.updateSession).toHaveBeenCalledWith('session-a', {
      profile_plugins: [otherEntry, { ...disabledOverride, enabled: true }],
    })
  })

  it('creates a session override when editing settings inherited from a preset', async () => {
    const otherEntry = { id: 'other', kind: 'ternilo.other', enabled: true, config: { value: 1 } }
    const inheritedMount = {
      id: 'preset-example-extension',
      kind: 'ternilo.extension.package',
      enabled: true,
      config: {
        package_id: 'tools.example',
        version: '1.0.0',
        settings: { style: 'preset', attempts: 3, policy: { mode: 'safe' } },
        preset_marker: 'preserved',
      },
    }
    workbench.currentSession = {
      identity: { session_id: 'session-a', agent_id: 'agent' },
      preset_plugins: [inheritedMount],
      profile_plugins: [otherEntry],
    } as unknown as LocalSession
    vi.mocked(api.request).mockImplementation(async (path) => {
      if (path === '/extensions?session_id=session-a') return inventory
      if (path === '/sessions/session-a/plugins') return { plugins: [inheritedMount] }
      return undefined as never
    })

    await settle(() =>
      root.render(
        <LocaleProvider>
          <PluginsSettings onChanged={async () => undefined} />
        </LocaleProvider>,
      ),
    )
    await settle(() => button('扩展包', host).click())
    const settings = host.querySelector('[data-extension-settings="tools.example@1.0.0"]')!
    await settle(() => button('展开: 会话设置：tools.example', settings).click())
    const style = settings.querySelector<HTMLInputElement>('input[id$="-style"]')!
    await settle(() => changeValue(style, 'session'))
    await settle(() => button('保存', settings).click())

    expect(workbench.updateSession).toHaveBeenCalledWith('session-a', {
      profile_plugins: [
        otherEntry,
        {
          ...inheritedMount,
          config: {
            ...inheritedMount.config,
            settings: { style: 'session', attempts: 3, policy: { mode: 'safe' } },
          },
        },
      ],
    })
  })

  it('does not offer uninstall for a permanently revoked package', async () => {
    vi.mocked(api.request).mockImplementation(async (path) => {
      if (path === '/extensions') {
        return {
          ...inventory,
          extensions: [{ ...inventory.extensions[0], enabled: false, revoked: true }],
        }
      }
      return undefined as never
    })
    await settle(() =>
      root.render(
        <LocaleProvider>
          <PluginsSettings onChanged={async () => undefined} />
        </LocaleProvider>,
      ),
    )
    await settle(() => button('扩展包', host).click())
    const packageRow = host.querySelector('[data-extension-package="tools.example@1.0.0"]')!
    expect(packageRow.textContent).toContain('已撤销')
    expect(packageRow.querySelector('[aria-label="卸载扩展包 tools.example"]')).toBeNull()
    expect([...packageRow.querySelectorAll('button')].some((item) => item.textContent?.includes('添加 Provider'))).toBe(
      false,
    )
  })

  it('materializes a signed Provider template with only the user-selectable identity and credential reference', async () => {
    const created: ProviderProfile = {
      id: 'materialized-example',
      source: 'user',
      display_name: 'Example Provider',
      base_url: 'https://api.example.test/v1',
      protocol: 'openai-responses',
      api_key_ref: 'EXAMPLE_API_KEY',
      defaults: { context_window: 128_000, max_output_tokens: 16_384 },
      models: [{ id: 'example-model', settings: { mode: 'inherit' } }],
      timeout_ms: 120_000,
      max_attempts: 3,
      retry_base_delay_ms: 250,
    }
    vi.mocked(api.request).mockImplementation(async (path) => {
      if (path === '/extensions') return inventory
      if (path === '/providers/from-extension') return created
      if (path === '/providers') return [created]
      if (path === '/credentials') return { references: [], records: [] }
      return undefined as never
    })
    await settle(() =>
      root.render(
        <LocaleProvider>
          <PluginsSettings onChanged={async () => undefined} />
        </LocaleProvider>,
      ),
    )
    await settle(() => button('扩展包', host).click())
    const packageRow = host.querySelector('[data-extension-package="tools.example@1.0.0"]')!
    await settle(() => button('添加 Provider', packageRow).click())

    const dialog = document.querySelector('[data-extension-provider-dialog]')!
    const id = dialog.querySelector<HTMLInputElement>('#extension-provider-id')!
    const credential = dialog.querySelector<HTMLInputElement>('#extension-provider-credential-ref')!
    expect(id.value).toBe('example')
    expect(credential.value).toBe('EXAMPLE_API_KEY')
    expect(dialog.textContent).toContain('端点、协议、模型与重试参数由清单固定')

    await settle(() => changeValue(id, 'materialized-example'))
    await settle(() => button('创建 Provider', dialog).click())
    expect(api.request).toHaveBeenCalledWith('/providers/from-extension', {
      method: 'POST',
      body: {
        package_id: 'tools.example',
        version: '1.0.0',
        template: 'example',
        provider_id: 'materialized-example',
        api_key_ref: 'EXAMPLE_API_KEY',
      },
    })
    expect(peekProviderInventory()).toEqual({ providers: [created], credentials: { references: [], records: [] } })
    expect(document.querySelector('[data-extension-provider-dialog]')).toBeNull()
    expect(workbench.notify).toHaveBeenCalledWith('Provider 已从扩展包模板创建')
  })

  it('reviews Rhai source and summarizes WASM binary without rendering executable UI', async () => {
    await settle(() =>
      root.render(
        <LocaleProvider>
          <PluginsSettings onChanged={async () => undefined} />
        </LocaleProvider>,
      ),
    )
    await settle(() => button('扩展包', host).click())
    const input = host.querySelector<HTMLInputElement>('#plugin-bundle-file')!
    await chooseJson(input, rhaiBundle)

    const review = document.querySelector('[data-extension-install-review]')!
    expect(review.textContent).toContain('rhai')
    expect(review.textContent).toContain('example_lookup')
    expect(review.textContent).toContain('example_save')
    const promptReview = review.querySelector('[data-extension-prompt-section-review="example-guidance"]')
    expect(promptReview?.textContent).toContain('example-guidance')
    expect(promptReview?.textContent).toContain('顺序 700')
    expect(promptReview?.textContent).toContain('Always cite the signed example source.\nKeep the response concise.')
    const defaultSkillReview = review.querySelector('[data-extension-skill-review="example-default-skill"]')
    expect(defaultSkillReview?.textContent).toContain('模型调用: 允许')
    expect(defaultSkillReview?.textContent).toContain('用户调用: 允许')
    expect(defaultSkillReview?.textContent).toContain('Default signed Skill content.\nFollow every required step.')
    const explicitSkillReview = review.querySelector('[data-extension-skill-review="example-explicit-skill"]')
    expect(explicitSkillReview?.textContent).toContain('适用场景: Use for explicit policy acceptance.')
    expect(explicitSkillReview?.textContent).toContain('模型调用: 不允许')
    expect(explicitSkillReview?.textContent).toContain('用户调用: 允许')
    expect(review.querySelector('[data-extension-hook-review="guard-lookup"]')?.textContent).toContain(
      'pre_tool_use · guard_lookup',
    )
    expect(review.querySelector('[data-extension-command-review="lookup-example"]')?.textContent).toContain(
      '/lookup-example',
    )
    expect(review.querySelector('[data-extension-command-review="lookup-example"]')?.textContent).toContain(
      'example_lookup',
    )
    expect(review.querySelector('[data-extension-provider-review="example"]')?.textContent).toContain(
      'Example Provider',
    )
    expect(review.querySelector('[data-extension-provider-review="example"]')?.textContent).toContain(
      'example-model',
    )
    expect(review.querySelector('[data-extension-source-review]')?.textContent).toContain('fn lookup(input)')
    expect(review.querySelector('[data-extension-presentation-review]')?.textContent).toContain('Example report')
    await settle(() => button('确认安装', review).click())
    expect(api.request).toHaveBeenCalledWith('/extensions', {
      method: 'POST',
      body: { bundle: rhaiBundle, granted_capabilities: ['log', 'workspace_read'] },
    })

    const binaryBundle: SignedExtensionBundle = {
      ...rhaiBundle,
      manifest: {
        ...rhaiBundle.manifest,
        package_id: 'tools.binary',
        payload_sha256: 'b'.repeat(64),
        runtime: {
          kind: 'wasm-component',
          world: 'ternilo:extension/runtime@1.0.0',
          limits: {
            fuel: 1000,
            max_memory_bytes: 65_536,
            max_input_bytes: 16_384,
            max_output_bytes: 16_384,
            max_workspace_read_bytes: 16_384,
          },
        },
      },
      payload: { kind: 'base64', content: 'AGFzbQEAAAA=' },
    }
    await chooseJson(input, binaryBundle)
    const binaryReview = document.querySelector('[data-extension-install-review]')!
    expect(binaryReview.textContent).toContain('wasm-component')
    expect(binaryReview.querySelector('[data-extension-binary-summary]')?.textContent).toContain('字节')
    expect(binaryReview.querySelector('[data-extension-source-review]')).toBeNull()
  })

  it('accepts a skill-only schema v1 bundle and does not render empty tool or prompt blocks', async () => {
    await settle(() =>
      root.render(
        <LocaleProvider>
          <PluginsSettings onChanged={async () => undefined} />
        </LocaleProvider>,
      ),
    )
    await settle(() => button('扩展包', host).click())
    const input = host.querySelector<HTMLInputElement>('#plugin-bundle-file')!
    const skillOnlyBundle: SignedExtensionBundle = {
      ...rhaiBundle,
      manifest: {
        ...rhaiBundle.manifest,
        package_id: 'skills.example',
        contributions: {
          tools: [],
          prompt_sections: [],
          skills: [
            {
              name: 'skill-only',
              description: 'A standalone signed Skill.',
              when_to_use: 'Use when only a Skill is contributed.',
              invocation: { model_invocable: true, user_invocable: false },
              content: 'Skill-only signed content.',
            },
          ],
          hooks: [],
          commands: [],
          providers: [],
        },
      },
    }
    await chooseJson(input, skillOnlyBundle)

    const review = document.querySelector('[data-extension-install-review]')!
    expect(review).not.toBeNull()
    expect(review.querySelector('[data-extension-tool]')).toBeNull()
    expect(review.textContent).not.toContain('工具贡献')
    expect(review.querySelector('[data-extension-prompt-sections-review]')).toBeNull()
    const skill = review.querySelector('[data-extension-skill-review="skill-only"]')
    expect(skill?.textContent).toContain('A standalone signed Skill.')
    expect(skill?.textContent).toContain('模型调用: 允许')
    expect(skill?.textContent).toContain('用户调用: 不允许')
    expect(skill?.textContent).toContain('Skill-only signed content.')
  })

  it('rejects non-v1 schemas, missing arrays, malformed contribution wires, and empty contributions', async () => {
    await settle(() =>
      root.render(
        <LocaleProvider>
          <PluginsSettings onChanged={async () => undefined} />
        </LocaleProvider>,
      ),
    )
    await settle(() => button('扩展包', host).click())
    const input = host.querySelector<HTMLInputElement>('#plugin-bundle-file')!

    await chooseJson(input, {
      ...rhaiBundle,
      manifest: { ...rhaiBundle.manifest, schema_version: 3 },
    })
    expect(host.querySelector('[role="alert"]')?.textContent).toContain('不是有效的签名扩展包 Bundle')
    expect(document.querySelector('[data-extension-install-review]')).toBeNull()

    for (const missing of ['tools', 'prompt_sections', 'skills', 'hooks', 'commands', 'providers'] as const) {
      const contributions: Record<string, unknown> = { ...rhaiBundle.manifest.contributions }
      delete contributions[missing]
      await chooseJson(input, {
        ...rhaiBundle,
        manifest: { ...rhaiBundle.manifest, contributions },
      })
      expect(host.querySelector('[role="alert"]')?.textContent).toContain('不是有效的签名扩展包 Bundle')
      expect(document.querySelector('[data-extension-install-review]')).toBeNull()
    }

    await chooseJson(input, {
      ...rhaiBundle,
      manifest: {
        ...rhaiBundle.manifest,
        contributions: {
          tools: [],
          prompt_sections: [],
          skills: [{
            name: 'malformed-policy',
            description: 'Missing one required invocation flag.',
            invocation: { model_invocable: true },
            content: 'This must not be accepted for review.',
          }],
          hooks: [],
          commands: [],
          providers: [],
        },
      },
    })
    expect(host.querySelector('[role="alert"]')?.textContent).toContain('不是有效的签名扩展包 Bundle')
    expect(document.querySelector('[data-extension-install-review]')).toBeNull()

    await chooseJson(input, {
      ...rhaiBundle,
      manifest: {
        ...rhaiBundle.manifest,
        contributions: {
          ...rhaiBundle.manifest.contributions,
          hooks: [{
            id: 'bad-hook',
            point: 'session_start',
            handler: 'bad_hook',
            matcher: { kind: 'tool_names', names: ['example_lookup'] },
          }],
        },
      },
    })
    expect(host.querySelector('[role="alert"]')?.textContent).toContain('不是有效的签名扩展包 Bundle')
    expect(document.querySelector('[data-extension-install-review]')).toBeNull()

    await chooseJson(input, {
      ...rhaiBundle,
      manifest: {
        ...rhaiBundle.manifest,
        contributions: {
          ...rhaiBundle.manifest.contributions,
          commands: [{
            name: 'outside',
            description: 'References a Tool outside this package.',
            tool: 'outside_package',
            fixed_arguments: {},
          }],
        },
      },
    })
    expect(host.querySelector('[role="alert"]')?.textContent).toContain('不是有效的签名扩展包 Bundle')
    expect(document.querySelector('[data-extension-install-review]')).toBeNull()

    await chooseJson(input, {
      ...rhaiBundle,
      manifest: {
        ...rhaiBundle.manifest,
        contributions: {
          ...rhaiBundle.manifest.contributions,
          providers: [{
            ...rhaiBundle.manifest.contributions.providers[0],
            base_url: 'file:///tmp/model',
          }],
        },
      },
    })
    expect(host.querySelector('[role="alert"]')?.textContent).toContain('不是有效的签名扩展包 Bundle')
    expect(document.querySelector('[data-extension-install-review]')).toBeNull()

    await chooseJson(input, {
      ...rhaiBundle,
      manifest: {
        ...rhaiBundle.manifest,
        contributions: { tools: [], prompt_sections: [], skills: [], hooks: [], commands: [], providers: [] },
      },
    })
    expect(host.querySelector('[role="alert"]')?.textContent).toContain('不是有效的签名扩展包 Bundle')
    expect(document.querySelector('[data-extension-install-review]')).toBeNull()
  })

  it('renders English Skill policy labels and preserves a long signed body in installed and review cards', async () => {
    localStorage.setItem('ternilo.locale', 'en')
    const longContent = `LONG_SIGNED_SKILL_${'x'.repeat(8_192)}\nFinal required step.`
    const longBundle: SignedExtensionBundle = {
      ...rhaiBundle,
      manifest: {
        ...rhaiBundle.manifest,
        contributions: {
          ...rhaiBundle.manifest.contributions,
          skills: [
            {
              name: 'long-default-skill',
              description: 'Default policy with a long signed body.',
              content: longContent,
            },
            {
              name: 'explicit-policy-skill',
              description: 'Explicit policy values.',
              when_to_use: 'Use for English policy review.',
              invocation: { model_invocable: false, user_invocable: true },
              content: 'Explicit policy body.',
            },
          ],
        },
      },
    }
    const longInventory: ExtensionInventory = {
      ...inventory,
      extensions: [{ ...inventory.extensions[0], manifest: longBundle.manifest }],
    }
    vi.mocked(api.request).mockImplementation(async (path) => (
      path === '/extensions' ? longInventory : (undefined as never)
    ))

    await settle(() =>
      root.render(
        <LocaleProvider>
          <PluginsSettings onChanged={async () => undefined} />
        </LocaleProvider>,
      ),
    )
    await settle(() => button('Extension packages', host).click())
    const packageRow = host.querySelector('[data-extension-package="tools.example@1.0.0"]')!
    const installedDefault = packageRow.querySelector('[data-extension-skill="long-default-skill"]')!
    expect(installedDefault.textContent).toContain('Model invocation: Allowed')
    expect(installedDefault.textContent).toContain('User invocation: Allowed')
    const installedBody = installedDefault.querySelector('[data-extension-skill-content]')!
    expect(installedBody.textContent).toBe(longContent)
    expect(installedBody.className).toContain('max-w-full')
    expect(installedBody.className).toContain('overflow-auto')
    expect(installedBody.className).toContain('break-all')
    const installedExplicit = packageRow.querySelector('[data-extension-skill="explicit-policy-skill"]')!
    expect(installedExplicit.textContent).toContain('When to use: Use for English policy review.')
    expect(installedExplicit.textContent).toContain('Model invocation: Not allowed')
    expect(installedExplicit.textContent).toContain('User invocation: Allowed')

    const input = host.querySelector<HTMLInputElement>('#plugin-bundle-file')!
    await chooseJson(input, longBundle)
    const review = document.querySelector('[data-extension-install-review]')!
    const reviewDefault = review.querySelector('[data-extension-skill-review="long-default-skill"]')!
    expect(reviewDefault.textContent).toContain('Model invocation: Allowed')
    expect(reviewDefault.textContent).toContain('User invocation: Allowed')
    const reviewBody = reviewDefault.querySelector('[data-extension-skill-content]')!
    expect(reviewBody.textContent).toBe(longContent)
    expect(reviewBody.className).toContain('max-w-full')
    expect(reviewBody.className).toContain('overflow-auto')
    expect(reviewBody.className).toContain('break-all')
    const reviewExplicit = review.querySelector('[data-extension-skill-review="explicit-policy-skill"]')!
    expect(reviewExplicit.textContent).toContain('Model invocation: Not allowed')
    expect(reviewExplicit.textContent).toContain('User invocation: Allowed')
  })
})

describe('session plugin configuration', () => {
  it('saves max_tool_calls=0 as a session override and can remove that override', async () => {
    const agentEntry = {
      id: 'agent-loop',
      kind: 'ternilo.agent.react',
      enabled: true,
      config: { max_tool_calls: 8 },
    }
    workbench.currentSession = {
      identity: { session_id: 'session-a', agent_id: 'agent' },
      profile_plugins: [agentEntry],
    } as unknown as LocalSession
    workbench.catalog = {
      revision: 'test',
      plugin_kinds: ['ternilo.agent.react'],
      plugins: [
        {
          kind: 'ternilo.agent.react',
          description: 'Agent loop',
          requires: [],
          provides: [],
          config_schema: {
            type: 'object',
            properties: {
              max_tool_calls: {
                type: 'integer',
                title: 'max_tool_calls',
                description: 'Maximum tool calls; 0 means no plugin limit.',
                minimum: 0,
                default: 512,
              },
            },
          },
        },
      ],
    }
    vi.mocked(api.request).mockImplementation(async (path) => {
      if (path === '/extensions?session_id=session-a') return inventory
      if (path === '/sessions/session-a/plugins') return { plugins: [agentEntry] }
      return undefined as never
    })

    await settle(() =>
      root.render(
        <LocaleProvider>
          <PluginsSettings onChanged={async () => undefined} />
        </LocaleProvider>,
      ),
    )
    await settle(() => button('展开: agent-loop', host).click())
    const maxSteps = host.querySelector<HTMLInputElement>('#plugin-agent-loop-max_tool_calls')!
    expect(maxSteps.value).toBe('8')
    await settle(() => changeValue(maxSteps, '0'))
    await settle(() => button('保存', host).click())

    expect(workbench.updateSession).toHaveBeenNthCalledWith(1, 'session-a', {
      profile_plugins: [{ ...agentEntry, config: { max_tool_calls: 0 } }],
    })

    await settle(() => button('展开: agent-loop', host).click())
    await settle(() => button('移除会话覆盖', host).click())
    expect(workbench.updateSession).toHaveBeenNthCalledWith(2, 'session-a', { profile_plugins: [] })
  })
})

describe('built-in plugin localization', () => {
  it('uses localized descriptions in configuration, catalog, and catalog search', async () => {
    localStorage.setItem('ternilo.locale', 'en')
    workbench.currentSession = {
      identity: { session_id: 'session-a', agent_id: 'agent' },
      profile_plugins: [],
    } as unknown as LocalSession
    workbench.catalog = {
      revision: 'test',
      plugin_kinds: ['ternilo.web.search.searxng', 'acme.search'],
      plugins: [
        {
          kind: 'ternilo.web.search.searxng',
          description: '通过配置的 SearXNG endpoint 搜索网页。',
          requires: [],
          provides: [],
          config_schema: {},
        },
        {
          kind: 'acme.search',
          description: '作者原文',
          requires: [],
          provides: [],
          config_schema: {},
        },
      ],
    }
    vi.mocked(api.request).mockImplementation(async (path) => {
      if (path === '/extensions?session_id=session-a') return inventory
      if (path === '/sessions/session-a/plugins')
        return {
          plugins: [{ id: 'web-search', kind: 'ternilo.web.search.searxng', enabled: true, config: {} }],
        }
      return undefined as never
    })

    await settle(() =>
      root.render(
        <LocaleProvider>
          <PluginsSettings onChanged={async () => undefined} />
        </LocaleProvider>,
      ),
    )
    expect(host.querySelector('[data-plugin-scope="session"]')?.textContent).toContain(
      'affect only the current session',
    )
    expect(host.textContent).toContain('Searches the web through the configured SearXNG endpoint.')
    expect(host.textContent).not.toContain('通过配置的 SearXNG endpoint 搜索网页。')

    await settle(() => button('Plugin list', host).click())
    expect(host.textContent).toContain('作者原文')
    const search = host.querySelector<HTMLInputElement>('input[type="search"]')!
    await settle(() => {
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')?.set?.call(search, 'Searches the web')
      search.dispatchEvent(new Event('input', { bubbles: true }))
    })
    expect(host.querySelector('[data-plugin-count]')?.textContent).toBe('1')
    expect(host.querySelector('[data-plugin-kind="ternilo.web.search.searxng"]')).not.toBeNull()
    expect(host.querySelector('[data-plugin-kind="acme.search"]')).toBeNull()
  })
})
