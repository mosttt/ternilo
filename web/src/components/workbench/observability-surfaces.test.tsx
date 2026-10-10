import * as React from 'react'
import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { LocaleProvider, useLocale } from '@/i18n/provider'
import type { LocalSession, SessionEvent } from '@/types'
import type { WorkflowRunView } from '@/domain/observability'
import { JobListAction } from './job-list-action'
import { SessionLineage } from './session-lineage'
import { WorkflowRunPanel } from './chat/workflow-run-panel'
import { SubagentEventRow } from './chat/observability-event-rows'

let host: HTMLDivElement
let root: Root
const savedValues = new Map<string, string>()

function event(seq: number, type: string, values: Record<string, unknown> = {}): SessionEvent {
  return { seq, type, run_id: 'run-1', occurred_at_ms: seq * 1_000, ...values }
}

function session(id: string, parent?: string, subagentId?: string): LocalSession {
  return {
    identity: { tenant_id: 'local', user_id: 'user', agent_id: 'agent', session_id: id },
    workspace_id: 'workspace', workspace_path: '/tmp/workspace', parent_session_id: parent,
    title: id, permissions: 'workspace_write', model: { provider: 'profile_default' },
    agent_preset: 'standard', preset_plugins: [], profile_plugins: [], mode: 'execute',
    created_at_ms: 1, updated_at_ms: 1,
    ...(subagentId ? { subagent: { subagent_id: subagentId, provider: 'in-process', transcript_kind: 'conversation' } as const } : {}),
  }
}

beforeEach(() => {
  vi.stubGlobal('localStorage', {
    getItem: (key: string) => savedValues.get(key) ?? null,
    setItem: (key: string, value: string) => savedValues.set(key, value),
  })
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true
  host = document.createElement('div')
  document.body.append(host)
  root = createRoot(host)
})

afterEach(() => {
  act(() => root.unmount())
  host.remove()
  savedValues.clear()
  vi.unstubAllGlobals()
  vi.restoreAllMocks()
  delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
})

function render(node: React.ReactNode) {
  act(() => root.render(<LocaleProvider>{node}</LocaleProvider>))
}

function LocaleFixture({ locale, children }: { locale: 'zh' | 'en'; children: React.ReactNode }) {
  const { locale: current, setLocale } = useLocale()
  React.useEffect(() => { if (current !== locale) setLocale(locale) }, [current, locale, setLocale])
  return <>{children}</>
}

