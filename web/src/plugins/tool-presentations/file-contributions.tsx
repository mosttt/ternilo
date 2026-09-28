import { FileDiff, FileSearch, FileText, Search } from 'lucide-react'
import {
  DiffToolResult, ReadToolResult, SearchToolResult,
} from '@/components/workbench/tool-result-views'
import { toolNumber, toolRecord, toolString, type SearchEntry } from '@/domain/tool-presentation'
import {
  argumentsOf, defineToolPresentation, exactTitle, outputJson,
  type BuiltinView,
} from './shared'

export const readTool = defineToolPresentation<BuiltinView<'read'>>({
  id: 'builtin.read-file', priority: 100,
  matches: trace => trace.name === 'read_file',
  parse: trace => {
    if (!trace.output || trace.output.is_error) return null
    const decoded = toolRecord(outputJson(trace))
    if (!decoded || typeof decoded.content !== 'string') return null
    const args = argumentsOf(trace)
    return {
      kind: 'read',
      path: toolString(decoded.path) || toolString(args.path),
      content: decoded.content,
      startLine: toolNumber(decoded.start_line) ?? toolNumber(args.start_line) ?? 1,
      endLine: toolNumber(decoded.end_line),
      totalLines: toolNumber(decoded.total_lines),
    }
  },
  icon: () => FileText,
  title: exactTitle,
  target: trace => toolString(argumentsOf(trace).path),
  render: (view, { t }) => <ReadToolResult view={view} t={t} />,
})

export const writeTool = defineToolPresentation<BuiltinView<'diff'>>({
  id: 'builtin.write-file', priority: 100,
  matches: trace => trace.name === 'write_file',
  parse: trace => {
    if (!trace.output || trace.output.is_error) return null
    const decoded = toolRecord(outputJson(trace))
    const args = argumentsOf(trace)
    return {
      kind: 'diff', operation: 'write',
      path: toolString(decoded?.path) || toolString(args.path),
      before: '', after: toolString(args.content),
    }
  },
  icon: () => FileDiff,
  title: exactTitle,
  target: trace => toolString(argumentsOf(trace).path),
  render: (view, { t }) => <DiffToolResult view={view} t={t} />,
})

export const replaceTool = defineToolPresentation<BuiltinView<'diff'>>({
  id: 'builtin.replace-file', priority: 100,
  matches: trace => trace.name === 'replace_in_file',
  parse: trace => {
    if (!trace.output || trace.output.is_error) return null
    const decoded = toolRecord(outputJson(trace))
    const args = argumentsOf(trace)
    return {
      kind: 'diff', operation: 'replace',
      path: toolString(decoded?.path) || toolString(args.path),
      before: toolString(args.old), after: toolString(args.new),
    }
  },
  icon: () => FileDiff,
  title: exactTitle,
  target: trace => toolString(argumentsOf(trace).path),
  render: (view, { t }) => <DiffToolResult view={view} t={t} />,
})

export const searchTool = defineToolPresentation<BuiltinView<'search'>>({
  id: 'builtin.file-search', priority: 100,
  matches: trace => trace.name === 'search_files' || trace.name === 'glob_files',
  parse: trace => {
    if (!trace.output || trace.output.is_error) return null
    const decoded = outputJson(trace)
    const report = toolRecord(decoded)
    const mode = trace.name === 'glob_files' ? 'glob' : 'search'
    const results = Array.isArray(decoded) ? decoded : report?.[mode === 'glob' ? 'files' : 'matches']
    if (!Array.isArray(results)) return null
    const entries: SearchEntry[] = results.flatMap(value => {
      if (typeof value === 'string') return [{ path: value }]
      const item = toolRecord(value)
      if (!item || typeof item.path !== 'string') return []
      return [{
        path: item.path,
        line: toolNumber(item.line),
        column: toolNumber(item.column),
        preview: typeof item.preview === 'string' ? item.preview : undefined,
      }]
    })
    return {
      kind: 'search',
      query: toolString(argumentsOf(trace).pattern),
      entries,
      mode,
      warnings: Array.isArray(report?.warnings) ? report.warnings.filter((warning): warning is string => typeof warning === 'string') : [],
      truncated: report?.truncated === true,
    }
  },
  icon: trace => trace.name === 'search_files' ? Search : FileSearch,
  title: exactTitle,
  target: trace => toolString(argumentsOf(trace).pattern),
  render: (view, { t }) => <SearchToolResult view={view} t={t} />,
})

export const fileToolPresentations = { readTool, writeTool, replaceTool, searchTool }
