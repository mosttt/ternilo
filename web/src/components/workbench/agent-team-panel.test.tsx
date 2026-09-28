// @vitest-environment jsdom

import * as React from 'react'
import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { ApiError } from '@/api/client'
import { LocaleProvider } from '@/i18n/provider'
import type { AgentTeamSnapshot, LocalSession, SessionEvent } from '@/types'
import { agentTeamApi } from './agent-team-api'
import { AgentTeamSurface, AgentTeamTrigger, useOpenAgentTeam } from './agent-team-panel'
import { SessionLineage } from './session-lineage'
import { SubagentEventRow } from './chat/observability-event-rows'

let host: HTMLDivElement
let root: Root

beforeEach(() => {
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true
  host = document.createElement('div')
  document.body.append(host)
  root = createRoot(host)
  vi.spyOn(agentTeamApi, 'snapshot').mockResolvedValue(teamSnapshot())
  vi.spyOn(agentTeamApi, 'events').mockResolvedValue([])
  vi.spyOn(agentTeamApi, 'createTask').mockResolvedValue({} as never)
  vi.spyOn(agentTeamApi, 'replaceTask').mockResolvedValue({} as never)
  vi.spyOn(agentTeamApi, 'deleteTask').mockResolvedValue(undefined)
  vi.spyOn(agentTeamApi, 'sendMessage').mockResolvedValue({} as never)
  vi.spyOn(agentTeamApi, 'markMessageRead').mockResolvedValue({} as never)
})

afterEach(() => {
  act(() => root.unmount())
  host.remove()
  document.querySelectorAll('[data-agent-team-panel]').forEach(node => node.remove())
  vi.restoreAllMocks()
  delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
})

function session(id: string): LocalSession {
  return {
    identity: { tenant_id: 'local', user_id: 'user', agent_id: 'agent', session_id: id },
    workspace_id: 'workspace', workspace_path: '/tmp/workspace', title: id,
    permissions: 'workspace_write', model: { provider: 'profile_default' },
    agent_preset: 'standard', preset_plugins: [], profile_plugins: [], mode: 'execute',
    created_at_ms: 1, updated_at_ms: 1,
  }
}

function childSession(id: string, subagentId: string): LocalSession {
  return {
    ...session(id),
    parent_session_id: 'owner',
    subagent: { subagent_id: subagentId, provider: 'in-process', transcript_kind: 'conversation' },
  }
}

function teamSnapshot(subagents: Array<{ id: string; label: string; provider?: string }> = []): AgentTeamSnapshot {
  return {
    team_id: 'team-fixture',
    current_member_id: 'member-lead',
    members: [
      { id: 'member-lead', label: 'Lead', role: 'lead' },
      ...subagents.map((subagent, index) => ({
        id: `member-${index + 1}`,
        parent_id: 'member-lead',
        subagent_id: subagent.id,
        label: subagent.label,
        provider: subagent.provider ?? 'in-process',
        role: 'subagent' as const,
      })),
    ],
    tasks: [],
    messages: [],
  }
}

function subagentEvent(seq: number, subagent: Record<string, unknown>): SessionEvent {
  return {
    seq, type: 'subagent_updated', run_id: 'run', occurred_at_ms: seq,
    subagent: {
      subagent_id: `agent-${seq}`,
      provider: 'in-process',
      label: `Agent ${seq}`,
      task: `Task ${seq}`,
      supports_followup: true,
      output: null,
      error: null,
      created_at_ms: seq,
      updated_at_ms: seq,
      ...subagent,
    },
  }
}

function OpenTeam({ id }: { id?: string }) {
  const open = useOpenAgentTeam()
  return <button type="button" onClick={() => open?.(id)}>open fixture team</button>
}

function surface(events: SessionEvent[], children: React.ReactNode, overrides: {
  sessionId?: string
  sessions?: LocalSession[]
  onFollowup?: (id: string, message: string) => Promise<void>
  onStop?: (id: string) => Promise<void>
  onOpenSession?: (id: string) => void
  readOnly?: boolean
  liveSnapshot?: AgentTeamSnapshot | null
} = {}) {
  return <LocaleProvider>
    <AgentTeamSurface
      sessionId={overrides.sessionId ?? 'owner'}
      events={events}
      sessions={overrides.sessions ?? [session('owner'), session('agent-addressed')]}
      liveSnapshot={overrides.liveSnapshot}
      readOnly={overrides.readOnly}
      onOpenSession={overrides.onOpenSession ?? vi.fn()}
      onFollowup={overrides.onFollowup ?? vi.fn(async () => {})}
      onStop={overrides.onStop ?? vi.fn(async () => {})}
    >
      {children}
    </AgentTeamSurface>
  </LocaleProvider>
}

