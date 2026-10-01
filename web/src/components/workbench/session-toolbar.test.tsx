import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { LocaleProvider } from '@/i18n/provider'
import { TooltipProvider } from '@/components/ui/tooltip'
import type { AgentPresetRoster, LocalSession } from '@/types'
import { SessionHeaderActions, SessionToolbar } from './session-toolbar'

let host: HTMLDivElement
let root: Root

const session: LocalSession = {
  identity: { tenant_id: 'tenant', user_id: 'user', agent_id: 'agent', session_id: 'session' },
  workspace_id: 'workspace',
  placement: 'cloud',
  workspace_path: 'cloud / workspace',
  title: 'Session',
  permissions: 'workspace_write',
  model: { provider: 'profile_default' },
  agent_preset: 'standard',
  preset_plugins: [],
  profile_plugins: [],
  mode: 'execute',
  created_at_ms: 1,
  updated_at_ms: 1,
}

beforeEach(() => {
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true
  host = document.createElement('div')
  document.body.append(host)
  root = createRoot(host)
})

afterEach(() => {
  act(() => root.unmount())
  host.remove()
  document.querySelectorAll('[data-radix-popper-content-wrapper]').forEach(node => node.remove())
  vi.restoreAllMocks()
  delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
})

function render(value: LocalSession) {
  act(() => root.render(
    <LocaleProvider>
      <SessionToolbar session={value} onSetPermissions={vi.fn(async () => undefined)} />
    </LocaleProvider>,
  ))
  act(() => host.querySelector<HTMLButtonElement>('button')!.dispatchEvent(
    new MouseEvent('pointerdown', { bubbles: true, cancelable: true, button: 0 }),
  ))
  return [...document.querySelectorAll<HTMLElement>('[role="menuitemradio"]')].map(item => item.textContent?.trim())
}

describe('Session permission placement policy', () => {
  it('blocks changes when configuration access is revoked while the permission menu is open', () => {
    const onSetPermissions = vi.fn(async () => undefined)
    const renderToolbar = (configure: boolean) => act(() => root.render(<LocaleProvider>
      <SessionToolbar session={{ ...session, access: { owner_user_id: 'owner', storage_user_id: 'owner', ownership_revision: 0, is_execution_owner: false, is_owner: false, sources: [], role_limited: false, permissions: { view: true, submit: true, stop: false, configure } } }} onSetPermissions={onSetPermissions} />
    </LocaleProvider>))
    renderToolbar(true)
    act(() => host.querySelector('button')!.dispatchEvent(new MouseEvent('pointerdown', { bubbles: true, cancelable: true, button: 0 })))
    expect(document.querySelectorAll('[role="menuitemradio"]')).toHaveLength(2)
    renderToolbar(false)
    for (const item of document.querySelectorAll<HTMLElement>('[role="menuitemradio"]')) {
      expect(item.hasAttribute('data-disabled')).toBe(true)
      act(() => item.click())
    }
    expect(onSetPermissions).not.toHaveBeenCalled()
  })

  it('hides full access for Cloud Sessions and keeps it for local-node Sessions', () => {
    expect(render(session)).toEqual(['只读', '工作区写入'])
    act(() => document.body.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true })))
    expect(render({ ...session, placement: 'local_node' })).toEqual(['只读', '工作区写入', '完整访问'])
  })
})

