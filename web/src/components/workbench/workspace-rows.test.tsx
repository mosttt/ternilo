import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { LocalSession, Workspace } from '@/types'
import { LocaleProvider } from '@/i18n/provider'
import { SessionRow, WorkspaceRow, type RowDragHandlers } from './workspace-rows'
import css from './workspace-rows.module.css'

let host: HTMLDivElement
let root: Root

function memoryStorage(): Storage {
  const values = new Map<string, string>()
  return {
    get length() { return values.size },
    clear: () => values.clear(),
    getItem: key => values.get(key) ?? null,
    key: index => [...values.keys()][index] ?? null,
    removeItem: key => { values.delete(key) },
    setItem: (key, value) => { values.set(key, value) },
  }
}

const drag: RowDragHandlers = {
  draggable: false,
  marker: null,
  onDragStart: () => undefined,
  onDragOver: () => undefined,
  onDrop: () => undefined,
  onDragEnd: () => undefined,
}

const workspace: Workspace = {
  workspace_id: 'workspace-1',
  path: '/home/user/project',
  title: 'Project',
  created_at_ms: 0,
  updated_at_ms: 0,
  placement: 'local_node',
  status: 'online',
}

const session: LocalSession = {
  identity: { tenant_id: 'local', user_id: 'user', agent_id: 'agent', session_id: 'session-1' },
  workspace_id: workspace.workspace_id,
  workspace_path: workspace.path,
  title: 'Session one',
  permissions: 'workspace_write',
  model: { provider: 'profile_default' },
  agent_preset: 'standard',
  preset_plugins: [],
  profile_plugins: [],
  mode: 'execute',
  created_at_ms: 0,
  updated_at_ms: 0,
}

beforeEach(() => {
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true
  vi.stubGlobal('localStorage', memoryStorage())
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
  delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
})

function pointerDown(element: Element) {
  act(() => element.dispatchEvent(new MouseEvent('pointerdown', { bubbles: true, cancelable: true, button: 0 })))
}