function render(node: React.ReactNode) {
  act(() => root.render(node))
}

function dialog(): HTMLElement {
  const value = document.querySelector<HTMLElement>('[data-agent-team-panel]')
  if (!value) throw new Error('Agent Team dialog not found')
  return value
}

function button(rootElement: ParentNode, name: string): HTMLButtonElement {
  const value = [...rootElement.querySelectorAll<HTMLButtonElement>('button')]
    .find(candidate => candidate.getAttribute('aria-label') === name || candidate.textContent?.trim() === name)
  if (!value) throw new Error(`button ${name} not found`)
  return value
}

function changeTextarea(textarea: HTMLTextAreaElement, value: string) {
  const setter = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value')?.set
  setter?.call(textarea, value)
  textarea.dispatchEvent(new Event('input', { bubbles: true }))
}

function changeInput(input: HTMLInputElement | HTMLSelectElement, value: string) {
  const prototype = input instanceof HTMLSelectElement ? HTMLSelectElement.prototype : HTMLInputElement.prototype
  const setter = Object.getOwnPropertyDescriptor(prototype, 'value')?.set
  setter?.call(input, value)
  input.dispatchEvent(new Event('change', { bubbles: true }))
}

async function waitForMember(id: string) {
  await act(async () => {
    await vi.waitFor(() => {
      const member = document.querySelector(`[data-agent-team-member="${id}"]`)
      if (!member) throw new Error(`member ${id} not found: ${document.body.textContent}`)
    })
  })
}

