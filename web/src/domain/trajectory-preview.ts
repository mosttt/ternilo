import type { ToolTrace } from './events'

const PREVIEW_SOURCE_CHARACTERS = 2_048
const PREVIEW_OUTPUT_CHARACTERS = 512

/** Builds a bounded one-line preview while the original payload remains in Details. */
export function trajectoryPreviewText(text: string): string {
  const source = text.slice(0, PREVIEW_SOURCE_CHARACTERS)
  const compact = source
    .replace(/```[^\n]*\n?/g, '')
    .replace(/!\[([^\]]*)\]\([^)]*\)/g, '$1')
    .replace(/\[([^\]]+)\]\([^)]*\)/g, '$1')
    .replace(/<[^>]+>/g, ' ')
    .replace(/^\s{0,3}(?:#{1,6}|>|[-+*]|\d+[.)])\s+/gm, '')
    .replace(/[*_~`]+/g, '')
    .replace(/\s+/g, ' ')
    .trim()
  const preview = compact.slice(0, PREVIEW_OUTPUT_CHARACTERS).trimEnd()
  return source.length < text.length || preview.length < compact.length ? `${preview}…` : preview
}

function stringValue(value: unknown) {
  if (typeof value === 'string') return value
  if (Array.isArray(value)) return value.map(item => String(item)).join(' ')
  if (value == null) return ''
  return JSON.stringify(value)
}

export type ToolPreviewLabel = 'command' | 'file' | 'search' | 'scope' | 'web' | 'code'

export interface ToolTracePreviewParts {
  input: Array<{ label: ToolPreviewLabel; value: string }>
  output: string
  error: boolean
}

function inputPreview(trace: ToolTrace): ToolTracePreviewParts['input'] {
  const args = trace.arguments && typeof trace.arguments === 'object' && !Array.isArray(trace.arguments)
    ? trace.arguments as Record<string, unknown>
    : {}
  const name = trace.name.toLocaleLowerCase()
  const choose = (label: ToolPreviewLabel, ...keys: string[]) => {
    const value = keys.map(key => args[key]).find(candidate => candidate != null)
    const text = stringValue(value)
    return text ? [{ label, value: trajectoryPreviewText(text) }] : []
  }
  if (/terminal|shell|bash|command|exec/.test(name)) return choose('command', 'command', 'cmd', 'script')
  if (/read|write|edit|patch|file|glob/.test(name)) return choose('file', 'path', 'file_path', 'filename', 'glob')
  if (/search|grep|find/.test(name)) {
    return [...choose('search', 'pattern', 'query', 'search_term'), ...choose('scope', 'path', 'directory', 'include')]
  }
  if (/web|fetch|browser|url/.test(name)) return choose('web', 'url', 'query')
  if (trace.kind === 'code' || /code|dispatch/.test(name)) {
    return choose('code', 'description', 'code', 'script', 'command')
  }
  const serialized = stringValue(trace.arguments)
  return serialized ? [{ label: 'code', value: trajectoryPreviewText(serialized) }] : []
}

export function toolTracePreviewParts(trace: ToolTrace): ToolTracePreviewParts {
  const input = inputPreview(trace)
  const output = trajectoryPreviewText(String(trace.output?.content ?? ''))
  return { input, output, error: trace.output?.is_error === true }
}

/** A locale-neutral fallback for search indexes and non-React consumers. */
export function toolTracePreview(trace: ToolTrace) {
  const parts = toolTracePreviewParts(trace)
  const input = parts.input.map(item => `${item.label} ${item.value}`).join(' · ')
  const result = parts.output ? `${parts.error ? 'error' : 'result'} ${parts.output}` : ''
  return [input || trace.name, result].filter(Boolean).join(' → ')
}
