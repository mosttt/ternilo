import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { LocaleProvider } from '@/i18n/provider'
import type { PluginCatalogEntry, PluginEntry } from '@/types'
import { PluginConfigCard } from './plugin-config-card'

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

const entry: PluginEntry = {
  id: 'local-files',
  kind: 'ternilo.files.local',
  enabled: true,
  config: {},
}

const metadata = (): PluginCatalogEntry => ({
  kind: entry.kind,
  description: 'Workspace files',
  requires: [],
  provides: [],
  config_schema: {
    type: 'object',
    properties: {
      max_read_bytes: { type: 'integer', default: 2_097_152, description: 'Maximum read bytes' },
    },
  },
})

let host: HTMLDivElement
let root: Root

beforeEach(() => {
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true
  host = document.createElement('div')
  document.body.append(host)
  root = createRoot(host)
})

afterEach(() => {
  act(() => root.unmount())
  host.remove()
  localStorage.clear()
  delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
})

async function settle(action?: () => void) {
  await act(async () => {
    action?.()
    await Promise.resolve()
    await Promise.resolve()
  })
}

function render(pluginMetadata: PluginCatalogEntry) {
  root.render(
    <LocaleProvider>
      <PluginConfigCard
        entry={{ ...entry }}
        metadata={pluginMetadata}
        overridden={false}
        onToggle={vi.fn()}
        onSave={vi.fn()}
        onReset={vi.fn()}
      />
    </LocaleProvider>,
  )
}

describe('PluginConfigCard draft lifecycle', () => {
  it('keeps editable changes while a running task blocks save, then saves the same draft', async () => {
    const onSave = vi.fn(async () => undefined)
    const draw = (blocked: boolean) => root.render(<LocaleProvider><PluginConfigCard
      entry={{ ...entry }} metadata={metadata()} overridden={false} onSave={onSave}
      disabledReason={blocked ? '当前任务结束后才能保存' : undefined} /></LocaleProvider>)
    await settle(() => draw(false))
    await settle(() => host.querySelector<HTMLButtonElement>('[aria-label="展开: local-files"]')?.click())
    const input = host.querySelector<HTMLInputElement>('#plugin-local-files-max_read_bytes')!
    await settle(() => {
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')?.set?.call(input, '3145728')
      input.dispatchEvent(new Event('input', { bubbles: true }))
    })
    const save = [...host.querySelectorAll<HTMLButtonElement>('button')].find(button => button.textContent === '保存')!
    await settle(() => draw(true))
    expect(save.disabled).toBe(true)
    expect(input.disabled).toBe(false)
    expect(input.value).toBe('3145728')
    expect(host.querySelector('[role="status"]')?.textContent).toContain('任务结束')
    await settle(() => draw(false))
    expect(save.disabled).toBe(false)
    expect(input.value).toBe('3145728')
    await settle(() => save.click())
    expect(onSave).toHaveBeenCalledWith({ ...entry, config: { max_read_bytes: 3145728 } })
  })

  it.each([
    { hostLimit: 0, configured: 0, expected: '不限制工具调用次数' },
    { hostLimit: 512, configured: 0, expected: '每轮最多 512 次' },
    { hostLimit: 512, configured: 8, expected: '每轮最多 8 次' },
    { hostLimit: 4, configured: 8, expected: '每轮最多 4 次' },
    { hostLimit: undefined, configured: 0, expected: null },
  ])('shows the effective tool limit for host=$hostLimit and plugin=$configured', async ({ hostLimit, configured, expected }) => {
    await settle(() => root.render(<LocaleProvider><PluginConfigCard
      entry={{ id: 'agent-loop', kind: 'ternilo.agent.react', enabled: true, config: { max_tool_calls: configured } }}
      metadata={{ kind: 'ternilo.agent.react', description: 'Agent', requires: [], provides: [], config_schema: {
        type: 'object', properties: { max_tool_calls: { type: 'integer', minimum: 0, default: 512 } },
      } }}
      hostToolCallLimit={hostLimit}
      overridden={false}
      onSave={vi.fn()}
    /></LocaleProvider>))
    await settle(() => host.querySelector<HTMLButtonElement>('[aria-label="展开: agent-loop"]')?.click())
    const hint = host.querySelector('[data-tool-call-limit]')
    if (expected) expect(hint?.textContent).toContain(expected)
    else expect(hint).toBeNull()
    if (hostLimit && (configured === 0 || configured > hostLimit)) expect(hint?.textContent).toContain('已受本机或平台上限约束')
    else expect(hint?.textContent ?? '').not.toContain('已受本机或平台上限约束')
  })

  it('keeps a user draft when an equivalent catalog object is projected again', async () => {
    await settle(() => render(metadata()))
    await settle(() => host.querySelector<HTMLButtonElement>('[aria-label="展开: local-files"]')?.click())
    const input = host.querySelector<HTMLInputElement>('#plugin-local-files-max_read_bytes')!
    await settle(() => {
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')?.set?.call(input, '3145728')
      input.dispatchEvent(new Event('input', { bubbles: true }))
    })
    const discard = [...host.querySelectorAll<HTMLButtonElement>('button')]
      .find(button => button.textContent?.trim() === '放弃修改')!
    expect(discard.disabled).toBe(false)

    await settle(() => render(metadata()))
    expect(input.value).toBe('3145728')
    expect(discard.disabled).toBe(false)

    await settle(() => discard.click())
    expect(input.value).toBe('2097152')
    expect(discard.disabled).toBe(true)
  })
})