describe('AgentTeamSurface', () => {
  it('uses the canonical child Session terminal event instead of a stale parent running event', async () => {
    vi.mocked(agentTeamApi.snapshot).mockResolvedValue(teamSnapshot([
      { id: 'agent-1', label: 'Researcher' },
    ]))
    vi.mocked(agentTeamApi.events).mockResolvedValue([
      { seq: 0, type: 'turn_started', run_id: 'child-run', occurred_at_ms: 10 },
      { seq: 1, type: 'turn_finished', run_id: 'child-run', occurred_at_ms: 20 },
    ])
    render(surface(
      [subagentEvent(1, { subagent_id: 'agent-1', status: 'running', label: 'Researcher' })],
      <OpenTeam id="agent-1" />,
      { sessions: [session('owner'), childSession('child-session', 'agent-1')] },
    ))

    await act(async () => { button(host, 'open fixture team').click() })
    await waitForMember('agent-1')
    const member = dialog().querySelector<HTMLElement>('[data-agent-team-member="agent-1"]')!
    expect(agentTeamApi.events).toHaveBeenCalledWith('child-session', expect.any(AbortSignal))
    expect(member.dataset.status).toBe('idle')
    expect(member.querySelector('[data-agent-team-followup]')).not.toBeNull()
  })

  it('keeps the canonical Team readable for viewers without exposing mutations', async () => {
    const events = [subagentEvent(1, { status: 'idle', label: 'Researcher' })]
    vi.mocked(agentTeamApi.snapshot).mockResolvedValue(teamSnapshot([
      { id: 'agent-1', label: 'Researcher' },
    ]))
    render(surface(events, <OpenTeam id="agent-1" />, { readOnly: true }))

    await act(async () => { button(host, 'open fixture team').click() })
    await waitForMember('agent-1')
    const panel = dialog()
    expect(panel.querySelector('[data-agent-team-viewer-read-only]')).not.toBeNull()
    expect(panel.textContent).toContain('只读者')
    expect(panel.querySelector('[data-agent-team-followup]')).toBeNull()
    expect(panel.querySelector('[data-agent-team-stop]')).toBeNull()
    expect([...panel.querySelectorAll('button')].some(item => item.textContent?.includes('发送消息'))).toBe(false)

    await act(async () => button(panel, '任务').click())
    expect([...panel.querySelectorAll('button')].some(item => item.textContent?.trim() === '新建任务')).toBe(false)
  })

  it('uses the live Team snapshot without periodic canonical refreshes', async () => {
    vi.useFakeTimers()
    try {
      const liveSnapshot = teamSnapshot()
      liveSnapshot.members[0]!.label = 'New session'
      render(surface([], <OpenTeam />, { liveSnapshot }))
      await act(async () => {
        button(host, 'open fixture team').click()
        await Promise.resolve()
        await Promise.resolve()
      })
      expect(dialog().textContent).toContain('Team 负责人')
      expect(dialog().textContent).toContain('新会话')
      expect(dialog().textContent).not.toContain('New session')
      expect(agentTeamApi.snapshot).not.toHaveBeenCalled()
      await act(async () => { await vi.advanceTimersByTimeAsync(9_000) })
      expect(agentTeamApi.snapshot).not.toHaveBeenCalled()
      act(() => button(dialog(), '关闭').click())
      await act(async () => { await vi.advanceTimersByTimeAsync(6_000) })
      expect(agentTeamApi.snapshot).not.toHaveBeenCalled()
    } finally {
      vi.useRealTimers()
    }
  })

  it('opens from an ordinary Session and never renders its local Session id', async () => {
    vi.mocked(agentTeamApi.snapshot).mockResolvedValue(teamSnapshot())
    render(surface([], <AgentTeamTrigger label />, { sessionId: 'private-local-session-id' }))

    await act(async () => {
      button(host, '打开 Agent Team').click()
      await Promise.resolve()
    })
    await waitForMember('member-lead')
    const panel = dialog()
    expect(agentTeamApi.snapshot).toHaveBeenCalledWith('private-local-session-id', expect.any(AbortSignal))
    expect(panel.textContent).toContain('Team 负责人')
    expect(panel.textContent).not.toContain('private-local-session-id')
  })

  it('renders canonical roster facts and exposes only state-backed actions', async () => {
    const followup = vi.fn(async () => {})
    const stop = vi.fn(async () => {})
    const events = [
      subagentEvent(1, { status: 'idle', label: 'Researcher', task: 'Inspect files', output: 'Found three issues' }),
      subagentEvent(2, { status: 'failed', label: 'Reviewer', error: 'Provider failed' }),
      subagentEvent(3, { status: 'running', label: 'One shot', supports_followup: false }),
      subagentEvent(4, { status: 'completed', label: 'Settled' }),
      subagentEvent(5, { status: 'idle', label: 'Historical', supports_followup: undefined }),
    ]
    vi.mocked(agentTeamApi.snapshot).mockResolvedValue(teamSnapshot(events.map(event => {
      const subagent = event.subagent as Record<string, string>
      return { id: subagent.subagent_id, label: subagent.label, provider: subagent.provider }
    })))
    render(surface(events, <OpenTeam id="agent-1" />, { onFollowup: followup, onStop: stop }))

    await act(async () => {
      button(host, 'open fixture team').click()
    })
    await waitForMember('agent-1')
    let panel = dialog()
    expect(panel.querySelector('[data-agent-team-member="agent-1"][data-selected]')).not.toBeNull()
    expect(panel.textContent).toContain('Researcher')
    expect(panel.textContent).toContain('in-process')
    expect(panel.textContent).toContain('Inspect files')
    expect(panel.textContent).toContain('Found three issues')
    expect(panel.textContent).toContain('Provider failed')
    expect(panel.textContent).toContain('一次性 Agent')
    expect(panel.textContent).toContain('没有可验证的续发能力')

    const followupBox = panel.querySelector<HTMLElement>('[data-agent-team-member="agent-1"]')!
    await act(async () => {
      changeTextarea(followupBox.querySelector('textarea')!, 'Check the tests too')
      await Promise.resolve()
    })
    await act(async () => {
      button(followupBox, '发送').click()
      await Promise.resolve()
    })
    expect(followup).toHaveBeenCalledWith('agent-1', 'Check the tests too')
    expect(document.querySelector('[data-agent-team-panel]')).toBeNull()

    await act(async () => {
      button(host, 'open fixture team').click()
    })
    await waitForMember('agent-3')
    panel = dialog()
    const running = panel.querySelector<HTMLElement>('[data-agent-team-member="agent-3"]')!
    await act(async () => {
      button(running, '停止').click()
      await Promise.resolve()
    })
    expect(stop).toHaveBeenCalledWith('agent-3')
    expect(document.querySelector('[data-agent-team-panel]')).toBeNull()

    await act(async () => {
      button(host, 'open fixture team').click()
    })
    await waitForMember('agent-4')
    panel = dialog()
    expect(panel.querySelector('[data-agent-team-member="agent-4"] textarea')).toBeNull()
    expect(panel.querySelector('[data-agent-team-member="agent-5"] textarea')).toBeNull()
  })

  it('keeps a failed follow-up draft visible and leaves the Team panel open', async () => {
    const events = [subagentEvent(1, { status: 'idle', label: 'Researcher' })]
    vi.mocked(agentTeamApi.snapshot).mockResolvedValue(teamSnapshot([
      { id: 'agent-1', label: 'Researcher' },
    ]))
    render(surface(events, <OpenTeam id="agent-1" />, {
      onFollowup: vi.fn(async () => { throw new Error('follow-up transport offline') }),
    }))

    await act(async () => { button(host, 'open fixture team').click() })
    await waitForMember('agent-1')
    const member = dialog().querySelector<HTMLElement>('[data-agent-team-member="agent-1"]')!
    const textarea = member.querySelector<HTMLTextAreaElement>('textarea')!
    act(() => changeTextarea(textarea, 'Please keep this draft'))
    await act(async () => {
      button(member, '发送').click()
      await Promise.resolve()
    })

    await act(async () => {
      await vi.waitFor(() => expect(dialog().textContent).toContain('follow-up transport offline'))
    })
    expect(dialog()).not.toBeNull()
    expect(textarea.value).toBe('Please keep this draft')
  })

  it('does not let an old Session follow-up close or report into a newly opened Team panel', async () => {
    let settleOld!: (error?: Error) => void
    const oldAction = new Promise<void>((resolve, reject) => {
      settleOld = error => error ? reject(error) : resolve()
    })
    vi.mocked(agentTeamApi.snapshot).mockImplementation(async sessionId => teamSnapshot([
      sessionId === 'session-a'
        ? { id: 'agent-a', label: 'Agent A' }
        : { id: 'agent-b', label: 'Agent B' },
    ]))
    const eventsA = [subagentEvent(1, { subagent_id: 'agent-a', status: 'idle', label: 'Agent A' })]
    const eventsB = [subagentEvent(2, { subagent_id: 'agent-b', status: 'idle', label: 'Agent B' })]
    const followupA = vi.fn(() => oldAction)

    render(surface(eventsA, <OpenTeam id="agent-a" />, { sessionId: 'session-a', onFollowup: followupA }))
    await act(async () => { button(host, 'open fixture team').click() })
    await waitForMember('agent-a')
    const memberA = dialog().querySelector<HTMLElement>('[data-agent-team-member="agent-a"]')!
    act(() => changeTextarea(memberA.querySelector('textarea')!, 'Old Session work'))
    act(() => button(memberA, '发送').click())
    expect(followupA).toHaveBeenCalled()

    await act(async () => {
      render(surface(eventsB, <OpenTeam id="agent-b" />, { sessionId: 'session-b' }))
      await Promise.resolve()
    })
    await act(async () => { button(host, 'open fixture team').click() })
    await waitForMember('agent-b')

    await act(async () => {
      settleOld(new Error('stale Session failure'))
      await oldAction.catch(() => undefined)
      await Promise.resolve()
    })
    const currentPanel = dialog()
    expect(currentPanel.querySelector('[data-agent-team-member="agent-b"]')).not.toBeNull()
    expect(currentPanel.textContent).not.toContain('stale Session failure')
  })

  it('opens a targeted member from both Chat and lineage without inventing a transcript', async () => {
    const events = [subagentEvent(1, {
      subagent_id: 'agent-memory', status: 'idle', label: 'Memory worker', output: 'done',
    })]
    const owner = session('owner')
    vi.mocked(agentTeamApi.snapshot).mockResolvedValue(teamSnapshot([
      { id: 'agent-memory', label: 'Memory worker' },
    ]))
    render(surface(events, <>
      <SubagentEventRow event={events[0]!} sessions={[owner]} onOpenSession={vi.fn()} onSelect={vi.fn()} />
      <SessionLineage events={events} session={owner} sessions={[owner]} onOpenSession={vi.fn()} />
    </>))

    await act(async () => {
      button(host, '在 Agent Team 中打开“Memory worker”').click()
    })
    await waitForMember('agent-memory')
    let panel = dialog()
    expect(panel.querySelector('[data-agent-team-member="agent-memory"][data-selected]')).not.toBeNull()
    expect(panel.textContent).toContain('没有独立会话记录')
    act(() => button(panel, '关闭').click())

    act(() => button(host, '1 个子 Agent').click())
    const lineage = host.querySelector<HTMLElement>('[data-session-lineage]')!
    await act(async () => {
      button(lineage, '在 Agent Team 中打开“Memory worker”').click()
    })
    await waitForMember('agent-memory')
    panel = dialog()
    expect(panel.querySelector('[data-agent-team-member="agent-memory"][data-selected]')).not.toBeNull()
  })

  it('keeps a bound child readable in the shared parent without claiming its transcript is absent or requesting access', async () => {
    const events = [subagentEvent(1, {
      subagent_id: 'agent-private', session_id: 'private-child', transcript_kind: 'conversation',
      status: 'idle', label: 'Private child', task: 'Inspect the build', output: 'Build summary',
    })]
    const owner = session('owner')
    const open = vi.fn()
    vi.mocked(agentTeamApi.snapshot).mockResolvedValue(teamSnapshot([
      { id: 'agent-private', label: 'Private child' },
    ]))
    render(surface(events, <>
      <SubagentEventRow event={events[0]!} sessions={[owner]} onOpenSession={open} onSelect={vi.fn()} />
      <SessionLineage events={events} session={owner} sessions={[owner]} onOpenSession={open} />
    </>, { sessions: [owner], onOpenSession: open, readOnly: true }))

    const notice = '暂时无法从这里打开子会话；任务摘要和结果仍可查看。'
    expect(host.querySelector('[data-subagent-event]')?.textContent).toContain(notice)
    act(() => button(host, '1 个子 Agent').click())
    expect(host.querySelector('[data-session-lineage]')?.textContent).toContain(notice)
    expect(host.textContent).not.toContain('没有独立会话记录')
    const lineage = host.querySelector<HTMLElement>('[data-session-lineage]')!
    await act(async () => button(lineage, '在 Agent Team 中打开“Private child”').click())
    await waitForMember('agent-private')
    const member = dialog().querySelector<HTMLElement>('[data-agent-team-member="agent-private"]')!
    expect(member.textContent).toContain(notice)
    expect(member.textContent).not.toContain('没有独立会话记录')
    expect(member.textContent).toContain('Inspect the build')
    expect(member.querySelector('[data-agent-team-output]')?.textContent).toContain('Build summary')
    expect([...member.querySelectorAll('button')].some(item => item.textContent?.includes('打开独立会话'))).toBe(false)
    expect(member.querySelector('[data-agent-team-followup]')).toBeNull()
    expect(agentTeamApi.events).not.toHaveBeenCalled()
    expect(open).not.toHaveBeenCalled()
  })

  it('navigates to an explicitly persisted grandchild without requiring a root live event', async () => {
    const owner = session('owner')
    const child = {
      ...session('child-session'), parent_session_id: 'owner',
      subagent: { subagent_id: 'child-agent', provider: 'in-process', transcript_kind: 'conversation' as const },
    }
    const grandchild = {
      ...session('grandchild-private-session'), parent_session_id: 'child-session',
      subagent: { subagent_id: 'grandchild-agent', provider: 'in-process', transcript_kind: 'conversation' as const },
    }
    const snapshot = teamSnapshot([
      { id: 'child-agent', label: 'Child' },
      { id: 'grandchild-agent', label: 'Grandchild' },
    ])
    snapshot.members[2]!.parent_id = snapshot.members[1]!.id
    vi.mocked(agentTeamApi.snapshot).mockResolvedValue(snapshot)
    const openSession = vi.fn()
    render(surface([], <OpenTeam />, { sessions: [owner, child, grandchild], onOpenSession: openSession }))

    await act(async () => {
      button(host, 'open fixture team').click()
      await Promise.resolve()
    })
    await waitForMember('grandchild-agent')
    const grandchildCard = dialog().querySelector<HTMLElement>('[data-agent-team-member="grandchild-agent"]')!
    expect(grandchildCard.textContent).not.toContain('grandchild-private-session')
    act(() => button(grandchildCard, '打开独立会话').click())
    expect(openSession).toHaveBeenCalledWith('grandchild-private-session')
  })

  it('uses the shared task board and mailbox with CAS conflict recovery', async () => {
    const snapshot: AgentTeamSnapshot = {
      ...teamSnapshot([{ id: 'worker-a', label: 'Worker A' }]),
      tasks: [{
        id: 'task-base', subject: 'Base task', description: 'Initial work', status: 'pending',
        dependencies: [], owner: 'member-1', revision: 2, created_at_ms: 10, updated_at_ms: 10,
      }],
      messages: [{
        id: 'message-1', from: 'member-1', to: 'member-lead', content: 'Please review', created_at_ms: 20,
      }],
    }
    vi.mocked(agentTeamApi.snapshot).mockResolvedValue(snapshot)
    render(surface([], <OpenTeam />))
    await act(async () => {
      button(host, 'open fixture team').click()
      await Promise.resolve()
    })
    await waitForMember('worker-a')
    let panel = dialog()

    act(() => panel.querySelector<HTMLButtonElement>('#agent-team-tasks-tab')!.click())
    act(() => button(panel, '新建任务').click())
    const createForm = panel.querySelector<HTMLFormElement>('[data-agent-team-task-form="create"]')!
    act(() => changeInput(createForm.querySelector('input')!, 'Follow-up task'))
    await act(async () => { button(createForm, '保存').click(); await Promise.resolve() })
    expect(agentTeamApi.createTask).toHaveBeenCalledWith('owner', expect.objectContaining({
      subject: 'Follow-up task', status: 'pending', dependencies: [], owner: null,
    }))

    act(() => button(panel, '编辑任务“Base task”').click())
    const editForm = panel.querySelector<HTMLFormElement>('[data-agent-team-task-form="edit"]')!
    act(() => changeInput(editForm.querySelector('select')!, 'completed'))
    await act(async () => { button(editForm, '保存').click(); await Promise.resolve() })
    expect(agentTeamApi.replaceTask).toHaveBeenCalledWith('owner', snapshot.tasks[0], expect.objectContaining({
      subject: 'Base task', status: 'completed', owner: 'member-1',
    }))

    vi.mocked(agentTeamApi.deleteTask).mockRejectedValueOnce(new ApiError('conflict', 409, 'conflict'))
    act(() => button(panel, '删除任务“Base task”').click())
    await act(async () => { button(panel, '删除').click(); await Promise.resolve(); await Promise.resolve() })
    await act(async () => {
      await vi.waitFor(() => expect(panel.textContent).toContain('已刷新最新 Team 状态'))
    })
    expect(agentTeamApi.deleteTask).toHaveBeenCalledWith('owner', snapshot.tasks[0])
    expect(agentTeamApi.snapshot).toHaveBeenCalledTimes(4)

    act(() => panel.querySelector<HTMLButtonElement>('#agent-team-mailbox-tab')!.click())
    const mailbox = panel.querySelector<HTMLElement>('[data-agent-team-mailbox]')!
    act(() => changeTextarea(mailbox.querySelector('textarea')!, 'Status update'))
    await act(async () => { button(mailbox, '发送').click(); await Promise.resolve() })
    expect(agentTeamApi.sendMessage).toHaveBeenCalledWith('owner', 'member-1', 'Status update')

    await act(async () => { button(mailbox, '标为已读').click(); await Promise.resolve() })
    expect(agentTeamApi.markMessageRead).toHaveBeenCalledWith('owner', 'message-1')

    vi.mocked(agentTeamApi.sendMessage).mockRejectedValueOnce(new Error('mailbox transport offline'))
    act(() => changeTextarea(mailbox.querySelector('textarea')!, 'Retry delivery'))
    await act(async () => { button(mailbox, '发送').click(); await Promise.resolve() })
    expect(panel.textContent).toContain('mailbox transport offline')
  })
})
