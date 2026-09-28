import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { api } from '@/api/client'
import { LocaleProvider, useLocale } from '@/i18n/provider'
import type { AgentPresetDocument, AgentPresetRoster, ApplicationCatalog, ExtensionInventory } from '@/types'
import { PresetsSettings } from './presets-settings'

vi.mock('@/api/client', () => ({ api: { request: vi.fn() } }))

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

const systemDocument: AgentPresetDocument = {
  id: 'standard',
  display_name: 'Standard',
  description: 'Ternilo 标准软件 Agent：工作区文件、受限 shell、Skills、计划与代码模式。',
  trust: 'system',
  profile: { plugins: [] },
}

const roster: AgentPresetRoster = {
  default_id: 'standard',
  authorable: true,
  presets: [
    { ...systemDocument },
    { id: 'mine', display_name: '我的 Agent', description: '作者原文', trust: 'user' },
  ],
}

const catalog: ApplicationCatalog = {
  revision: 'test',
  plugin_kinds: ['ternilo.agent.react'],
  plugins: [{
    kind: 'ternilo.agent.react',
    description: 'Agent loop',
    requires: [],
    provides: [],
    config_schema: {
      type: 'object',
      properties: {
        max_steps: {
          type: 'integer',
          title: 'max_steps',
          description: 'Maximum Agent steps; 0 means no plugin limit.',
          minimum: 0,
          default: 0,
        },
        max_tool_calls: { type: 'integer', title: 'max_tool_calls', minimum: 0, default: 512 },
      },
    },
  }],
}

const workbench = vi.hoisted(() => ({
  presets: null as AgentPresetRoster | null,
  catalog: null as ApplicationCatalog | null,
  refresh: vi.fn(), updateSession: vi.fn(), notify: vi.fn(),
}))

vi.mock('@/state/workbench', () => ({
  useWorkbench: () => ({
    presets: workbench.presets,
    catalog: workbench.catalog,
    currentSession: null,
    currentWorkspace: null,
    refresh: workbench.refresh,
    updateSession: workbench.updateSession,
    notify: workbench.notify,
  }),
}))

let host: HTMLDivElement
let root: Root

beforeEach(() => {
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true
  localStorage.setItem('ternilo.locale', 'en')
  workbench.presets = roster
  workbench.catalog = catalog
  vi.mocked(api.request).mockResolvedValue(systemDocument)
  host = document.createElement('div')
  document.body.append(host)
  root = createRoot(host)
})

