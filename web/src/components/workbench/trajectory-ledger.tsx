import * as React from 'react'
import { useVirtualizer } from '@tanstack/react-virtual'
import { Bot, Box, ChevronDown, ChevronRight, CircleAlert, Clock3, Code2, User } from 'lucide-react'
import { trajectoryRecordTitle, type TrajectoryRecord, type TrajectoryTurn } from '@/domain/trajectory'
import { presentRuntimeError } from '@/domain/runtime-error'
import { toolTracePreviewParts } from '@/domain/trajectory-preview'
import { declarativeInputSummary } from '@/domain/tool-presentation'
import { declarativeToolIcon } from '@/plugins/tool-presentations/declarative-contribution'
import type { Translate } from '@/i18n/runtime'
import type { DetailsSelection } from './details-panel'
import { formatDuration, formatTime } from '@/lib/utils'
import css from './trajectory-view.module.css'

const VIRTUALIZATION_THRESHOLD = 100
const VIRTUAL_OVERSCAN = 12

type LedgerRow =
  | { key: string; kind: 'turn'; turn: TrajectoryTurn }
  | { key: string; kind: 'group'; turn: TrajectoryTurn; group: TrajectoryTurn['groups'][number] }
  | { key: string; kind: 'record'; turn: TrajectoryTurn; record: TrajectoryRecord }

export interface TrajectoryLedgerHandle {
  focusRecord(key: string): void
}

export function TrajectoryLedgerHeader({ t }: { t: Translate<'trajectory'> }) {
  return <div aria-hidden="true" className={css.columnHead} data-trajectory-column-head="">
    <span>#</span><span>{t('column.record')}</span><span></span>
    <span>{t('column.input')}</span><span>{t('column.output')}</span>
    <span>{t('column.think')}</span><span>{t('column.cache')}</span><span>{t('column.time')}</span>
  </div>
}

function recordIcon(record: TrajectoryRecord) {
  if (record.kind === 'user') return User
  if (record.kind === 'assistant') return Bot
  if (record.kind === 'tool') return record.trace?.presentation ? declarativeToolIcon(record.trace.presentation) : Box
  if (record.kind === 'code') return Code2
  if (record.kind === 'error') return CircleAlert
  return Clock3
}

function recordSummary(record: TrajectoryRecord, t: Translate<'trajectory'>, errorT: Translate<'chat'>) {
  if (record.kind === 'error') return presentRuntimeError(record.event.message, errorT).message
  if (record.kind === 'assistant' && !record.summary.trim() && record.toolCallCount != null) {
    return t('record.toolCalls', { count: record.toolCallCount })
  }
  if (record.trace) {
    const preview = toolTracePreviewParts(record.trace)
    const input = declarativeInputSummary(record.trace)
      || preview.input.map(item => `${t(`preview.${item.label}`)} ${item.value}`).join(' · ')
    const result = preview.output ? `${t(preview.error ? 'preview.error' : 'preview.result')} ${preview.output}` : ''
    return [input || record.trace.name, result].filter(Boolean).join(' → ')
  }
  return record.summary || record.event.type
}

function compactTokens(value: number | undefined) {
  if (value == null) return '—'
  if (value < 1_000) return String(value)
  return `${Math.round(value / 100) / 10}K`
}

function recordTag(record: TrajectoryRecord, t: Translate<'trajectory'>) {
  if (record.kind === 'user') return t('kind.user')
  if (record.kind === 'assistant') return t('kind.assistant')
  if (record.kind === 'tool') return record.depth > 0 ? t('kind.subtool') : t('kind.tool')
  if (record.kind === 'code') return t('kind.code')
  if (record.kind === 'error') return t('kind.error')
  return t('kind.event')
}

function turnStatus(turn: TrajectoryTurn, t: Translate<'trajectory'>) {
  if (turn.status === 'complete') return t('status.completed')
  if (turn.status === 'error') return t('status.failed')
  if (turn.status === 'cancelled') return t('status.cancelled')
  return t('status.pending')
}

function flattenRows(turns: TrajectoryTurn[], collapsed: ReadonlySet<string>): LedgerRow[] {
  return turns.flatMap(turn => {
    const rows: LedgerRow[] = [{ key: `turn:${turn.runId}`, kind: 'turn', turn }]
    if (collapsed.has(turn.runId)) return rows
    for (const group of turn.groups) {
      rows.push({ key: `group:${turn.runId}:${group.key}`, kind: 'group', turn, group })
      rows.push(...group.records.map(record => ({ key: `record:${record.key}`, kind: 'record' as const, turn, record })))
    }
    return rows
  })
}

