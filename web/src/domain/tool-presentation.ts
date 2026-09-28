import type { ToolPresentationField } from '@/types'
import type { ToolTrace } from './events'

export interface SearchEntry {
  path: string
  line?: number
  column?: number
  preview?: string
}

export interface WebSource {
  title: string
  url: string
  snippet: string
  engine?: string
}

export type BuiltinToolPresentation =
  | { kind: 'read'; path: string; content: string; startLine: number; endLine?: number; totalLines?: number }
  | { kind: 'diff'; path: string; before: string; after: string; operation: 'write' | 'replace' }
  | { kind: 'search'; query: string; entries: SearchEntry[]; mode: 'search' | 'glob'; warnings: string[]; truncated: boolean }
  | { kind: 'terminal'; command: string; stdout: string; stderr: string; exitCode?: number; timedOut: boolean }
  | { kind: 'web-search'; query: string; sources: WebSource[] }
  | { kind: 'web-fetch'; url: string; statusCode: number; contentType: string; body: string }
  | { kind: 'skill'; name: string; content: string; error: boolean }
  | { kind: 'generic'; content: string; error: boolean }

export function toolRecord(value: unknown): Record<string, unknown> | null {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
    ? value as Record<string, unknown>
    : null
}

export function toolJson(value: string): unknown {
  try { return JSON.parse(value) } catch { return null }
}

export function toolString(value: unknown) {
  return typeof value === 'string' ? value : ''
}

export function toolNumber(value: unknown) {
  return typeof value === 'number' && Number.isFinite(value) ? value : undefined
}

export function presentationValue(value: unknown, path: readonly string[]) {
  let current = value
  for (const segment of path) {
    if (typeof current !== 'object' || current === null || Array.isArray(current)) return undefined
    current = (current as Record<string, unknown>)[segment]
  }
  return current
}

export function presentationDisplayValue(value: unknown) {
  if (value == null) return ''
  if (typeof value === 'string') return value
  if (typeof value === 'number' || typeof value === 'boolean') return String(value)
  return JSON.stringify(value)
}

export function presentationFieldSummary(value: unknown, fields: readonly ToolPresentationField[]) {
  return fields.map(field => {
    const display = presentationDisplayValue(presentationValue(value, field.path))
    return display ? `${field.label}: ${display}` : ''
  }).filter(Boolean).join(' · ')
}

export function declarativeInputSummary(trace: ToolTrace) {
  return trace.presentation ? presentationFieldSummary(trace.arguments, trace.presentation.input_summary ?? []) : ''
}

export function safeWebUrl(value: string) {
  try {
    const url = new URL(value)
    return url.protocol === 'http:' || url.protocol === 'https:' ? value : undefined
  } catch {
    return undefined
  }
}
