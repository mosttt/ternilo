import * as React from 'react'
import { executionTargetKey, type ExecutionTarget } from '@/domain/execution-target'
import {
  loadProviderInventory, peekProviderInventory, subscribeProviderInventory,
} from '@/domain/provider-inventory'
import { resolvedProviderModel } from '@/domain/provider-model'
import { currentCloudModel, loadCloudModelInventory, peekCloudModelInventory, subscribeCloudModelInventory } from '@/domain/cloud-model-inventory'
import type { Translate } from '@/i18n/runtime'
import type { ModelSelection, SessionEvent } from '@/types'
import css from './composer-context-meter.module.css'

const RADIUS = 5.5
const CIRCUMFERENCE = 2 * Math.PI * RADIUS

function compact(value: number): string {
  return value >= 1_000_000
    ? `${Math.round(value / 100_000) / 10}M`
    : value >= 1_000 ? `${Math.round(value / 100) / 10}K` : String(value)
}

function useContextWindow(selection: ModelSelection, target: ExecutionTarget): number | null {
  const providerKind = selection.provider
  const providerId = selection.provider === 'named_provider' ? selection.provider_id : ''
  const modelId = selection.provider === 'named_provider' ? selection.model : ''
  const targetKey = executionTargetKey(target)
  const subscribe = React.useCallback((listener: () => void) => (
    subscribeProviderInventory(() => listener(), target)
  ), [targetKey])
  const getSnapshot = React.useCallback(() => peekProviderInventory(target), [targetKey])
  const inventory = React.useSyncExternalStore(subscribe, getSnapshot, getSnapshot)
  const cloudEnabled = target.placement === 'cloud' && (providerKind === 'platform_model' || providerKind === 'named_provider')
  const subscribeCloud = React.useCallback((listener: () => void) => cloudEnabled
    ? subscribeCloudModelInventory(listener, target) : () => {}, [cloudEnabled, targetKey])
  const getCloudSnapshot = React.useCallback(() => cloudEnabled ? peekCloudModelInventory(target) : null, [cloudEnabled, targetKey])
  const cloudInventory = React.useSyncExternalStore(subscribeCloud, getCloudSnapshot, getCloudSnapshot)
  React.useEffect(() => {
    if (providerKind === 'named_provider' && !cloudEnabled) void loadProviderInventory(false, target).catch(() => undefined)
  }, [providerKind, cloudEnabled, targetKey])
  React.useEffect(() => {
    if (cloudEnabled) void loadCloudModelInventory(false, target).catch(() => undefined)
  }, [cloudEnabled, targetKey])
  if (cloudEnabled) return currentCloudModel(cloudInventory, selection)?.model?.defaults.context_window ?? null
  if (providerKind !== 'named_provider') return null
  const provider = inventory?.providers.find(item => item.id === providerId)
  const value = resolvedProviderModel(provider, modelId)?.context_window
  return typeof value === 'number' && Number.isFinite(value) && value > 0 ? value : null
}

function latestMeasuredUsage(events: readonly SessionEvent[]): { input: number; output: number } | null {
  for (let index = events.length - 1; index >= 0; index -= 1) {
    const usage = events[index]?.response?.usage
    if (!usage) continue
    if (!Number.isFinite(usage.input_tokens) || !Number.isFinite(usage.output_tokens)) continue
    return { input: usage.input_tokens, output: usage.output_tokens }
  }
  return null
}

export function ComposerContextMeter({
  model,
  events,
  target = {},
  t,
}: {
  model: ModelSelection
  events: readonly SessionEvent[]
  target?: ExecutionTarget
  t: Translate<'conversation'>
}) {
  const contextWindow = useContextWindow(model, target)
  const usage = React.useMemo(() => latestMeasuredUsage(events), [events])
  const [open, setOpen] = React.useState(false)
  const rootRef = React.useRef<HTMLSpanElement>(null)
  const used = usage ? usage.input + usage.output : 0
  const available = contextWindow !== null && usage !== null && used > 0

  React.useEffect(() => {
    if (!open) return
    const pointer = (event: PointerEvent) => {
      if (event.target instanceof Node && rootRef.current?.contains(event.target)) return
      setOpen(false)
    }
    const key = (event: KeyboardEvent) => { if (event.key === 'Escape') setOpen(false) }
    document.addEventListener('pointerdown', pointer)
    document.addEventListener('keydown', key)
    return () => {
      document.removeEventListener('pointerdown', pointer)
      document.removeEventListener('keydown', key)
    }
  }, [open])

  React.useEffect(() => { if (!available) setOpen(false) }, [available])
  if (!available || contextWindow === null || usage === null) return null
  const percent = Math.min(100, Math.max(1, Math.round(used / contextWindow * 100)))
  return (
    <span ref={rootRef} className={css.root}>
      <button
        type="button"
        className={css.trigger}
        aria-label={t('context.aria', { percent })}
        aria-haspopup="dialog"
        aria-expanded={open}
        onClick={() => setOpen(value => !value)}
      >
        <svg viewBox="0 0 14 14" width="14" height="14" aria-hidden="true">
          <circle className={css.track} cx="7" cy="7" r={RADIUS} />
          <circle className={css.fill} data-warning={percent >= 80 || undefined} cx="7" cy="7" r={RADIUS} strokeDasharray={`${CIRCUMFERENCE * percent / 100} ${CIRCUMFERENCE}`} transform="rotate(-90 7 7)" />
        </svg>
      </button>
      {open && (
        <div className={css.panel} role="dialog" aria-label={t('context.used')}>
          <div className={css.header}><strong>{percent}%</strong><span>{t('context.used')}</span><code>{compact(used)} / {compact(contextWindow)}</code></div>
          <div className={css.bar}><span style={{ width: `${percent}%` }} /></div>
          <dl>
            <div><dt>{t('context.input')}</dt><dd>{compact(usage.input)}</dd></div>
            <div><dt>{t('context.output')}</dt><dd>{compact(usage.output)}</dd></div>
            <div><dt>{t('context.window')}</dt><dd>{compact(contextWindow)}</dd></div>
          </dl>
          {percent >= 80 && <p>{t('context.warning')}</p>}
        </div>
      )}
    </span>
  )
}
