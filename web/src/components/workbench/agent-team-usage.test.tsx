// @vitest-environment jsdom
import * as React from 'react'
import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import type { AgentTeamSnapshot, SessionEvent } from '@/types'
import { en } from '@/i18n/resources/observability'
import type { Translate } from '@/i18n/runtime'
import { agentTeamApi } from './agent-team-api'
import { AgentTeamUsage } from './agent-team-usage'

let host: HTMLDivElement, root: Root
const t: Translate<'observability'> = (key, parameters) => en[key].replace(/\{(\w+)\}/g, (all, name) => String(parameters?.[name] ?? all))
const snapshot = (id: string): AgentTeamSnapshot => ({ team_id: id, current_member_id: id, members: [{ id, role: 'lead', label: id }], tasks: [], messages: [] })
beforeEach(() => { (globalThis as any).IS_REACT_ACT_ENVIRONMENT = true; host = document.createElement('div'); document.body.append(host); root = createRoot(host) })
afterEach(() => { act(() => root.unmount()); host.remove(); vi.restoreAllMocks(); delete (globalThis as any).IS_REACT_ACT_ENVIRONMENT })
const render = (id: string) => root.render(<AgentTeamUsage snapshot={snapshot(id)} sessionId={id} sessions={[]} events={[]} liveActivity={{}} refreshRevision={0} t={t} />)
it('rejects a stale history after switching teams and marks unavailable usage as unknown', async () => {
  let resolve!: (events: SessionEvent[]) => void
  vi.spyOn(agentTeamApi, 'events').mockImplementationOnce(() => new Promise(yes => { resolve = yes })).mockRejectedValueOnce(new Error('forbidden'))
  await act(async () => render('first'))
  await act(async () => render('second'))
  expect(host.textContent).toContain('Records for 1 members are unavailable')
  expect(host.querySelector('[data-usage-estimate]')?.textContent).toContain('Unknown')
  await act(async () => resolve([{ seq: 1, run_id: 'r', occurred_at_ms: 1, type: 'assistant_message', response: { provider: 'private-other-team', model: 'm', content: '', finish_reason: 'stop', usage: { input_tokens: 100, output_tokens: 5 } } }]))
  expect(host.textContent).not.toContain('private-other-team')
})
