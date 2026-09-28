import * as React from 'react'
import { Hand, MousePointer2, RotateCcw, ZoomIn, ZoomOut } from 'lucide-react'
import { trajectoryRecordTitle, type TrajectoryRecord, type TrajectoryTurn } from '@/domain/trajectory'
import {
  buildTrajectoryTimeline,
  clampTimelineRange,
  normalizeTimelineSelection,
  panTimelineRange,
  zoomTimelineRange,
  type TrajectoryTimelineMode,
  type TrajectoryTimeRange,
} from '@/domain/trajectory-timeline'
import { useTranslate } from '@/i18n/provider'
import { formatDuration } from '@/lib/utils'
import css from './trajectory-timeline.module.css'

const lanes = [
  { id: 'input', labelKey: 'column.input', index: 0 },
  { id: 'model', labelKey: 'column.model', index: 1 },
  { id: 'tools', labelKey: 'column.tools', index: 2 },
] as const

type Interaction =
  | { kind: 'range'; pointerId: number; startTime: number; startClientX: number; startClientY: number }
  | { kind: 'pan'; pointerId: number; startClientX: number; viewport: TrajectoryTimeRange; resetOnClick: boolean }

function intersects(span: TrajectoryTimeRange, range: TrajectoryTimeRange | null) {
  return range === null || span.start <= range.end && span.end >= range.start
}

