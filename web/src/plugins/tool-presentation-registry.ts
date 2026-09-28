import * as React from 'react'
import type { LucideIcon } from 'lucide-react'
import type { ToolTrace } from '@/domain/events'
import type { Translate } from '@/i18n/runtime'
import { ContributionRegistry, type ContributionDisposer } from './contribution-registry'

export interface ToolResultRenderContext {
  trace: ToolTrace
  t: Translate<'chat'>
}

export interface ToolPresentationContribution<View> {
  id: string
  priority: number
  matches(trace: ToolTrace): boolean
  parse(trace: ToolTrace): View | null
  icon(trace: ToolTrace): LucideIcon
  title(trace: ToolTrace, t: Translate<'chat'>): string
  target(trace: ToolTrace): string
  badge?(trace: ToolTrace, t: Translate<'chat'>): string | undefined
  render(view: View, context: ToolResultRenderContext): React.ReactNode
  kind?(trace: ToolTrace): string | undefined
}

export interface RegisteredToolPresentationContribution {
  id: string
  priority: number
  matches(trace: ToolTrace): boolean
  parse(trace: ToolTrace): unknown | null
  icon(trace: ToolTrace): LucideIcon
  title(trace: ToolTrace, t: Translate<'chat'>): string
  target(trace: ToolTrace): string
  badge?(trace: ToolTrace, t: Translate<'chat'>): string | undefined
  render(view: unknown, context: ToolResultRenderContext): React.ReactNode
  kind?(trace: ToolTrace): string | undefined
}

export interface ResolvedToolPresentation {
  contribution: RegisteredToolPresentationContribution
  result: {
    contribution: RegisteredToolPresentationContribution
    view: unknown
  } | null
}

export class ToolPresentationRegistry {
  readonly #entries = new ContributionRegistry<RegisteredToolPresentationContribution>(
    contribution => contribution.id,
    (left, right) => right.priority - left.priority || left.id.localeCompare(right.id),
  )

  getSnapshot = this.#entries.getSnapshot
  subscribe = this.#entries.subscribe

  register<View>(contribution: ToolPresentationContribution<View>): ContributionDisposer {
    return this.#entries.register({
      ...contribution,
      parse: trace => contribution.parse(trace),
      render: (view, context) => contribution.render(view as View, context),
    })
  }

  resolve(
    trace: ToolTrace,
    entries: readonly RegisteredToolPresentationContribution[] = this.getSnapshot(),
  ): ResolvedToolPresentation | null {
    let contribution: RegisteredToolPresentationContribution | undefined
    for (const candidate of entries) {
      if (!candidate.matches(trace)) continue
      contribution ??= candidate
      const view = candidate.parse(trace)
      if (view !== null) return {
        contribution,
        result: { contribution: candidate, view },
      }
    }
    return contribution ? { contribution, result: null } : null
  }
}

export const toolPresentationRegistry = new ToolPresentationRegistry()

export function registerToolPresentation<View>(contribution: ToolPresentationContribution<View>) {
  return toolPresentationRegistry.register(contribution)
}

export function useToolPresentation(trace: ToolTrace) {
  const entries = React.useSyncExternalStore(
    toolPresentationRegistry.subscribe,
    toolPresentationRegistry.getSnapshot,
    toolPresentationRegistry.getSnapshot,
  )
  return React.useMemo(() => toolPresentationRegistry.resolve(trace, entries), [entries, trace])
}