afterEach(() => {
  act(() => root.unmount())
  host.remove()
  document.body.innerHTML = ''
  localStorage.clear()
  vi.clearAllMocks()
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

function changeInput(element: HTMLInputElement, value: string) {
  Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')?.set?.call(element, value)
  element.dispatchEvent(new Event('input', { bubbles: true }))
}

function LocaleSwitchingPresets() {
  const { locale, setLocale } = useLocale()
  return (
    <>
      <button type="button" aria-label="switch-test-locale" onClick={() => setLocale(locale === 'zh' ? 'en' : 'zh')} />
      <PresetsSettings />
    </>
  )
}

describe('system preset localization', () => {
  it('localizes the card and view while retaining user-authored metadata', async () => {
    await settle(() => root.render(<LocaleProvider><PresetsSettings /></LocaleProvider>))
    expect(host.textContent).toContain('A full software Agent with both native tools and the Rhai Code Mode SDK.')
    expect(host.textContent).toContain('我的 Agent')
    expect(host.textContent).toContain('作者原文')
    expect(host.textContent).not.toContain('Ternilo 标准软件 Agent')

    const view = [...host.querySelectorAll<HTMLButtonElement>('button')]
      .find(item => item.getAttribute('aria-label') === 'View: Standard Mode')
    expect(view).toBeDefined()
    await settle(() => view?.click())
    expect(document.body.textContent).toContain('View Standard Mode')
    expect(document.body.textContent).toContain('A full software Agent with both native tools and the Rhai Code Mode SDK.')
    expect(document.body.textContent).not.toContain('Ternilo 标准软件 Agent')
  })

  it('freezes the Chinese built-in name and description when creating a custom preset', async () => {
    localStorage.setItem('ternilo.locale', 'zh')
    const created: AgentPresetDocument = {
      ...systemDocument,
      id: 'standard-custom',
      display_name: '标准模式 · 自定义',
      trust: 'user',
    }
    vi.mocked(api.request).mockResolvedValue(created)
    await settle(() => root.render(<LocaleProvider><LocaleSwitchingPresets /></LocaleProvider>))

    await settle(() => buttonWithLabel('复制预设: 标准模式').click())
    expect(document.body.textContent).toContain('复制预设 · 标准模式')
    await settle(() => buttonWithText('创建预设').click())

    expect(api.request).toHaveBeenNthCalledWith(1, '/agent-presets', {
      method: 'POST',
      body: { from: 'standard', id: 'standard-custom', display_name: '标准模式 · 自定义' },
    })
    expect(api.request).toHaveBeenNthCalledWith(2, '/agent-presets/standard-custom', {
      method: 'PUT',
      body: {
        display_name: '标准模式 · 自定义',
        description: '完整的软件 Agent，同时提供原生工具与 Rhai Code Mode SDK。',
        profile: systemDocument.profile,
      },
    })

    workbench.presets = {
      ...roster,
      presets: [
        ...roster.presets,
        {
          id: 'standard-custom',
          display_name: '标准模式 · 自定义',
          description: '完整的软件 Agent，同时提供原生工具与 Rhai Code Mode SDK。',
          trust: 'user',
        },
      ],
    }
    await settle(() => buttonWithLabel('switch-test-locale').click())
    expect(host.textContent).toContain('Standard Mode')
    expect(host.textContent).toContain('标准模式 · 自定义')
    expect(host.textContent).toContain('完整的软件 Agent，同时提供原生工具与 Rhai Code Mode SDK。')
  })

  it('freezes the English built-in name and description when creating a custom preset', async () => {
    const created: AgentPresetDocument = {
      ...systemDocument,
      id: 'standard-custom',
      display_name: 'Standard Mode · Custom',
      trust: 'user',
    }
    vi.mocked(api.request).mockResolvedValue(created)
    await settle(() => root.render(<LocaleProvider><PresetsSettings /></LocaleProvider>))

    await settle(() => buttonWithLabel('Copy preset: Standard Mode').click())
    await settle(() => buttonWithText('Create preset').click())

    expect(api.request).toHaveBeenNthCalledWith(1, '/agent-presets', {
      method: 'POST',
      body: { from: 'standard', id: 'standard-custom', display_name: 'Standard Mode · Custom' },
    })
    expect(api.request).toHaveBeenNthCalledWith(2, '/agent-presets/standard-custom', {
      method: 'PUT',
      body: {
        display_name: 'Standard Mode · Custom',
        description: 'A full software Agent with both native tools and the Rhai Code Mode SDK.',
        profile: systemDocument.profile,
      },
    })
  })

  it('edits max_tool_calls=0 through the schema form and keeps raw Profile JSON advanced', async () => {
    const userDocument: AgentPresetDocument = {
      id: 'mine',
      display_name: '我的 Agent',
      description: '作者原文',
      trust: 'user',
      profile: {
        plugins: [{
          id: 'agent-loop',
          kind: 'ternilo.agent.react',
          enabled: true,
          config: { max_tool_calls: 8 },
        }],
      },
    }
    vi.mocked(api.request).mockImplementation(async (path) => {
      if (path === '/agent-presets/mine') return userDocument
      return undefined as never
    })
    await settle(() => root.render(<LocaleProvider><PresetsSettings /></LocaleProvider>))

    const edit = [...host.querySelectorAll<HTMLButtonElement>('button')]
      .find(item => item.getAttribute('aria-label') === 'Edit: 我的 Agent')
    expect(edit).toBeDefined()
    await settle(() => edit?.click())
    const advanced = document.querySelector<HTMLDetailsElement>('details')!
    expect(advanced.open).toBe(false)
    expect(advanced.textContent).toContain('Plugin Profile (advanced)')

    await settle(() => buttonWithLabel('Expand: agent-loop').click())
    const maxSteps = document.querySelector<HTMLInputElement>('#plugin-agent-loop-max_tool_calls')!
    expect(maxSteps.value).toBe('8')
    await settle(() => changeInput(maxSteps, '0'))
    await settle(() => buttonWithText('Apply to draft').click())

    const nextProfile = {
      plugins: [{
        id: 'agent-loop',
        kind: 'ternilo.agent.react',
        enabled: true,
        config: { max_tool_calls: 0 },
      }],
    }
    expect(document.querySelector<HTMLTextAreaElement>('#preset-edit-profile')?.value).toBe(JSON.stringify(nextProfile, null, 2))
    await settle(() => buttonWithText('Save preset').click())

    expect(api.request).toHaveBeenCalledWith('/agent-presets/mine', {
      method: 'PUT',
      body: {
        display_name: '我的 Agent',
        description: '作者原文',
        profile: nextProfile,
      },
    })
  })

  it('shows inherited plugins and saves only the edited entry alongside the existing overlay', async () => {
    const base = { plugins: [
      { id: 'inherited-runner', kind: 'ternilo.agent.react', enabled: true, config: { max_steps: 12, max_tool_calls: 512 } },
      { id: 'inherited-files', kind: 'ternilo.files.local', enabled: true, config: { max_read_bytes: 1024 } },
      { id: 'code-mode', kind: 'ternilo.tools.code_mode', enabled: true, config: { mode: 'code' } },
    ] }
    const overlay = { plugins: [{ id: 'code-mode', kind: 'ternilo.tools.code_mode', enabled: true, config: { mode: 'both' } }] }
    const userDocument: AgentPresetDocument = {
      id: 'mine', display_name: '我的 Agent', description: '作者原文', trust: 'user',
      base_profile: base, profile: overlay,
    }
    vi.mocked(api.request).mockResolvedValue(userDocument)
    await settle(() => root.render(<LocaleProvider><PresetsSettings /></LocaleProvider>))
    await settle(() => buttonWithLabel('Edit: 我的 Agent').click())
    expect(document.querySelectorAll('[data-preset-plugin-editor] [data-plugin-id]')).toHaveLength(3)
    expect(JSON.parse(document.querySelector<HTMLTextAreaElement>('#preset-edit-profile')!.value)).toEqual(overlay)
    await settle(() => buttonWithLabel('Expand: inherited-runner').click())
    const input = document.querySelector<HTMLInputElement>('#plugin-inherited-runner-max_tool_calls')!
    expect(input.value).toBe('512')
    await settle(() => changeInput(input, '0'))
    await settle(() => buttonWithText('Apply to draft').click())
    const expectedProfile = { plugins: [overlay.plugins[0], { ...base.plugins[0], config: { max_steps: 12, max_tool_calls: 0 } }] }
    expect(JSON.parse(document.querySelector<HTMLTextAreaElement>('#preset-edit-profile')!.value)).toEqual(expectedProfile)
    await settle(() => buttonWithText('Save preset').click())
    expect(api.request).toHaveBeenCalledWith('/agent-presets/mine', { method: 'PUT', body: {
      display_name: '我的 Agent', description: '作者原文', profile: expectedProfile,
    } })
    expect(base.plugins[0].config.max_tool_calls).toBe(512)
  })

  it('replaces each inherited entry as a whole and restores inheritance when raw JSON removes its override', async () => {
    const base = { plugins: [{ id: 'runner', kind: 'ternilo.agent.react', enabled: true, config: { max_steps: 8, max_tool_calls: 100 } }] }
    const userDocument: AgentPresetDocument = {
      id: 'mine', display_name: '我的 Agent', description: '作者原文', trust: 'user', base_profile: base,
      profile: { plugins: [{ id: 'runner', kind: 'ternilo.agent.react', enabled: true, config: { max_tool_calls: 2 } }] },
    }
    vi.mocked(api.request).mockResolvedValue(userDocument)
    await settle(() => root.render(<LocaleProvider><PresetsSettings /></LocaleProvider>))
    await settle(() => buttonWithLabel('Edit: 我的 Agent').click())
    await settle(() => buttonWithLabel('Expand: runner').click())
    expect(document.querySelector<HTMLInputElement>('#plugin-runner-max_tool_calls')!.value).toBe('2')
    expect(document.querySelector<HTMLInputElement>('#plugin-runner-max_steps')!.value).toBe('0')
    const raw = document.querySelector<HTMLTextAreaElement>('#preset-edit-profile')!
    await settle(() => {
      Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value')!.set!.call(raw, '{"plugins":[]}')
      raw.dispatchEvent(new Event('input', { bubbles: true }))
    })
    expect(document.querySelector<HTMLInputElement>('#plugin-runner-max_tool_calls')!.value).toBe('100')
    expect(document.querySelector<HTMLInputElement>('#plugin-runner-max_steps')!.value).toBe('8')
    await settle(() => buttonWithText('Save preset').click())
    expect(api.request).toHaveBeenCalledWith('/agent-presets/mine', { method: 'PUT', body: {
      display_name: '我的 Agent', description: '作者原文', profile: { plugins: [] },
    } })
  })

  it('saves disabling an inherited plugin without copying unrelated base entries', async () => {
    const entry = { id: 'inherited-tool', kind: 'ternilo.tools.jobs', enabled: true, config: {} }
    const userDocument: AgentPresetDocument = {
      id: 'mine', display_name: '我的 Agent', description: '作者原文', trust: 'user',
      base_profile: { plugins: [entry, { id: 'unrelated', kind: 'ternilo.files.local', enabled: true, config: {} }] },
      profile: { plugins: [] },
    }
    vi.mocked(api.request).mockResolvedValue(userDocument)
    await settle(() => root.render(<LocaleProvider><PresetsSettings /></LocaleProvider>))
    await settle(() => buttonWithLabel('Edit: 我的 Agent').click())
    await settle(() => buttonWithLabel('Disable inherited-tool').click())
    await settle(() => buttonWithText('Save preset').click())
    expect(api.request).toHaveBeenCalledWith('/agent-presets/mine', { method: 'PUT', body: {
      display_name: '我的 Agent', description: '作者原文', profile: { plugins: [{ ...entry, enabled: false }] },
    } })
    expect(entry.enabled).toBe(true)
  })

  it.each([false, true])('edits required Extension settings through the signed manifest schema (inherited: %s)', async inherited => {
    const userDocument: AgentPresetDocument = {
      id: 'mine',
      display_name: '我的 Agent',
      description: '作者原文',
      trust: 'user',
      profile: {
        plugins: [{
          id: 'extension-tools-required-1-0-0',
          kind: 'ternilo.extension.package',
          enabled: true,
          config: {
            package_id: 'tools.required',
            version: '1.0.0',
            settings: {},
          },
        }],
      },
    }
    if (inherited) {
      userDocument.base_profile = userDocument.profile
      userDocument.profile = { plugins: [] }
    }
    const inventory: ExtensionInventory = {
      publishers: [],
      extensions: [{
        manifest: {
          schema_version: 1,
          package_id: 'tools.required',
          version: '1.0.0',
          description: 'Required settings fixture',
          source: 'internal',
          publisher_key_id: 'publisher-a',
          payload_sha256: 'a'.repeat(64),
          runtime: {
            kind: 'rhai',
            limits: {
              max_operations: 1_000,
              max_wall_ms: 1_000,
              max_input_bytes: 16_384,
              max_output_bytes: 16_384,
              max_string_bytes: 16_384,
              max_collection_items: 1_000,
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
            required: ['endpoint'],
            properties: {
              endpoint: { type: 'string', title: 'Endpoint' },
            },
          },
          contributions: {
            tools: [{
              handler: 'required_settings',
              spec: {
                name: 'required_settings',
                description: 'Required settings fixture.',
                input_schema: { type: 'object' },
              },
              output_schema: { type: 'object' },
              effect: 'read_only',
            }],
            prompt_sections: [],
            skills: [],
            hooks: [],
            commands: [],
            providers: [],
          },
          requested_capabilities: [],
        },
        granted_capabilities: [],
        enabled: true,
        revoked: false,
        installed_at_ms: 1,
        updated_at_ms: 1,
      }],
    }
    vi.mocked(api.request).mockImplementation(async (path, options) => {
      if (path === '/agent-presets/mine' && options?.method !== 'PUT') return userDocument
      if (path === '/extensions') return inventory
      return undefined as never
    })
    await settle(() => root.render(<LocaleProvider><PresetsSettings /></LocaleProvider>))

    await settle(() => buttonWithLabel('Edit: 我的 Agent').click())
    expect(api.request).toHaveBeenCalledWith('/extensions')
    await settle(() => buttonWithLabel('Expand: tools.required 1.0.0').click())
    const endpoint = document.querySelector<HTMLInputElement>(
      '#plugin-extension-tools-required-1-0-0-endpoint',
    )!
    expect(endpoint.value).toBe('')
    await settle(() => changeInput(endpoint, 'https://required.example.test'))
    await settle(() => buttonWithText('Apply to draft').click())
    await settle(() => buttonWithText('Save preset').click())

    expect(api.request).toHaveBeenCalledWith('/agent-presets/mine', {
      method: 'PUT',
      body: {
        display_name: '我的 Agent',
        description: '作者原文',
        profile: {
          plugins: [{
            id: 'extension-tools-required-1-0-0',
            kind: 'ternilo.extension.package',
            enabled: true,
            config: {
              package_id: 'tools.required',
              version: '1.0.0',
              settings: { endpoint: 'https://required.example.test' },
            },
          }],
        },
      },
    })
  })
})

function buttonWithText(label: string) {
  const value = [...document.querySelectorAll<HTMLButtonElement>('button')]
    .find(item => item.textContent?.trim() === label)
  if (!value) throw new Error(`missing button ${label}`)
  return value
}

function buttonWithLabel(label: string) {
  const value = document.querySelector<HTMLButtonElement>(`button[aria-label="${label}"]`)
  if (!value) throw new Error(`missing button ${label}`)
  return value
}
