import * as React from 'react'
import type { SessionStats } from '@/types'
import type { Translate } from '@/i18n/runtime'
import { Tooltip, TooltipContent, TooltipTrigger } from '@/components/ui/tooltip'
import css from './session-stats-line.module.css'

function formatTokens(value: number) {
  const scaled = (number: number) => String(Math.round(number * 10) / 10)
  if (value < 1_000) return String(value)
  if (value < 1_000_000) return `${scaled(value / 1_000)}K`
  return `${scaled(value / 1_000_000)}M`
}

function formatDuration(value: number) {
  const seconds = value / 1_000
  if (seconds < 60) return `${Math.round(seconds * 10) / 10}s`
  const rounded = Math.round(seconds)
  return `${Math.floor(rounded / 60)}m${rounded % 60}s`
}

export function hasHorizontalOverflow(scrollWidth: number, clientWidth: number) {
  return scrollWidth > clientWidth + 1
}

export function sessionStatsGroups(stats: SessionStats | null, t: Translate<'conversation'>): string[] {
  if (!stats) return []
  const groups: string[] = []
  if (stats.steps > 0) {
    groups.push(t('stats.turnsSteps', { turns: stats.turns, steps: stats.steps }))
    const durations: string[] = []
    if (stats.model_duration_ms > 0) durations.push(t('stats.llmDuration', { duration: formatDuration(stats.model_duration_ms) }))
    if (stats.tool_duration_ms > 0) durations.push(t('stats.toolDuration', { duration: formatDuration(stats.tool_duration_ms) }))
    if (durations.length) groups.push(durations.join(' · '))

    const speeds: string[] = []
    if (stats.measured_first_tokens > 0) {
      speeds.push(t('stats.firstToken', { duration: formatDuration(stats.first_token_duration_ms / stats.measured_first_tokens) }))
    }
    if (stats.generation_duration_ms > 0 && stats.generation_output_tokens > 0) {
      const throughput = stats.generation_output_tokens / (stats.generation_duration_ms / 1_000)
      speeds.push(t('stats.throughput', { speed: Math.round(throughput * 10) / 10 }))
    }
    if (speeds.length) groups.push(speeds.join(' · '))
  }

  if (stats.exact_input_tokens > 0 || stats.exact_output_tokens > 0 || stats.exact_reasoning_tokens > 0) {
    if (stats.exact_input_tokens > 0) {
      const ratio = Math.min(100, Math.max(0, Math.round(stats.cached_input_tokens / stats.exact_input_tokens * 100)))
      groups.push(t('stats.cacheHit', { ratio }))
    }
    const usage = [
      t('stats.input', { tokens: formatTokens(stats.exact_input_tokens) }),
      t('stats.output', { tokens: formatTokens(stats.exact_output_tokens) }),
    ]
    if (stats.exact_reasoning_tokens > 0) usage.push(t('stats.reasoning', { tokens: formatTokens(stats.exact_reasoning_tokens) }))
    groups.push(usage.join(' · '))
  }
  return groups
}

export function SessionStatsLine({ stats, t }: { stats: SessionStats | null; t: Translate<'conversation'> }) {
  const groups = sessionStatsGroups(stats, t)
  const label = groups.join(' | ')
  const rootRef = React.useRef<HTMLDivElement>(null)
  const [truncated, setTruncated] = React.useState(false)
  React.useLayoutEffect(() => {
    const element = rootRef.current
    if (!element) return
    const measure = () => setTruncated(hasHorizontalOverflow(element.scrollWidth, element.clientWidth))
    measure()
    if (typeof ResizeObserver === 'undefined') return
    const observer = new ResizeObserver(measure)
    observer.observe(element)
    return () => observer.disconnect()
  }, [label])
  if (!groups.length) return null
  const row = (
    <div ref={rootRef} className={`${css.root} session-stats-line`} data-truncated={truncated || undefined} aria-label={label} tabIndex={truncated ? 0 : undefined}>
      {groups.map((group, index) => (
        <span key={`${index}:${group}`} className={css.group} data-session-stats-group="">
          {index > 0 && <span className={css.separator} aria-hidden>｜</span>}
          <span>{group}</span>
        </span>
      ))}
    </div>
  )
  return (
    <Tooltip delayDuration={500}>
      <TooltipTrigger asChild>{row}</TooltipTrigger>
      {truncated && <TooltipContent side="top" className="max-w-xl">{label}</TooltipContent>}
    </Tooltip>
  )
}