export const TrajectoryLedger = React.forwardRef<TrajectoryLedgerHandle, {
  turns: TrajectoryTurn[]
  collapsed: ReadonlySet<string>
  rangeKeys: ReadonlySet<string> | null
  selection: DetailsSelection
  onToggleTurn(runId: string): void
  onSelect(record: TrajectoryRecord): void
  t: Translate<'trajectory'>
  errorT: Translate<'chat'>
}>(function TrajectoryLedger({ turns, collapsed, rangeKeys, selection, onToggleTurn, onSelect, t, errorT }, forwardedRef) {
  const rows = React.useMemo(() => flattenRows(turns, collapsed), [collapsed, turns])
  const rootRef = React.useRef<HTMLDivElement>(null)
  const [scrollElement, setScrollElement] = React.useState<HTMLElement | null>(null)
  const [scrollMargin, setScrollMargin] = React.useState(0)
  const virtualized = rows.length >= VIRTUALIZATION_THRESHOLD
  const virtualizer = useVirtualizer({
    count: virtualized ? rows.length : 0,
    getScrollElement: () => scrollElement,
    estimateSize: index => rows[index]?.kind === 'record'
      && typeof matchMedia === 'function'
      && matchMedia('(max-width: 760px)').matches ? 52 : 30,
    overscan: VIRTUAL_OVERSCAN,
    scrollMargin,
    getItemKey: index => rows[index]?.key ?? index,
  })

  React.useLayoutEffect(() => {
    const root = rootRef.current
    const scroller = root?.closest<HTMLElement>('.conversation-scroll') ?? null
    setScrollElement(scroller)
    if (!root || !scroller) return
    const measure = () => {
      const next = root.getBoundingClientRect().top - scroller.getBoundingClientRect().top + scroller.scrollTop
      setScrollMargin(next)
      virtualizer.measure()
    }
    measure()
    const observer = new ResizeObserver(measure)
    observer.observe(root)
    return () => observer.disconnect()
  }, [virtualized])

  const focusRecord = React.useCallback((key: string) => {
    const index = rows.findIndex(row => row.kind === 'record' && row.record.key === key)
    if (index < 0) return
    if (virtualized) {
      virtualizer.scrollToIndex(index, { align: 'center' })
      return
    }
    const recordElement = [...(rootRef.current?.querySelectorAll<HTMLElement>('[data-record-key]') ?? [])]
      .find(row => row.dataset.recordKey === key)
    recordElement?.scrollIntoView({ block: 'nearest' })
  }, [rows, virtualized, virtualizer])

  React.useImperativeHandle(forwardedRef, () => ({ focusRecord }), [focusRecord])

  const renderRow = (row: LedgerRow) => {
    if (row.kind === 'turn') {
      const closed = collapsed.has(row.turn.runId)
      return (
        <button
          type="button"
          className={css.turnHeader}
          aria-expanded={!closed}
          onClick={() => onToggleTurn(row.turn.runId)}
        >
          <span className={css.turnTitle}>{closed ? <ChevronRight /> : <ChevronDown />}{t('turn.label', { turn: row.turn.number })}</span>
          <code>{row.turn.runId}</code>
          <span className={css.turnStatus}>{turnStatus(row.turn, t)}</span>
          <span>{formatDuration(row.turn.durationMs)}</span>
        </button>
      )
    }
    if (row.kind === 'group') {
      const calls = row.group.records.filter(record => record.kind === 'tool' || record.kind === 'code').length
      return (
        <div className={css.groupHeader}>
          <strong>{row.group.key === 'message' ? t('group.message') : t('group.step', { step: row.group.records[0]?.step ?? 0 })}</strong>
          {calls > 0 && <span>{t('group.calls', { count: calls })}</span>}
        </div>
      )
    }
    const record = row.record
    const Icon = recordIcon(record)
    const active = selection?.kind === 'tool'
      ? selection.trace.id === record.trace?.id
      : selection?.kind === 'event' && selection.event.seq === record.event.seq
    return (
      <button
        type="button"
        className={css.record}
        data-trajectory-record=""
        data-record-key={record.key}
        data-kind={record.kind}
        data-selected={active || undefined}
        data-error={record.error || undefined}
        data-outside-range={rangeKeys !== null && !rangeKeys.has(record.key) || undefined}
        data-depth={record.depth || undefined}
        style={{ '--trajectory-depth': record.depth } as React.CSSProperties}
        onClick={() => onSelect(record)}
      >
        <span className={css.index}>#{record.event.seq}</span>
        <span className={css.tag}><Icon />{recordTag(record, t)}</span>
        <span className={css.body}>
          <strong>{trajectoryRecordTitle(record, t)}</strong>
          {record.requestNumber != null && <small>{t('record.request', { number: record.requestNumber })}</small>}
          <span>{recordSummary(record, t, errorT)}</span>
          <span className={css.mobileMetrics}>
            <span>{t('record.input', { tokens: compactTokens(record.inputTokens) })}</span>
            <span>{t('record.output', { tokens: compactTokens(record.outputTokens) })}</span>
            <span>{t('record.reasoning', { tokens: compactTokens(record.reasoningTokens) })}</span>
            <span>{t('record.cache', { tokens: compactTokens(record.cachedTokens) })}</span>
          </span>
        </span>
        <span className={css.metric} data-metric={t('column.input')}>{compactTokens(record.inputTokens)}</span>
        <span className={css.metric} data-metric={t('column.output')}>{compactTokens(record.outputTokens)}</span>
        <span className={css.metric} data-metric={t('column.think')}>{compactTokens(record.reasoningTokens)}</span>
        <span className={css.metric} data-metric={t('column.cache')}>{compactTokens(record.cachedTokens)}</span>
        <span className={css.time}>{record.running ? t('record.running') : record.timing?.durationMs == null ? formatTime(record.event.occurred_at_ms).split(' ').at(-1) : formatDuration(record.timing.durationMs)}</span>
      </button>
    )
  }

  const virtualRows = virtualizer.getVirtualItems()
  return (
    <div ref={rootRef} aria-label={t('ledger.aria')} className={css.ledger} data-trajectory-ledger="" data-virtualized={virtualized || undefined} role="list">
      {virtualized ? (
        <div className={css.virtualCanvas} style={{ height: virtualizer.getTotalSize() }}>
          {virtualRows.map(virtualRow => {
            const row = rows[virtualRow.index]!
            return (
              <div
                className={css.virtualRow}
                role="listitem"
                data-status={row.turn.status}
                data-index={virtualRow.index}
                key={row.key}
                ref={virtualizer.measureElement}
                style={{ transform: `translateY(${virtualRow.start - scrollMargin}px)` }}
              >
                {renderRow(row)}
              </div>
            )
          })}
        </div>
      ) : rows.map(row => <div className={css.ledgerRow} data-status={row.turn.status} key={row.key} role="listitem">{renderRow(row)}</div>)}
    </div>
  )
})
