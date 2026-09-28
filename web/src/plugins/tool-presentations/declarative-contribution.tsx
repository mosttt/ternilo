import {
  Braces, Code2, Database, FileText, Globe2, Puzzle, Search, Sparkles, TerminalSquare, Wrench,
  type LucideIcon,
} from 'lucide-react'
import { AssistantMarkdown } from '@/components/workbench/chat/assistant-markdown'
import type { ToolTrace } from '@/domain/events'
import {
  declarativeInputSummary, presentationDisplayValue, presentationValue, toolJson,
} from '@/domain/tool-presentation'
import type { Translate } from '@/i18n/runtime'
import type { ToolPresentationDescriptor, ToolPresentationField } from '@/types'
import { cn } from '@/lib/utils'
import css from '@/components/workbench/tool-call-tree.module.css'
import { defineToolPresentation } from './shared'

const icons: Record<ToolPresentationDescriptor['icon_kind'], LucideIcon> = {
  wrench: Wrench,
  puzzle: Puzzle,
  sparkles: Sparkles,
  file: FileText,
  terminal: TerminalSquare,
  search: Search,
  globe: Globe2,
  database: Database,
  code: Code2,
  unknown: Braces,
}

export function declarativeToolIcon(descriptor?: ToolPresentationDescriptor | null) {
  return icons[descriptor?.icon_kind ?? 'unknown'] ?? Wrench
}

interface DeclarativeView {
  descriptor: ToolPresentationDescriptor
  content: string
  error: boolean
}

function JsonResult({ content, label }: { content: string; label: string }) {
  const value = toolJson(content)
  return <pre className={css.declarativeCode} data-tool-scroll="" tabIndex={0} role="region" aria-label={label}>{value == null ? content : JSON.stringify(value, null, 2)}</pre>
}

function TableResult({ content, columns, label }: { content: string; columns: readonly ToolPresentationField[]; label: string }) {
  const parsed = toolJson(content)
  if (!Array.isArray(parsed)) return <pre className={css.declarativeCode} data-tool-scroll="" tabIndex={0} role="region" aria-label={label}>{content}</pre>
  return <div className={css.declarativeTableScroll} data-tool-scroll="" tabIndex={0} role="region" aria-label={label}>
    <table className={css.declarativeTable}>
      <thead><tr>{columns.map((column, index) => <th key={`${column.label}-${index}`}>{column.label}</th>)}</tr></thead>
      <tbody>{parsed.map((row, rowIndex) => <tr key={rowIndex}>{columns.map((column, columnIndex) => <td key={`${column.label}-${columnIndex}`}>{presentationDisplayValue(presentationValue(row, column.path)) || '—'}</td>)}</tr>)}</tbody>
    </table>
  </div>
}

function DeclarativeResult({ view, t }: { view: DeclarativeView; t: Translate<'chat'> }) {
  const kind = view.descriptor.result.kind
  const outputLabel = t('details.output')
  return <section className={cn(css.resultCard, css.declarativeResult)} data-error={view.error || undefined} data-tool-view={`declarative-${kind}`}>
    {kind === 'markdown'
      ? <div className={css.declarativeMarkdown} data-tool-scroll="" tabIndex={0} role="region" aria-label={outputLabel}><AssistantMarkdown source={view.content} streaming={false} t={t} /></div>
      : kind === 'json'
        ? <JsonResult content={view.content} label={outputLabel} />
        : kind === 'table'
          ? <TableResult content={view.content} columns={view.descriptor.result.columns ?? []} label={outputLabel} />
          : <pre className={css.declarativeCode} data-tool-scroll="" tabIndex={0} role="region" aria-label={outputLabel}>{view.content || t('tool.emptyResult')}</pre>}
  </section>
}

export const declarativeTool = defineToolPresentation<DeclarativeView>({
  id: 'builtin.declarative',
  priority: 1_000,
  matches: trace => Boolean(trace.presentation),
  parse: trace => trace.presentation && trace.output ? {
    descriptor: trace.presentation,
    content: trace.output.content,
    error: trace.output.is_error,
  } : null,
  icon: trace => declarativeToolIcon(trace.presentation),
  title: (trace, t) => trace.presentation?.title || trace.name || t('tool.call'),
  target: declarativeInputSummary,
  kind: () => 'declarative',
  render: (view, { t }) => <DeclarativeResult view={view} t={t} />,
})