describe('Session header actions', () => {
  it('offers the usage limit only for Cloud sessions and respects revoked configuration access', () => {
    const onEditModelLimit = vi.fn()
    const renderHeader = (placement: 'cloud' | 'local_node', configure: boolean) => act(() => root.render(<LocaleProvider><TooltipProvider>
      <SessionHeaderActions
        session={{ ...session, placement, access: { owner_user_id: 'owner', storage_user_id: 'owner', ownership_revision: 0, is_execution_owner: false, is_owner: false, sources: [], role_limited: false, permissions: { view: true, submit: true, stop: false, configure } } }}
        presets={{ default_id: 'standard', authorable: false, presets: [] }}
        onSetPreset={vi.fn(async () => undefined)} onTogglePlan={vi.fn(async () => undefined)} onChooseWorkspace={vi.fn()}
        onRename={vi.fn()} onFork={vi.fn(async () => undefined)} onArchive={vi.fn(async () => undefined)} onExport={vi.fn(async () => undefined)} onEditModelLimit={onEditModelLimit}
      />
    </TooltipProvider></LocaleProvider>))
    renderHeader('cloud', true)
    act(() => host.querySelector('[aria-label="更多会话操作"]')!.dispatchEvent(new MouseEvent('pointerdown', { bubbles: true, cancelable: true, button: 0 })))
    const findLimit = () => [...document.querySelectorAll<HTMLElement>('[role="menuitem"]')].find(item => item.textContent === '任务用量上限')
    expect(findLimit()).toBeDefined()
    renderHeader('cloud', false)
    expect(findLimit()!.hasAttribute('data-disabled')).toBe(true)
    act(() => findLimit()!.click())
    expect(onEditModelLimit).not.toHaveBeenCalled()
    renderHeader('local_node', true)
    expect(findLimit()).toBeUndefined()
  })

  it('blocks preset selection when configuration access is revoked while the menu is open', () => {
    const onSetPreset = vi.fn(async () => undefined)
    const renderHeader = (configure: boolean) => act(() => root.render(<LocaleProvider><TooltipProvider>
      <SessionHeaderActions
        session={{ ...session, blank: true, access: { owner_user_id: 'owner', storage_user_id: 'owner', ownership_revision: 0, is_execution_owner: false, is_owner: false, sources: [], role_limited: false, permissions: { view: true, submit: true, stop: false, configure } } }}
        presets={{ default_id: 'standard', authorable: false, presets: [{ id: 'standard', display_name: 'Standard', description: 'Standard agent', trust: 'system' }] }}
        onSetPreset={onSetPreset} onTogglePlan={vi.fn(async () => undefined)} onChooseWorkspace={vi.fn()}
        onRename={vi.fn()} onFork={vi.fn(async () => undefined)} onArchive={vi.fn(async () => undefined)} onExport={vi.fn(async () => undefined)}
      />
    </TooltipProvider></LocaleProvider>))
    renderHeader(true)
    act(() => host.querySelector('button')!.dispatchEvent(new MouseEvent('pointerdown', { bubbles: true, cancelable: true, button: 0 })))
    expect(document.querySelectorAll('[role="menuitem"]')).toHaveLength(1)
    renderHeader(false)
    const preset = document.querySelector<HTMLElement>('[role="menuitem"]')!
    expect(preset.hasAttribute('data-disabled')).toBe(true)
    act(() => preset.click())
    expect(onSetPreset).not.toHaveBeenCalled()
  })

  it('invokes the export action from the more menu', () => {
    const onExport = vi.fn(async () => undefined)
    const roster: AgentPresetRoster = {
      default_id: 'standard',
      authorable: false,
      presets: [{
        id: 'standard',
        display_name: 'Standard',
        description: 'Standard agent',
        trust: 'system',
      }],
    }
    act(() => root.render(
      <LocaleProvider><TooltipProvider>
          <SessionHeaderActions
            session={session}
            presets={roster}
            onSetPreset={vi.fn(async () => undefined)}
            onTogglePlan={vi.fn(async () => undefined)}
            onChooseWorkspace={vi.fn()}
            onRename={vi.fn()}
            onFork={vi.fn(async () => undefined)}
            onArchive={vi.fn(async () => undefined)}
            onExport={onExport}
          />
      </TooltipProvider></LocaleProvider>,
    ))
    const more = [...host.querySelectorAll<HTMLButtonElement>('button')]
      .find(button => button.getAttribute('aria-label') === '更多会话操作')
    expect(more).toBeDefined()
    act(() => more!.dispatchEvent(new MouseEvent('pointerdown', { bubbles: true, cancelable: true, button: 0 })))
    const exportItem = [...document.querySelectorAll<HTMLElement>('[role="menuitem"]')]
      .find(item => item.textContent?.trim() === '导出会话')
    expect(exportItem).toBeDefined()
    act(() => exportItem!.dispatchEvent(new MouseEvent('click', { bubbles: true, cancelable: true })))
    expect(onExport).toHaveBeenCalledOnce()
    expect(document.body.contains(exportItem!)).toBe(false)

    act(() => more!.dispatchEvent(new MouseEvent('pointerdown', { bubbles: true, cancelable: true, button: 0 })))
    const keyboardExportItem = [...document.querySelectorAll<HTMLElement>('[role="menuitem"]')]
      .find(item => item.textContent?.trim() === '导出会话')
    expect(keyboardExportItem).toBeDefined()
    keyboardExportItem!.focus()
    act(() => keyboardExportItem!.dispatchEvent(new KeyboardEvent('keydown', {
      key: 'Enter', bubbles: true, cancelable: true,
    })))
    expect(onExport).toHaveBeenCalledTimes(2)
    expect(document.body.contains(keyboardExportItem!)).toBe(false)
  })
})