describe('Workbench observability surfaces', () => {
  it('hides ordinary sessions and lists jobs proven by canonical lifecycle events', () => {
    render(<JobListAction events={[]} />)
    expect(host.querySelector('[data-job-list-action]')).toBeNull()

    render(<JobListAction events={[event(1, 'job_updated', { job: {
      job_id: 'job-1', command: 'cargo test', status: 'running', result: null, error: null,
    } })]} />)
    const trigger = host.querySelector<HTMLButtonElement>('[data-job-list-action] > button')!
    expect(trigger.getAttribute('aria-label')).toBe('1 个后台任务')
    act(() => trigger.click())
    expect(host.querySelector('[data-job-status="running"]')?.textContent).toContain('cargo test')
  })

  it('makes only genuinely addressable subagent rows navigable and explains in-process rows', () => {
    const open = vi.fn()
    const events = [event(1, 'subagent_updated', { subagent: {
      subagent_id: 'agent-in-process', provider: 'in-process', label: 'Research', task: 'inspect files',
      status: 'running', output: null, error: null, created_at_ms: 1, updated_at_ms: 1,
    } })]
    render(<>
      <SubagentEventRow event={events[0]!} sessions={[]} onOpenSession={open} onSelect={vi.fn()} />
      <SessionLineage events={events} session={session('parent')} sessions={[session('parent'), session('child', 'parent')]} onOpenSession={open} />
    </>)
    const card = host.querySelector('[data-subagent-event]')!
    expect(card.textContent).toContain('没有发布独立会话记录')
    expect(card.querySelector('[aria-label="打开子 Agent 会话“Research”"]')).toBeNull()
    act(() => host.querySelector<HTMLButtonElement>('[data-session-lineage] > button')!.click())
    expect(host.querySelector('[role="treeitem"][aria-disabled="true"]')?.textContent).toContain('Research')
    expect(host.textContent).toContain('没有独立会话记录')
    const child = [...host.querySelectorAll<HTMLButtonElement>('[role="treeitem"]')].find(item => item.textContent?.includes('child'))!
    act(() => child.click())
    expect(open).toHaveBeenCalledWith('child')
  })

  it.each([
    ['zh', '暂时无法从这里打开子会话；任务摘要和结果仍可查看。', '没有独立会话记录', '打开子 Agent 会话“Research”'],
    ['en', 'This child session cannot be opened here right now. Task summaries and results remain available.', 'no independent session record', 'Open subagent session “Research”'],
  ])('keeps a published child summary readable until its matching session becomes available (%s)', (locale, notice, absent, openLabel) => {
    const open = vi.fn()
    const owner = session('parent')
    const childEvent = event(1, 'subagent_updated', { subagent: {
      subagent_id: 'agent-research', session_id: 'canonical-child', transcript_kind: 'conversation',
      provider: 'in-process', label: 'Research', task: 'inspect files', status: 'failed',
      output: 'Preserved findings', error: 'The last check failed', created_at_ms: 1, updated_at_ms: 1,
    } })
    const display = (sessions: LocalSession[]) => act(() => root.render(<LocaleProvider><LocaleFixture locale={locale as 'zh' | 'en'}>
      <SubagentEventRow event={childEvent} sessions={sessions} onOpenSession={open} onSelect={vi.fn()} />
      <SessionLineage events={[childEvent]} session={owner} sessions={sessions} onOpenSession={open} />
    </LocaleFixture></LocaleProvider>))
    display([owner])
    act(() => host.querySelector<HTMLButtonElement>('[data-session-lineage] > button')!.click())
    const card = host.querySelector('[data-subagent-event]')!
    expect(card.textContent).toContain(notice)
    expect(card.textContent).toContain('inspect files')
    expect(card.querySelector('details pre')?.textContent).toBe('Preserved findings')
    expect(card.querySelector('details[open] pre')?.textContent).toBe('The last check failed')
    expect(host.querySelector('[data-session-lineage]')?.textContent).toContain(notice)
    expect(host.textContent).not.toContain(absent)
    expect(host.textContent).not.toMatch(/403|permission denied|权限不足/i)
    expect(card.querySelector(`button[aria-label='${openLabel}']`)).toBeNull()

    display([owner, session('canonical-child', 'parent', 'different-agent')])
    expect(card.textContent).toContain(notice)
    expect(card.querySelector(`button[aria-label='${openLabel}']`)).toBeNull()
    expect(open).not.toHaveBeenCalled()

    display([owner, session('canonical-child', 'parent', 'agent-research')])
    expect(host.textContent).not.toContain(notice)
    const action = card.querySelector<HTMLButtonElement>(`button[aria-label='${openLabel}']`)!
    expect(action).not.toBeNull()
    act(() => action.click())
    expect(open).toHaveBeenCalledExactlyOnceWith('canonical-child')
  })

  it('uses explicit child Session ids and renders recursive descendants and ancestors', () => {
    const open = vi.fn()
    const rootSession = session('root')
    const child = session('canonical-child', 'root', 'agent-runtime-id')
    const grandchild = session('nested-child', 'canonical-child', 'agent-nested')
    const events = [event(1, 'subagent_updated', { subagent: {
      subagent_id: 'agent-runtime-id', session_id: 'canonical-child', transcript_kind: 'conversation',
      provider: 'in-process', label: 'Research', task: 'inspect files', status: 'idle',
      created_at_ms: 1, updated_at_ms: 1,
    } })]
    render(<SessionLineage events={events} session={rootSession} sessions={[rootSession, child, grandchild]} onOpenSession={open} />)
    act(() => host.querySelector<HTMLButtonElement>('[data-session-lineage] > button')!.click())
    const childAction = host.querySelector<HTMLButtonElement>('[aria-label="打开子 Agent 会话“Research”"]')!
    expect(childAction).not.toBeNull()
    expect(host.querySelector('[data-lineage-session="nested-child"]')).not.toBeNull()
    act(() => host.querySelector<HTMLButtonElement>('[data-lineage-session="nested-child"]')!.click())
    expect(open).toHaveBeenCalledWith('nested-child')

    render(<SessionLineage events={[]} session={grandchild} sessions={[rootSession, child, grandchild]} onOpenSession={open} />)
    act(() => host.querySelector<HTMLButtonElement>('[data-session-lineage] > button')!.click())
    expect(host.querySelector('[data-lineage-ancestor="root"]')).not.toBeNull()
    expect(host.querySelector('[data-lineage-ancestor="canonical-child"]')).not.toBeNull()
  })

  it('renders workflow phases and enables navigation only for a matching member session', () => {
    const open = vi.fn()
    const run: WorkflowRunView = {
      id: 'wf', anchor: event(0, 'workflow_run_started'), name: 'Review', description: 'Check code',
      status: 'running', currentPhase: 'Inspect', logs: ['reading'], startedAt: 0,
      phases: [{ key: 'Inspect', title: 'Inspect', members: [
        { sequence: 1, label: 'Reviewer', phase: 'Inspect', subagentId: 'agent-session', status: 'running' },
        { sequence: 2, label: 'In process', phase: 'Inspect', subagentId: 'agent-memory', status: 'completed' },
      ] }],
    }
    render(<WorkflowRunPanel run={run} sessions={[session('agent-session')]} onOpenSession={open} />)
    expect(host.querySelector('[data-workflow-run="wf"]')?.textContent).toContain('当前阶段：Inspect')
    const member = host.querySelector<HTMLButtonElement>('[aria-label="打开成员会话“Reviewer”"]')!
    act(() => member.click())
    expect(open).toHaveBeenCalledWith('agent-session')
    expect(host.querySelector('[aria-label*="In process"]')?.textContent).toContain('In process')
  })
})
