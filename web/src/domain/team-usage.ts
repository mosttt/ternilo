import type { AgentTeamSnapshot, LocalSession, SessionEvent } from '@/types'

/** Resolve through the parent tree: child names alone are not globally unique. */
export function teamSessionIds(snapshot: AgentTeamSnapshot, currentSessionId: string, sessions: readonly LocalSession[]) {
  const result = new Map<string, string>([[snapshot.current_member_id, currentSessionId]])
  const byId = new Map(sessions.map(session => [session.identity.session_id, session]))
  let changed = true
  while (changed) {
    changed = false
    for (const member of snapshot.members) {
      const sessionId = result.get(member.id)
      const parentSessionId = sessionId ? byId.get(sessionId)?.parent_session_id : undefined
      if (member.parent_id && parentSessionId && !result.has(member.parent_id)) {
        result.set(member.parent_id, parentSessionId); changed = true
      }
      if (sessionId || !member.parent_id || !member.subagent_id) continue
      const parentId = result.get(member.parent_id)
      if (!parentId) continue
      const candidates = sessions.filter(session => session.parent_session_id === parentId
        && session.subagent?.subagent_id === member.subagent_id
        && session.subagent?.transcript_kind === 'conversation')
      if (candidates.length === 1) {
        result.set(member.id, candidates[0]!.identity.session_id); changed = true
      }
    }
  }
  return result
}

export interface ModelUsage {
  provider: string
  model: string
  input: number
  output: number
  cached: number
  cacheWrite: number
  reasoning: number
  responses: number
  unmeasured: number
  replayed: number
}
export interface UsagePrice { input: string; output: string; cached: string; cacheWrite: string }
export const emptyPrice: UsagePrice = { input: '', output: '', cached: '', cacheWrite: '' }
export const usageKey = (usage: Pick<ModelUsage, 'provider' | 'model'>) => JSON.stringify([usage.provider, usage.model])

/** Count recorded responses, including earlier regenerated branches, once by sequence. */
export function modelUsage(events: readonly SessionEvent[]): ModelUsage[] {
  const groups = new Map<string, ModelUsage>()
  const seen = new Set<number>()
  for (const event of events) {
    if (event.type !== 'assistant_message' || !event.response || seen.has(event.seq)) continue
    seen.add(event.seq)
    const response = event.response
    const key = usageKey(response)
    const group = groups.get(key) ?? { provider: response.provider, model: response.model, input: 0, output: 0, cached: 0, cacheWrite: 0, reasoning: 0, responses: 0, unmeasured: 0, replayed: 0 }
    groups.set(key, group)
    if (response.replayed) { group.replayed += 1; continue }
    group.responses += 1
    const usage = response.usage
    const numbers = usage && [usage.input_tokens, usage.output_tokens, usage.cached_input_tokens ?? 0, usage.cache_write_tokens ?? 0, usage.reasoning_tokens ?? 0]
    if (!usage || !numbers?.every(value => Number.isFinite(value) && value >= 0)) { group.unmeasured += 1; continue }
    group.input += usage.input_tokens
    group.output += usage.output_tokens
    group.cached += usage.cached_input_tokens ?? 0
    group.cacheWrite += usage.cache_write_tokens ?? 0
    group.reasoning += usage.reasoning_tokens ?? 0
  }
  return [...groups.values()]
}

export function mergeUsage(groups: readonly ModelUsage[]): ModelUsage[] {
  const result = new Map<string, ModelUsage>()
  for (const usage of groups) {
    const key = usageKey(usage), current = result.get(key)
    if (!current) { result.set(key, { ...usage }); continue }
    for (const field of ['input', 'output', 'cached', 'cacheWrite', 'reasoning', 'responses', 'unmeasured', 'replayed'] as const) current[field] += usage[field]
  }
  return [...result.values()]
}

export function estimateUsage(usage: ModelUsage, price: UsagePrice): number | null {
  if (usage.unmeasured || usage.cached + usage.cacheWrite > usage.input) return null
  const amounts = { input: usage.input - usage.cached - usage.cacheWrite, output: usage.output, cached: usage.cached, cacheWrite: usage.cacheWrite }
  let total = 0
  for (const field of Object.keys(amounts) as (keyof UsagePrice)[]) {
    if (!amounts[field]) continue
    const rate = Number(price[field])
    if (!price[field].trim() || !Number.isFinite(rate) || rate < 0) return null
    total += amounts[field] * rate / 1_000_000
  }
  return Number.isFinite(total) ? total : null
}
