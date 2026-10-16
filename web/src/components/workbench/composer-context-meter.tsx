import * as React from 'react'
import { Popover } from 'radix-ui'
import { executionTargetKey, type ExecutionTarget } from '@/domain/execution-target'
import {
  loadProviderInventory, peekProviderInventory, subscribeProviderInventory,
} from '@/domain/provider-inventory'
import { resolvedProviderModel } from '@/domain/provider-model'
import { currentCloudModel, loadCloudModelInventory, peekCloudModelInventory, subscribeCloudModelInventory } from '@/domain/cloud-model-inventory'
import type { Translate } from '@/i18n/runtime'
import type { ModelContextUsage, ModelSelection, SessionEvent } from '@/types'
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
  const cloudEnabled = providerKind === 'platform_model' || providerKind === 'account_provider' || providerKind === 'computer_provider' || (target.placement === 'cloud' && providerKind === 'named_provider')
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

function latestMeasuredUsage(events: readonly SessionEvent[]): { input: number; output: number; cached: number; reasoning: number } | null {
  for (let index = events.length - 1; index >= 0; index -= 1) {
    const usage = events[index]?.response?.usage
    if (!usage) continue
    if (!Number.isFinite(usage.input_tokens) || !Number.isFinite(usage.output_tokens)) continue
    return { input: usage.input_tokens, output: usage.output_tokens, cached: usage.cached_input_tokens ?? 0, reasoning: usage.reasoning_tokens ?? 0 }
  }
  return null
}

const contextParts = [
  ['system_chars', 'context.system'],
  ['tool_schema_chars', 'context.tools'],
  ['conversation_chars', 'context.messages'],
  ['tool_output_chars', 'context.toolResults'],
  ['additional_state_chars', 'context.additionalState'],
] as const

function latestContextUsage(events: readonly SessionEvent[]): ModelContextUsage | null {
  const request = events.findLast(event => event.type === 'model_request_started')
  const usage = request?.context_usage
  if (!usage || ![...contextParts.map(([key]) => usage[key]), usage.unmeasured_attachments]
    .every(value => Number.isSafeInteger(value) && value >= 0)) return null
  return usage
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
  const contextUsage = React.useMemo(() => latestContextUsage(events), [events])
  const [open, setOpen] = React.useState(false)
  const used = usage ? usage.input + usage.output : 0
  const measured = contextWindow !== null && usage !== null && used > 0
  const available = measured || contextUsage !== null
  React.useEffect(() => { if (!available) setOpen(false) }, [available])
  if (!available) return null
  const ratio = measured ? Math.min(100, used / contextWindow * 100) : 0
  const percent = Math.round(ratio)
  const percentLabel = ratio < 1 ? '<1' : String(percent)
  const textTotal = contextUsage ? contextParts.reduce((total, [key]) => total + contextUsage[key], 0) : 0
  return (
    <Popover.Root open={open} onOpenChange={setOpen}>
      <span className={css.root}>
        <Popover.Trigger asChild><button type="button" className={css.trigger} data-context-meter
          aria-label={measured ? t('context.aria', { percent: percentLabel }) : t('context.details')}>
          <svg viewBox="0 0 14 14" width="14" height="14" aria-hidden="true">
            <circle className={css.track} cx="7" cy="7" r={RADIUS} />
            <circle className={css.fill} data-warning={percent >= 80 || undefined} cx="7" cy="7" r={RADIUS}
              strokeDasharray={`${CIRCUMFERENCE * ratio / 100} ${CIRCUMFERENCE}`} transform="rotate(-90 7 7)" />
          </svg>
        </button></Popover.Trigger>
      </span>
      <Popover.Portal><Popover.Content className={css.panel} side="top" align="end" sideOffset={8} collisionPadding={12} aria-label={t('context.details')}>
        {measured && <>
          <div className={css.header}><strong>{percentLabel}%</strong><span>{t('context.used')}</span><code>{compact(used)} / {compact(contextWindow)}</code></div>
          <div className={css.bar}><span style={{ width: `${ratio}%` }} /></div>
          <dl>
            <div><dt>{t('context.input')}</dt><dd>{compact(usage.input)}</dd></div>
            <div><dt>{t('context.output')}</dt><dd>{compact(usage.output)}</dd></div>
            <div><dt>{t('context.cached')}</dt><dd>{compact(usage.cached)}</dd></div>
            <div><dt>{t('context.reasoning')}</dt><dd>{compact(usage.reasoning)}</dd></div>
            <div><dt>{t('context.window')}</dt><dd>{compact(contextWindow)}</dd></div>
          </dl>
          <p className={css.note}>{t('context.measurement')}</p>
        </>}
        {contextUsage && <section className={css.composition} data-context-composition>
          <div className={css.header}><strong>{t('context.composition')}</strong><code>{textTotal.toLocaleString()}</code></div>
          <div className={css.compositionBar} aria-hidden="true">
            {contextParts.map(([key], index) => <span key={key} data-part={index} style={{ width: `${textTotal ? contextUsage[key] / textTotal * 100 : 0}%` }} />)}
          </div>
          <dl>{contextParts.map(([key, label], index) => <div key={key} data-context-part={key}>
            <dt><span className={css.swatch} data-part={index} />{t(label)}</dt><dd>{contextUsage[key].toLocaleString()}</dd>
          </div>)}</dl>
          <p className={css.note}>{t('context.compositionMeasurement')}</p>
          {contextUsage.unmeasured_attachments > 0 && <p className={css.note}>{t('context.unmeasuredAttachments', { count: contextUsage.unmeasured_attachments })}</p>}
        </section>}
        {percent >= 80 && <p className={css.warning}>{t('context.warning')}</p>}
      </Popover.Content></Popover.Portal>
    </Popover.Root>
  )
}
