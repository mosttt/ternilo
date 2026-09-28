import { Box, Clock3, FoldVertical, Search, UnfoldVertical } from 'lucide-react'
import type { TrajectoryTimelineMode } from '@/domain/trajectory-timeline'
import type { Translate } from '@/i18n/runtime'
import css from './trajectory-toolbar.module.css'

export function TrajectoryToolbar({
  mode,
  allTurnsCollapsed,
  callsCollapsed,
  query,
  onModeChange,
  onToggleTurns,
  onToggleCalls,
  onQueryChange,
  t,
}: {
  mode: TrajectoryTimelineMode
  allTurnsCollapsed: boolean
  callsCollapsed: boolean
  query: string
  onModeChange(mode: TrajectoryTimelineMode): void
  onToggleTurns(): void
  onToggleCalls(): void
  onQueryChange(query: string): void
  t: Translate<'trajectory'>
}) {
  const actual = mode === 'actual'
  return (
    <div className={css.root} role="toolbar" aria-label={t('toolbar.aria')} data-trajectory-toolbar="">
      <div className={css.actions}>
        <button
          type="button"
          className={css.action}
          aria-pressed={actual}
          title={actual ? t('toolbar.compactIdle') : t('toolbar.actualTime')}
          onClick={() => onModeChange(actual ? 'duration' : 'actual')}
        >
          <Clock3 />
          <span>{actual ? t('toolbar.actualTime') : t('toolbar.compactIdle')}</span>
        </button>
        <button
          type="button"
          className={css.action}
          aria-pressed={allTurnsCollapsed}
          title={allTurnsCollapsed ? t('toolbar.expandTurns') : t('toolbar.collapseTurns')}
          onClick={onToggleTurns}
        >
          {allTurnsCollapsed ? <UnfoldVertical /> : <FoldVertical />}
          <span>{t('toolbar.turns')}</span>
        </button>
        <button
          type="button"
          className={css.action}
          aria-pressed={callsCollapsed}
          title={callsCollapsed ? t('toolbar.expandCalls') : t('toolbar.collapseCalls')}
          onClick={onToggleCalls}
        >
          <Box />
          <span>{t('toolbar.calls')}</span>
        </button>
      </div>
      <label className={css.search}>
        <Search aria-hidden="true" />
        <input
          type="search"
          aria-label={t('toolbar.search')}
          placeholder={t('toolbar.searchPlaceholder')}
          value={query}
          onChange={event => onQueryChange(event.currentTarget.value)}
        />
      </label>
    </div>
  )
}