export function TrajectoryTimeline({ turns, mode, range, selectedKey, onRangeChange, onSelect }: {
  turns: TrajectoryTurn[]
  mode: TrajectoryTimelineMode
  range: TrajectoryTimeRange | null
  selectedKey?: string
  onRangeChange(range: TrajectoryTimeRange | null): void
  onSelect(record: TrajectoryRecord): void
}) {
  const t = useTranslate('trajectory')
  const records = React.useMemo(() => turns.flatMap(turn => turn.records), [turns])
  const model = React.useMemo(() => buildTrajectoryTimeline(records, mode), [mode, records])
  const plotRef = React.useRef<HTMLDivElement>(null)
  const previousDomain = React.useRef<TrajectoryTimeRange | null>(null)
  const previousMode = React.useRef<TrajectoryTimelineMode | null>(null)
  const interaction = React.useRef<Interaction | null>(null)
  const [viewport, setViewport] = React.useState<TrajectoryTimeRange>({ start: 0, end: 1 })
  const [draft, setDraft] = React.useState<TrajectoryTimeRange | null>(null)
  const [interactionMode, setInteractionMode] = React.useState<'range' | 'pan'>('range')
  const [dragging, setDragging] = React.useState<'range' | 'pan' | null>(null)

  const domain = React.useMemo(() => model ? { start: model.start, end: model.end } : { start: 0, end: 1 }, [model])
  const minimumDuration = model ? Math.min(model.duration, Math.max(20, model.duration / Math.max(4, model.spans.length))) : 1

  React.useLayoutEffect(() => {
    if (!model) return
    setViewport(current => {
      const previous = previousDomain.current
      const modeChanged = previousMode.current !== null && previousMode.current !== mode
      const wasFullDomain = modeChanged || previous === null
        || Math.abs(current.start - previous.start) < 0.5 && Math.abs(current.end - previous.end) < 0.5
      return wasFullDomain ? domain : clampTimelineRange(current, domain)
    })
    previousDomain.current = domain
    previousMode.current = mode
  }, [domain, mode, model])

  React.useEffect(() => {
    if (!model || !selectedKey) return
    const selected = model.spans.find(span => span.key === selectedKey)
    if (!selected) return
    setViewport(current => {
      const duration = current.end - current.start
      if (selected.start >= current.start && selected.end <= current.end) return current
      if (selected.start < current.start) return clampTimelineRange({ start: selected.start, end: selected.start + duration }, domain)
      return clampTimelineRange({ start: selected.end - duration, end: selected.end }, domain)
    })
  }, [domain, model, selectedKey])

  React.useEffect(() => {
    if (model && range && (range.end < model.start || range.start > model.end)) onRangeChange(null)
  }, [model?.end, model?.start, onRangeChange, range])

  React.useEffect(() => {
    const plot = plotRef.current
    if (!plot || !model) return
    const onWheel = (event: WheelEvent) => {
      event.preventDefault()
      event.stopPropagation()
      const bounds = plot.getBoundingClientRect()
      const fraction = Math.min(1, Math.max(0, (event.clientX - bounds.left) / Math.max(1, bounds.width)))
      setViewport(current => {
        const anchor = current.start + fraction * (current.end - current.start)
        return zoomTimelineRange(current, anchor, Math.exp(event.deltaY * 0.0015), domain, minimumDuration)
      })
    }
    plot.addEventListener('wheel', onWheel, { passive: false })
    return () => plot.removeEventListener('wheel', onWheel)
  }, [domain, minimumDuration, model])

  if (!model) return null

  const timeAt = (clientX: number) => {
    const bounds = plotRef.current?.getBoundingClientRect()
    if (!bounds) return viewport.start
    const fraction = Math.min(1, Math.max(0, (clientX - bounds.left) / Math.max(1, bounds.width)))
    return viewport.start + fraction * (viewport.end - viewport.start)
  }
  const reset = () => {
    setViewport(domain)
    setDraft(null)
    onRangeChange(null)
  }
  const zoom = (scale: number) => {
    const middle = (viewport.start + viewport.end) / 2
    setViewport(current => zoomTimelineRange(current, middle, scale, domain, minimumDuration))
  }
  const recordAt = (clientX: number, clientY: number) => {
    const bounds = plotRef.current?.getBoundingClientRect()
    if (!bounds) return undefined
    const lane = Math.min(lanes.length - 1, Math.max(0, Math.floor((clientY - bounds.top) / Math.max(1, bounds.height) * lanes.length)))
    const visible = model.spans.filter(span => span.end >= viewport.start && span.start <= viewport.end)
    const candidates = visible.filter(span => span.lane === lane)
    const point = timeAt(clientX)
    return (candidates.length ? candidates : visible).reduce<(typeof model.spans)[number] | undefined>((nearest, span) => {
      if (!nearest) return span
      const distance = point < span.start ? span.start - point : point > span.end ? point - span.end : 0
      const nearestDistance = point < nearest.start ? nearest.start - point : point > nearest.end ? point - nearest.end : 0
      return distance < nearestDistance ? span : nearest
    }, undefined)?.record
  }
  const onPointerDown = (event: React.PointerEvent<HTMLDivElement>) => {
    if (event.button !== 0 && event.button !== 2) return
    const shouldPan = event.button === 2 || interactionMode === 'pan'
    interaction.current = shouldPan
      ? { kind: 'pan', pointerId: event.pointerId, startClientX: event.clientX, viewport, resetOnClick: event.button === 2 }
      : {
          kind: 'range', pointerId: event.pointerId, startTime: timeAt(event.clientX),
          startClientX: event.clientX, startClientY: event.clientY,
        }
    setDragging(shouldPan ? 'pan' : 'range')
    if (!shouldPan) setDraft({ start: timeAt(event.clientX), end: timeAt(event.clientX) })
    event.currentTarget.setPointerCapture(event.pointerId)
    event.preventDefault()
  }
  const onPointerMove = (event: React.PointerEvent<HTMLDivElement>) => {
    const active = interaction.current
    if (!active || active.pointerId !== event.pointerId) return
    if (active.kind === 'range') {
      const current = timeAt(event.clientX)
      setDraft({ start: Math.min(active.startTime, current), end: Math.max(active.startTime, current) })
      return
    }
    const bounds = event.currentTarget.getBoundingClientRect()
    const delta = -(event.clientX - active.startClientX) / Math.max(1, bounds.width) * (active.viewport.end - active.viewport.start)
    setViewport(panTimelineRange(active.viewport, delta, domain))
  }
  const finishPointer = (event: React.PointerEvent<HTMLDivElement>) => {
    const active = interaction.current
    if (!active || active.pointerId !== event.pointerId) return
    const moved = active.kind === 'range'
      ? Math.hypot(event.clientX - active.startClientX, event.clientY - active.startClientY)
      : Math.abs(event.clientX - active.startClientX)
    if (active.kind === 'range') {
      setDraft(null)
      if (moved < 6) {
        const record = recordAt(event.clientX, event.clientY)
        onRangeChange(null)
        if (record) onSelect(record)
      } else {
        const selected = normalizeTimelineSelection(active.startTime, timeAt(event.clientX), domain, minimumDuration)
        onRangeChange(selected)
      }
    } else if (active.resetOnClick && moved < 3) {
      reset()
    }
    interaction.current = null
    setDragging(null)
    if (event.currentTarget.hasPointerCapture(event.pointerId)) event.currentTarget.releasePointerCapture(event.pointerId)
  }
  const cancelPointer = (event: React.PointerEvent<HTMLDivElement>) => {
    if (interaction.current?.pointerId !== event.pointerId) return
    interaction.current = null
    setDraft(null)
    setDragging(null)
    if (event.currentTarget.hasPointerCapture(event.pointerId)) event.currentTarget.releasePointerCapture(event.pointerId)
  }
  const spanStyle = (span: TrajectoryTimeRange): React.CSSProperties | undefined => {
    const visibleStart = Math.max(span.start, viewport.start)
    const visibleEnd = Math.min(span.end, viewport.end)
    if (visibleEnd < viewport.start || visibleStart > viewport.end) return undefined
    const duration = Math.max(1, viewport.end - viewport.start)
    const left = (visibleStart - viewport.start) / duration * 100
    const width = Math.max(0, visibleEnd - visibleStart) / duration * 100
    return { left: `${left}%`, width: `max(3px, ${width}%)` }
  }
  const activeRange = draft ?? range
  const rangeStyle = activeRange ? {
    left: `${(Math.max(viewport.start, activeRange.start) - viewport.start) / (viewport.end - viewport.start) * 100}%`,
    width: `${Math.max(0, Math.min(viewport.end, activeRange.end) - Math.max(viewport.start, activeRange.start)) / (viewport.end - viewport.start) * 100}%`,
  } : undefined

  return (
    <div
      className={css.root}
      data-trajectory-timeline=""
      tabIndex={0}
      data-dragging={dragging ?? undefined}
      aria-label={t('timeline.total', { mode: mode === 'actual' ? t('timeline.actual') : t('toolbar.compactIdle'), duration: formatDuration(model.duration) })}
      onKeyDown={event => { if (event.key === 'Escape') reset() }}
    >
      <div className={css.header}>
        <span>{mode === 'actual' ? t('timeline.actual') : t('timeline.compact', { duration: formatDuration(model.removedIdleMs) })}</span>
        {range && <span className={css.rangeStatus} data-trajectory-timeline-range="">{t('timeline.range', { duration: formatDuration(range.end - range.start) })}</span>}
        <div className={css.controls} data-trajectory-timeline-controls="" aria-label={t('timeline.aria')}>
          <button type="button" aria-label={interactionMode === 'range' ? t('timeline.switchPan') : t('timeline.switchRange')} title={interactionMode === 'range' ? t('timeline.rangeMode') : t('timeline.panMode')} onClick={() => setInteractionMode(value => value === 'range' ? 'pan' : 'range')}>{interactionMode === 'range' ? <MousePointer2 /> : <Hand />}</button>
          <button type="button" aria-label={t('timeline.zoomIn')} title={t('timeline.zoomIn')} onClick={() => zoom(0.65)}><ZoomIn /></button>
          <button type="button" aria-label={t('timeline.zoomOut')} title={t('timeline.zoomOut')} onClick={() => zoom(1 / 0.65)}><ZoomOut /></button>
          <button type="button" aria-label={t('timeline.reset')} title={t('timeline.resetTitle')} onClick={reset}><RotateCcw /></button>
        </div>
      </div>
      <div className={css.scale} data-trajectory-timeline-scale=""><span>{formatDuration(viewport.start - model.start)}</span><span>{formatDuration(viewport.end - model.start)}</span></div>
      <div className={css.body}>
        <div className={css.labels} aria-hidden>{lanes.map(lane => <span key={lane.id}>{t(lane.labelKey)}</span>)}</div>
        <div
          ref={plotRef}
          className={css.plot}
          data-trajectory-timeline-plot=""
          onPointerDown={onPointerDown}
          onPointerMove={onPointerMove}
          onPointerUp={finishPointer}
          onPointerCancel={cancelPointer}
          onDoubleClick={reset}
          onContextMenu={event => event.preventDefault()}
        >
          {activeRange && <div className={css.selection} style={rangeStyle} />}
          {lanes.map(lane => (
            <div className={css.track} key={lane.id}>
              {model.spans.map(span => {
                if (span.lane !== lane.index) return null
                const style = spanStyle(span)
                if (!style) return null
                const record = span.record
                const durationMs = record.timing?.durationMs
                const ttftMs = record.timing?.firstTokenDurationMs
                const decodingMs = durationMs == null || ttftMs == null ? null : Math.max(0, durationMs - ttftMs)
                const ttftFraction = durationMs != null && durationMs > 0 && ttftMs != null
                  ? Math.min(1, Math.max(0, ttftMs / durationMs))
                  : null
                const kindLabel = record.kind === 'user' ? t('kind.user')
                  : record.kind === 'assistant' ? t('kind.assistant')
                    : record.kind === 'tool' ? record.depth > 0 ? t('kind.subtool') : t('kind.tool')
                      : record.kind === 'code' ? t('kind.code')
                        : record.kind === 'error' ? t('kind.error') : t('kind.event')
                const timingLabel = durationMs == null ? ''
                  : ttftMs != null && decodingMs != null
                    ? t('timeline.ttftDecoding', { ttft: formatDuration(ttftMs), decoding: formatDuration(decodingMs) })
                    : t('timeline.recordDuration', { duration: formatDuration(durationMs) })
                const recordTitle = trajectoryRecordTitle(record, t)
                return (
                  <span
                    aria-hidden="true"
                    key={record.key}
                    className={css.block}
                    data-trajectory-timeline-block=""
                    data-kind={record.kind}
                    data-assistant-timing={ttftFraction == null ? undefined : 'true'}
                    data-error={record.error || undefined}
                    data-running={record.running || undefined}
                    data-selected={selectedKey === record.key || undefined}
                    data-outside-range={!intersects(span, activeRange) || undefined}
                    style={{ ...style, ...(ttftFraction == null ? {} : { '--trajectory-ttft': `${ttftFraction * 100}%` }) } as React.CSSProperties}
                    title={[kindLabel, record.kind === 'tool' || record.kind === 'code' ? recordTitle : record.summary || recordTitle, timingLabel].filter(Boolean).join(' · ')}
                  />
                )
              })}
            </div>
          ))}
        </div>
      </div>
    </div>
  )
}
