import * as React from 'react'
import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { MessageSquare, Route } from 'lucide-react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { Translate } from '@/i18n/runtime'
import type { ConversationViewContribution } from '@/plugins/conversation-registry'
import type { LocalSession } from '@/types'
import { ConversationSession, ConversationSessionHeader } from './conversation-session'
vi.mock('./workspace-panel', () => ({ WorkspaceHeaderActions: () => null }))

;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean })
  .IS_REACT_ACT_ENVIRONMENT = true

const session: LocalSession = {
  identity: { tenant_id: 'local', user_id: 'user', agent_id: 'agent', session_id: 'session' },
  workspace_id: 'workspace',
  workspace_path: '/workspace',
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

const views: ConversationViewContribution[] = [
  { id: 'chat', order: 10, icon: MessageSquare, label: () => 'Chat', render: () => null, primary: true },
  { id: 'trajectory', order: 20, icon: Route, label: () => 'Trajectory', render: () => null },
]

const t = ((key: string) => key) as Translate<'conversation'>
let host: HTMLDivElement
let root: Root

beforeEach(() => {
  host = document.createElement('div')
  document.body.append(host)
  root = createRoot(host)
})

afterEach(() => {
  act(() => root.unmount())
  host.remove()
})

describe('ConversationSession tabs', () => {
  it('uses roving focus, keyboard navigation, and a labelled tabpanel', () => {
    function Harness() {
      const [view, setView] = React.useState('chat')
      return <>
        <ConversationSessionHeader
          session={session}
          workspace={null}
          blank={false}
          view={view}
          onView={setView}
          onOpenMobileSidebar={vi.fn()}
          actions={null}
          t={t}
          workspaceSessionTitle="New session"
          views={views}
        />
        <ConversationSession view={view} views={views.map(item => item.id)} content={<p>{view}</p>} />
      </>
    }

    act(() => root.render(<Harness />))
    const tabs = [...host.querySelectorAll<HTMLButtonElement>('[role="tab"]')]
    expect(tabs.map(tab => tab.tabIndex)).toEqual([0, -1])
    expect(tabs[0].getAttribute('aria-controls')).toBe('conversation-view-chat-panel')

    tabs[0].focus()
    act(() => tabs[0].dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowRight', bubbles: true })))

    expect(tabs[1].getAttribute('aria-selected')).toBe('true')
    expect(tabs.map(tab => tab.tabIndex)).toEqual([-1, 0])
    expect(document.activeElement).toBe(tabs[1])
    const panel = host.querySelector<HTMLElement>('[role="tabpanel"]:not([hidden])')!
    expect(panel.id).toBe('conversation-view-trajectory-panel')
    expect(panel.getAttribute('aria-labelledby')).toBe('conversation-view-trajectory-tab')
    expect(panel.textContent).toBe('trajectory')

    act(() => tabs[1].dispatchEvent(new KeyboardEvent('keydown', { key: 'Home', bubbles: true })))
    expect(tabs[0].getAttribute('aria-selected')).toBe('true')
    expect(document.activeElement).toBe(tabs[0])
  })

  it('does not expose tabs whose panels are unavailable while history is loading', () => {
    act(() => root.render(
      <ConversationSessionHeader
        session={session}
        workspace={null}
        blank={false}
        view="chat"
        onView={vi.fn()}
        onOpenMobileSidebar={vi.fn()}
        actions={<button type="button">Actions</button>}
        t={t}
        workspaceSessionTitle="New session"
        views={views}
        viewTabsAvailable={false}
      />,
    ))
    expect(host.querySelector('[role="tablist"]')).toBeNull()
    expect(host.textContent).toContain('Actions')
  })

  it('renders the workspace path as non-interactive information', () => {
    act(() => root.render(
      <ConversationSessionHeader
        session={session}
        workspace={null}
        blank={false}
        view="chat"
        onView={vi.fn()}
        onOpenMobileSidebar={vi.fn()}
        actions={null}
        t={t}
        workspaceSessionTitle="New session"
        views={views}
      />,
    ))
    const path = host.querySelector<HTMLElement>('[data-session-workspace]')!
    expect(path.tagName).toBe('SPAN')
    expect(path.textContent).toBe('/workspace')
    expect(path.closest('button, a')).toBeNull()
  })

  it('uses the localized fallback for the persisted placeholder title', () => {
    act(() => root.render(
      <ConversationSessionHeader
        session={{ ...session, title: 'New session' }}
        workspace={null}
        blank={false}
        view="chat"
        onView={vi.fn()}
        onOpenMobileSidebar={vi.fn()}
        actions={null}
        t={t}
        workspaceSessionTitle="新会话"
        views={views}
      />,
    ))
    expect(host.querySelector('[data-session-title]')?.textContent).toBe('新会话')
  })

  it('opens the shared Files page with the current conversation filter', () => {
    act(() => root.render(<ConversationSessionHeader
      session={{ ...session, identity: { ...session.identity, session_id: 'session/a' } }}
      workspace={null} blank={false} view="chat" onView={vi.fn()}
      onOpenMobileSidebar={vi.fn()} actions={null} t={t} workspaceSessionTitle="New session" views={views}
    />))
    act(() => host.querySelector<HTMLButtonElement>('[data-session-files]')!.click())
    expect(location.pathname).toBe('/files')
    expect(new URLSearchParams(location.search).get('session_id')).toBe('session/a')
    history.replaceState({}, '', '/')
  })
})
