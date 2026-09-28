import * as React from 'react'
import type { SessionEvent } from '@/types'
import { buildTrajectory, trajectoryRecordTitle, type TrajectoryRecord, type TrajectoryTurn } from '@/domain/trajectory'
import { readTrajectoryDurationMode, writeTrajectoryDurationMode } from '@/domain/trajectory-duration-store'
import { buildTrajectoryTimeline, trajectoryKeysInRange, type TrajectoryTimeRange } from '@/domain/trajectory-timeline'
import { useTrajectoryTail } from '@/hooks/use-trajectory-tail'
import { useLocale, useTranslate } from '@/i18n/provider'
import type { Translate } from '@/i18n/runtime'
import type { DetailsSelection } from './details-panel'
import { TrajectoryLedger, TrajectoryLedgerHeader, type TrajectoryLedgerHandle } from './trajectory-ledger'
import { TrajectoryTimeline } from './trajectory-timeline'
import { TrajectoryToolbar } from './trajectory-toolbar'
import css from './trajectory-view.module.css'

export function filterTrajectoryTurns(
  turns: TrajectoryTurn[],
  query: string,
  hideTools: boolean,
  selectedKey: string | undefined,
  t: Translate<'trajectory'>,
) {
  const normalized = query.trim().toLocaleLowerCase()
  return turns.flatMap(turn => {
    const groups = turn.groups.flatMap(group => {
      const records = group.records.filter(record => {
        if (record.key === selectedKey) return true
        if (hideTools && (record.kind === 'tool' || record.kind === 'code')) return false
        return !normalized || `${record.tag} ${trajectoryRecordTitle(record, t)} ${record.summary} ${record.reasoningContent ?? ''} ${record.event.type}`.toLocaleLowerCase().includes(normalized)
      })
      return records.length ? [{ ...group, records }] : []
    })
    const records = groups.flatMap(group => group.records)
    return records.length ? [{ ...turn, groups, records }] : []
  })
}

export function TrajectoryView({ sessionId, events, selection, onSelect }: {
  sessionId: string
  events: SessionEvent[]
  selection: DetailsSelection
  onSelect(selection: DetailsSelection): void
}) {
  const t = useTranslate('trajectory')
  const errorT = useTranslate('chat')
  const { locale } = useLocale()
  const [query, setQuery] = React.useState('')
  const [collapsed, setCollapsed] = React.useState<Set<string>>(new Set())
  const [hideTools, setHideTools] = React.useState(false)
  const [durationMode, setDurationMode] = React.useState(() => readTrajectoryDurationMode(typeof window === 'undefined' ? undefined : window.localStorage))
  const [timelineRange, setTimelineRange] = React.useState<TrajectoryTimeRange | null>(null)
  const ledgerRef = React.useRef<TrajectoryLedgerHandle>(null)
  const rootRef = useTrajectoryTail(sessionId, events.at(-1)?.seq ?? 0)
  const deferredEvents = React.useDeferredValue(events)
  const deferredQuery = React.useDeferredValue(query)
  const turns = React.useMemo(() => buildTrajectory(deferredEvents), [deferredEvents])
  const allRecords = React.useMemo(() => turns.flatMap(turn => turn.records), [turns])
  const selectedRecord = React.useMemo(() => allRecords.find(record => selection?.kind === 'tool'
    ? selection.trace.id === record.trace?.id
    : selection?.kind === 'event' && selection.event.seq === record.event.seq), [allRecords, selection])
  const selectedKey = selectedRecord?.key
  const visibleTurns = React.useMemo(
    () => filterTrajectoryTurns(turns, deferredQuery, hideTools, selectedKey, t),
    [deferredQuery, hideTools, locale, selectedKey, t, turns],
  )
  const allCollapsed = visibleTurns.length > 0 && visibleTurns.every(turn => collapsed.has(turn.runId))
  const timelineModel = React.useMemo(() => buildTrajectoryTimeline(visibleTurns.flatMap(turn => turn.records), durationMode), [durationMode, visibleTurns])
  const rangeKeys = React.useMemo(() => trajectoryKeysInRange(timelineModel, timelineRange), [timelineModel, timelineRange])

  React.useEffect(() => {
    writeTrajectoryDurationMode(typeof window === 'undefined' ? undefined : window.localStorage, durationMode)
    setTimelineRange(null)
  }, [durationMode])

  const selectRecord = React.useCallback((record: TrajectoryRecord) => {
    if (record.trace) onSelect({ kind: 'tool', trace: record.trace })
    else onSelect({
      kind: 'event', event: record.event, relatedEvents: record.relatedEvents,
      timing: record.timing, reasoningContent: record.reasoningContent,
    })
  }, [onSelect])

  const selectTimelineRecord = React.useCallback((record: TrajectoryRecord) => {
    selectRecord(record)
    requestAnimationFrame(() => ledgerRef.current?.focusRecord(record.key))
  }, [selectRecord])

  React.useEffect(() => {
    if (!selectedKey) return
    if (selectedRecord && collapsed.has(selectedRecord.event.run_id)) {
      setCollapsed(current => {
        if (!current.has(selectedRecord.event.run_id)) return current
        const next = new Set(current)
        next.delete(selectedRecord.event.run_id)
        return next
      })
      return
    }
    const frame = requestAnimationFrame(() => ledgerRef.current?.focusRecord(selectedKey))
    return () => cancelAnimationFrame(frame)
  }, [collapsed, selectedKey, selectedRecord])

  if (!events.length) return <div ref={rootRef} className={css.empty} data-trajectory-state="empty">{t('history.empty')}</div>
  return (
    <div ref={rootRef} className={css.root} data-trajectory-root="" data-trajectory-state={visibleTurns.length ? 'ready' : 'empty'}>
      <div className={css.stickyChrome} data-trajectory-sticky-chrome="">
        <TrajectoryToolbar
          mode={durationMode}
          allTurnsCollapsed={allCollapsed}
          callsCollapsed={hideTools}
          query={query}
          onModeChange={setDurationMode}
          onToggleTurns={() => setCollapsed(allCollapsed ? new Set() : new Set(visibleTurns.map(turn => turn.runId)))}
          onToggleCalls={() => setHideTools(value => !value)}
          onQueryChange={setQuery}
          t={t}
        />
        <TrajectoryTimeline turns={visibleTurns} mode={durationMode} range={timelineRange} selectedKey={selectedKey} onRangeChange={setTimelineRange} onSelect={selectTimelineRecord} />
        {visibleTurns.length ? <TrajectoryLedgerHeader t={t} /> : null}
      </div>

      {visibleTurns.length ? (
        <TrajectoryLedger
          ref={ledgerRef}
          turns={visibleTurns}
          collapsed={collapsed}
          rangeKeys={rangeKeys}
          selection={selection}
          onToggleTurn={runId => setCollapsed(current => {
            const next = new Set(current)
            if (next.has(runId)) next.delete(runId)
            else next.add(runId)
            return next
          })}
          onSelect={selectRecord}
          t={t}
          errorT={errorT}
        />
      ) : <div className={css.empty}>{t('history.noMatch')}</div>}
    </div>
  )
}
