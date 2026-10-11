import { describe, expect, it } from 'vitest'
import type { AgentTeamSnapshot, LocalSession, SessionEvent } from '@/types'
import { emptyPrice, estimateUsage, mergeUsage, modelUsage, teamSessionIds } from './team-usage'

const session = (id: string, parent?: string, child?: string) => ({ identity: { session_id: id }, parent_session_id: parent, subagent: child ? { subagent_id: child, transcript_kind: 'conversation' } : null }) as LocalSession
const snapshot: AgentTeamSnapshot = { team_id: 'a', current_member_id: 'child', members: [
  { id: 'lead', role: 'lead', label: 'Lead' },
  { id: 'child', role: 'subagent', label: 'Child', parent_id: 'lead', subagent_id: 'same-name' },
  { id: 'sibling', role: 'subagent', label: 'Sibling', parent_id: 'lead', subagent_id: 'sibling-name' },
  { id: 'unavailable', role: 'subagent', label: 'Missing', parent_id: 'lead', subagent_id: 'hidden-name' },
], tasks: [], messages: [] }
const response = (seq: number, model = 'm', usage: NonNullable<SessionEvent['response']>['usage'] = { input_tokens: 100, output_tokens: 20, cached_input_tokens: 40, cache_write_tokens: 10, reasoning_tokens: 8 }): SessionEvent => ({ seq, type: 'assistant_message', occurred_at_ms: 1, run_id: 'r', response: { provider: 'p', model, content: 'done', finish_reason: 'stop', usage } })

describe('team usage', () => {
  it('resolves from a child through its parent without mixing identically named children from another team', () => {
    const ids = teamSessionIds(snapshot, 'child-a', [session('child-a', 'lead-a', 'same-name'), session('child-b', 'lead-b', 'same-name'), session('sibling-a', 'lead-a', 'sibling-name'), session('sibling-b', 'lead-b', 'sibling-name')])
    expect(Object.fromEntries(ids)).toEqual({ child: 'child-a', lead: 'lead-a', sibling: 'sibling-a' })
    const ambiguous = teamSessionIds(snapshot, 'child-a', [session('child-a', 'lead-a', 'same-name'), session('s1', 'lead-a', 'sibling-name'), session('s2', 'lead-a', 'sibling-name')])
    expect(ambiguous.has('sibling')).toBe(false)
  })
  it('counts actual historical models once, preserves unknown measurements and excludes replays', () => {
    const replay = response(4); replay.response!.replayed = true
    const groups = modelUsage([response(1), response(1), response(2, 'other'), response(3, 'other', null), replay, { ...response(5), type: 'assistant_message_delta' }])
    expect(groups).toHaveLength(2)
    expect(groups[0]).toMatchObject({ input: 100, output: 20, cached: 40, cacheWrite: 10, reasoning: 8, responses: 1, replayed: 1 })
    expect(groups[1]).toMatchObject({ responses: 2, unmeasured: 1 })
    const price = { input: '2', output: '10', cached: '1', cacheWrite: '3' }
    expect(estimateUsage(groups[0]!, price)).toBeCloseTo(0.00037)
    expect(estimateUsage(groups[1]!, price)).toBeNull()
    expect(estimateUsage(groups[0]!, emptyPrice)).toBeNull()
    expect(estimateUsage(groups[0]!, { ...price, cached: '' })).toBeNull()
    expect(estimateUsage(groups[0]!, { ...price, input: '-1' })).toBeNull()
    expect(estimateUsage(groups[0]!, { input: '0', output: '0', cached: '0', cacheWrite: '0' })).toBe(0)
    expect(mergeUsage([...groups, ...groups])[0]).toMatchObject({ input: 200, output: 40 })
  })
})
