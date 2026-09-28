import { Globe2 } from 'lucide-react'
import { WebFetchToolResult, WebSearchToolResult } from '@/components/workbench/tool-result-views'
import { toolRecord, toolString, type WebSource } from '@/domain/tool-presentation'
import {
  argumentsOf, defineToolPresentation, exactTitle, outputJson,
  type BuiltinView,
} from './shared'

export const webSearchTool = defineToolPresentation<BuiltinView<'web-search'>>({
  id: 'builtin.web-search', priority: 100,
  matches: trace => trace.name === 'web_search',
  parse: trace => {
    if (!trace.output || trace.output.is_error) return null
    const decoded = outputJson(trace)
    if (!Array.isArray(decoded)) return null
    const sources: WebSource[] = decoded.flatMap(value => {
      const item = toolRecord(value)
      if (!item || typeof item.url !== 'string') return []
      return [{
        title: toolString(item.title) || item.url,
        url: item.url,
        snippet: toolString(item.snippet),
        engine: typeof item.engine === 'string' ? item.engine : undefined,
      }]
    })
    return { kind: 'web-search', query: toolString(argumentsOf(trace).query), sources }
  },
  icon: () => Globe2,
  title: exactTitle,
  target: trace => toolString(argumentsOf(trace).query),
  render: (view, { t }) => <WebSearchToolResult view={view} t={t} />,
})

export const webFetchTool = defineToolPresentation<BuiltinView<'web-fetch'>>({
  id: 'builtin.web-fetch', priority: 100,
  matches: trace => trace.name === 'web_fetch',
  parse: trace => {
    if (!trace.output || trace.output.is_error) return null
    const match = /^HTTP (\d+)\nContent-Type: ([^\n]+)\n\n([\s\S]*)$/.exec(trace.output.content)
    return match ? {
      kind: 'web-fetch',
      url: toolString(argumentsOf(trace).url),
      statusCode: Number(match[1]),
      contentType: match[2]!,
      body: match[3]!,
    } : null
  },
  icon: () => Globe2,
  title: exactTitle,
  target: trace => toolString(argumentsOf(trace).url),
  render: (view, { t }) => <WebFetchToolResult view={view} t={t} />,
})

export const webToolPresentations = { webSearchTool, webFetchTool }
