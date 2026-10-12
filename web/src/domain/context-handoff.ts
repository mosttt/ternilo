export const handoffFields = ['constraints', 'decisions', 'completed', 'pending', 'files', 'verification', 'risks'] as const
export type HandoffField = typeof handoffFields[number]
export type ContextHandoff = { objective: string } & Record<HandoffField, string[]>

export function parseContextHandoff(summary: string): ContextHandoff | null {
  try {
    const value: unknown = JSON.parse(summary)
    if (!value || typeof value !== 'object' || Array.isArray(value)) return null
    const record = value as Record<string, unknown>
    if (typeof record.objective !== 'string' || !record.objective.trim()) return null
    if (Object.keys(record).some(key => key !== 'objective' && !handoffFields.includes(key as HandoffField))) return null
    const lists = {} as Record<HandoffField, string[]>
    for (const key of handoffFields) {
      const items = record[key] ?? []
      if (!Array.isArray(items) || items.some(item => typeof item !== 'string')) return null
      lists[key] = items
    }
    return { objective: record.objective, ...lists }
  } catch { return null }
}