describe('Workspace rows', () => {
  it('shows the selected machine and disables owner operations on a shared workspace', () => {
    const shared = { ...workspace, node_id: 'my-vps', access: { owner_user_id: 'other', is_owner: false, sources: [], role_limited: false, permissions: { view: true, submit: true, stop: false, configure: false } } }
    act(() => root.render(<LocaleProvider><WorkspaceRow workspace={shared} platform showPlacement expanded active drag={drag} onToggle={vi.fn()} onNewSession={vi.fn()} onRename={vi.fn()} onUnregister={vi.fn()} /></LocaleProvider>))
    expect(host.textContent).toContain('my-vps')
    pointerDown(host.querySelector('[aria-label="工作区“Project”的操作"]')!)
    const items = [...document.querySelectorAll<HTMLElement>('[role="menuitem"]')]
    expect(items.find(item => item.textContent === '重命名')!.getAttribute('data-disabled')).not.toBeNull()
    expect(items.find(item => item.textContent?.includes('移除'))!.getAttribute('data-disabled')).not.toBeNull()
    expect(document.body.textContent).not.toContain('共享…')
    expect(host.querySelector<HTMLButtonElement>('[aria-label="在“Project”中新建会话"]')!.disabled).toBe(false)
  })

  it('keeps workspace folding, creation, and the ellipsis menu as separate gestures', () => {
    const toggle = vi.fn()
    const create = vi.fn()
    const rename = vi.fn()
    const unregister = vi.fn()
    act(() => root.render(<WorkspaceRow
      workspace={workspace}
      expanded
      active
      drag={drag}
      onToggle={toggle}
      onNewSession={create}
      onRename={rename}
      onUnregister={unregister}
    />))

    const row = host.querySelector('[data-sidebar-workspace-row]')!
    expect(row.getAttribute('aria-expanded')).toBeNull()
    expect(host.querySelector('[data-sidebar-workspace-button]')?.getAttribute('aria-expanded')).toBe('true')
    act(() => host.querySelector<HTMLButtonElement>('[data-sidebar-workspace-button]')!.click())
    expect(toggle).toHaveBeenCalledOnce()

    act(() => host.querySelector<HTMLButtonElement>('[aria-label="在“Project”中新建会话"]')!.click())
    expect(create).toHaveBeenCalledOnce()
    expect(toggle).toHaveBeenCalledOnce()

    pointerDown(host.querySelector('[aria-label="工作区“Project”的操作"]')!)
    expect(document.querySelector('[role="menu"]')?.textContent).toContain('重命名')
    expect(document.querySelector('[role="menu"]')?.textContent).toContain('移除工作区')
  })

  it('opens the exact session menu only from ellipsis and gives blank rows no verbs or time', () => {
    const rename = vi.fn()
    const fork = vi.fn(async () => undefined)
    const archive = vi.fn(async () => undefined)
    const renderRow = (value: LocalSession) => act(() => root.render(<SessionRow
      session={value}
      now={0}
      active={false}
      drag={drag}
      onSelect={() => undefined}
      onRename={rename}
      onFork={fork}
      onArchive={archive}
    />))

    renderRow(session)
    act(() => host.querySelector('[data-sidebar-session-button]')?.dispatchEvent(new MouseEvent('dblclick', { bubbles: true })))
    expect(rename).not.toHaveBeenCalled()
    pointerDown(host.querySelector('[aria-label="会话“Session one”的操作"]')!)
    const labels = [...document.querySelectorAll('[role="menuitem"]')].map(item => item.textContent?.trim())
    expect(labels).toEqual(['重命名', '分叉会话', '归档会话'])

    act(() => document.querySelector<HTMLElement>('[role="menuitem"]')?.click())
    expect(rename).toHaveBeenCalledWith(session)

    renderRow({ ...session, blank: true })
    expect(host.querySelector('[data-sidebar-session-time]')).toBeNull()
    expect(host.querySelector('[aria-label^="会话"][aria-label$="的操作"]')).toBeNull()
  })

  it('exposes real one-step manual ordering in row menus and disables the list edge', () => {
    const moveUp = vi.fn()
    const moveDown = vi.fn()
    act(() => root.render(<WorkspaceRow
      workspace={workspace}
      expanded
      active={false}
      drag={{ ...drag, draggable: true }}
      order={{ kind: 'workspace', canMoveUp: false, canMoveDown: true, moveUp, moveDown }}
      onToggle={() => undefined}
      onNewSession={() => undefined}
      onRename={() => undefined}
      onUnregister={() => undefined}
    />))

    pointerDown(host.querySelector('[aria-label="工作区“Project”的操作"]')!)
    const up = document.querySelector<HTMLElement>('[data-sidebar-reorder="up"]')!
    const down = document.querySelector<HTMLElement>('[data-sidebar-reorder="down"]')!
    expect(up.textContent).toContain('工作区上移')
    expect(up.hasAttribute('data-disabled')).toBe(true)
    expect(down.textContent).toContain('工作区下移')
    act(() => down.click())
    expect(moveUp).not.toHaveBeenCalled()
    expect(moveDown).toHaveBeenCalledOnce()
  })

  it('keeps a blank session actionless even when manual ordering supplies row actions', () => {
    window.localStorage.setItem('ternilo.locale', 'en')
    act(() => root.render(
      <LocaleProvider>
        <SessionRow
          session={{ ...session, blank: true }}
          now={0}
          active={false}
          drag={{ ...drag, draggable: true }}
          order={{
            kind: 'session-group',
            canMoveUp: true,
            canMoveDown: false,
            moveUp: () => undefined,
            moveDown: () => undefined,
          }}
          onSelect={() => undefined}
          onRename={() => undefined}
          onFork={async () => undefined}
          onArchive={async () => undefined}
        />
      </LocaleProvider>,
    ))
    expect(host.querySelector('[data-sidebar-session-time]')).toBeNull()
    expect(host.querySelector('[aria-label="Session actions for New Session"]')).toBeNull()
    expect(document.querySelector('[role="menu"]')).toBeNull()
  })

  it('localizes the persisted placeholder title after a direct command finishes', () => {
    act(() => root.render(
      <LocaleProvider>
        <SessionRow
          session={{ ...session, title: 'New session' }}
          statuses={[{ state: 'completed', kind: 'completed' }]}
          now={0}
          active={false}
          drag={drag}
          onSelect={() => undefined}
          onRename={() => undefined}
          onFork={async () => undefined}
          onArchive={async () => undefined}
        />
      </LocaleProvider>,
    ))
    expect(host.querySelector('[data-sidebar-session-title]')?.textContent).toBe('新会话')
    expect(host.textContent).not.toContain('New session')
  })

  it('renders a canonical child as a navigable nested row with an independent parent fold control', () => {
    const select = vi.fn()
    const toggle = vi.fn()
    const child: LocalSession = {
      ...session,
      identity: { ...session.identity, session_id: 'child-session' },
      parent_session_id: session.identity.session_id,
      subagent: { subagent_id: 'agent-child', provider: 'in-process', transcript_kind: 'conversation' },
    }
    act(() => root.render(<SessionRow
      session={child}
      statuses={[{ state: 'running', kind: 'running' }]}
      now={0}
      active
      drag={drag}
      depth={1}
      childCount={1}
      childrenExpanded={false}
      onToggleChildren={toggle}
      onSelect={select}
      onRename={() => undefined}
      onFork={async () => undefined}
      onArchive={async () => undefined}
    />))

    const row = host.querySelector('[data-sidebar-session-row]')!
    expect(row.getAttribute('data-session-depth')).toBe('1')
    expect(row.hasAttribute('data-subagent-session')).toBe(true)
    expect(row.getAttribute('aria-level')).toBe('3')
    const fold = host.querySelector<HTMLButtonElement>('[data-sidebar-session-children-toggle]')!
    expect(fold.classList.contains(css.childToggle)).toBe(true)
    expect(fold.type).toBe('button')
    expect(fold.getAttribute('aria-label')).toBe('展开“Session one”的子 Agent')
    expect(fold.getAttribute('aria-expanded')).toBe('false')
    expect(fold.getAttribute('data-state')).toBe('running')
    act(() => fold.click())
    expect(toggle).toHaveBeenCalledOnce()
    act(() => host.querySelector<HTMLButtonElement>('[data-sidebar-session-button]')!.click())
    expect(select).toHaveBeenCalledWith('child-session')
  })

  it('renders the same row contract from the English locale dictionary', () => {
    window.localStorage.setItem('ternilo.locale', 'en')
    act(() => root.render(
      <LocaleProvider>
        <WorkspaceRow
          workspace={workspace}
          expanded={false}
          active={false}
          drag={drag}
          onToggle={() => undefined}
          onNewSession={() => undefined}
          onRename={() => undefined}
          onUnregister={() => undefined}
        />
      </LocaleProvider>,
    ))
    expect(host.querySelector('[aria-label="Workspace actions for Project"]')).not.toBeNull()
    expect(host.querySelector('[aria-label="Local node · Online"]')).not.toBeNull()
  })

  it('keeps viewer rows selectable and foldable without exposing mutation or drag controls', () => {
    const toggle = vi.fn()
    const select = vi.fn()
    act(() => root.render(<>
      <WorkspaceRow
        workspace={workspace}
        expanded={false}
        active={false}
        drag={{ ...drag, draggable: true }}
        readOnly
        onToggle={toggle}
        onNewSession={vi.fn()}
        onRename={vi.fn()}
        onUnregister={vi.fn()}
      />
      <SessionRow
        session={session}
        now={0}
        active={false}
        drag={{ ...drag, draggable: true }}
        readOnly
        onSelect={select}
        onRename={vi.fn()}
        onFork={async () => undefined}
        onArchive={async () => undefined}
      />
    </>))

    act(() => host.querySelector<HTMLButtonElement>('[data-sidebar-workspace-button]')!.click())
    act(() => host.querySelector<HTMLButtonElement>('[data-sidebar-session-button]')!.click())
    expect(toggle).toHaveBeenCalledOnce()
    expect(select).toHaveBeenCalledWith('session-1')
    expect(host.querySelector('[data-sidebar-workspace-row]')?.getAttribute('draggable')).toBe('false')
    expect(host.querySelector('[data-sidebar-session-row]')?.getAttribute('draggable')).toBe('false')
    expect(host.querySelector('[aria-label="工作区“Project”的操作"]')).toBeNull()
    expect(host.querySelector('[aria-label="在“Project”中新建会话"]')).toBeNull()
    expect(host.querySelector('[aria-label="会话“Session one”的操作"]')).toBeNull()
  })
})
