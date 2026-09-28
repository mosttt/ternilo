import * as React from 'react'
import {
  Check, ChevronRight, CircleAlert, LoaderCircle, PanelRight,
} from 'lucide-react'
import type { ToolTrace } from '@/domain/events'
import '@/plugins/builtin-tool-presentations'
import { useToolPresentation } from '@/plugins/tool-presentation-registry'
import { useTranslate } from '@/i18n/provider'
import type { Translate } from '@/i18n/runtime'
import { formatDuration } from '@/lib/utils'
import css from './tool-call-tree.module.css'

type ChatTranslate = Translate<'chat'>

function ToolBranch({ trace, selectedCallId, onSelect, t }: {
  trace: ToolTrace
  selectedCallId?: string
  onSelect(trace: ToolTrace): void
  t: ChatTranslate
}) {
  const presentation = useToolPresentation(trace)
  const [open, setOpen] = React.useState(false)
  if (!presentation) throw new Error('tool presentation registry has no matching contribution')
  const Icon = presentation.contribution.icon(trace)
  const target = presentation.contribution.target(trace)
  const title = presentation.contribution.title(trace, t)
  const badge = presentation.contribution.badge?.(trace, t)
  const expandable = presentation.result !== null
  const state = !trace.output
    ? 'running'
    : trace.output.content === 'tool_call_cancelled'
      ? 'cancelled'
      : trace.output.is_error ? 'error' : 'complete'
  const statusLabel = state === 'running'
    ? t('row.running')
    : state === 'cancelled'
      ? t('row.cancelled')
      : state === 'error' ? t('row.failed') : t('event.completed')
  const actionLabel = expandable
    ? t(open ? 'tool.collapseResult' : 'tool.expandResult', { title })
    : t('tool.view', { title })
  return <div className={css.branch} data-tool-call-id={trace.id} data-tool={presentation.contribution.kind?.(trace)} data-tool-contribution={presentation.contribution.id} data-selected={trace.id === selectedCallId || undefined} data-state={state}>
    <div className={css.header} data-error={trace.output?.is_error || undefined}>
      <button
        type="button"
        className={css.toggle}
        data-tool-call-toggle=""
        aria-expanded={expandable ? open : undefined}
        aria-label={t('tool.toggleLabel', { action: actionLabel, status: statusLabel })}
        onClick={() => expandable ? setOpen(value => !value) : onSelect(trace)}
      >
        <Icon className={css.icon} aria-hidden />
        <strong>{title}</strong>
        {badge && <span className={css.codeBadge} data-tool-badge="">{badge}</span>}
        <span className={css.state} aria-hidden>{state === 'running' ? <LoaderCircle className="animate-spin" /> : state === 'error' || state === 'cancelled' ? <CircleAlert /> : <Check />}</span>
        {target && <code className={css.target}>{target}</code>}
        <span className={css.duration}>{state === 'cancelled' ? t('row.cancelled') : trace.output ? formatDuration(trace.durationMs) : t('row.running')}</span>
        {expandable && <ChevronRight className={css.chevron} data-open={open || undefined} aria-hidden />}
      </button>
      <button type="button" className={css.inspect} data-tool-call-inspect="" aria-label={t('tool.inspect', { name: trace.name })} onClick={() => onSelect(trace)}><PanelRight aria-hidden /></button>
    </div>
    {open && presentation.result && <div className={css.result}>{presentation.result.contribution.render(presentation.result.view, { trace, t })}</div>}
    {trace.children.length > 0 && <div className={css.children} data-subcalls="">{trace.children.map(child => <ToolBranch key={child.id} trace={child} selectedCallId={selectedCallId} onSelect={onSelect} t={t} />)}</div>}
  </div>
}

export function ToolCallTree({ trace, selectedCallId, onSelect }: {
  trace: ToolTrace
  selectedCallId?: string
  onSelect(trace: ToolTrace): void
}) {
  const t = useTranslate('chat')
  return <div className={css.root} data-tool-call-tree=""><ToolBranch trace={trace} selectedCallId={selectedCallId} onSelect={onSelect} t={t} /></div>
}
