import { ChevronRight, Clock3 } from 'lucide-react'
import type { ChatTurnUsage } from '@/domain/chat-turns'
import type { Translate } from '@/i18n/runtime'
import css from './turn-usage.module.css'

function compact(value: number) {
  if (value < 1_000) return String(value)
  if (value < 1_000_000) return `${Math.round(value / 100) / 10}K`
  return `${Math.round(value / 100_000) / 10}M`
}

export function TurnUsage({ usage, t }: { usage: ChatTurnUsage; t: Translate<'chat'> }) {
  const input = usage.uncachedInputTokens + (usage.cacheReadTokens ?? 0) + (usage.cacheWriteTokens ?? 0)
  const hit = input > 0 && usage.cacheReadTokens !== undefined
    ? Math.round(usage.cacheReadTokens / input * 1_000) / 10
    : null
  const summary = hit == null
    ? `${compact(usage.totalTokens)} tok`
    : t('message.turnUsage.summaryWithCache', { total: `${compact(usage.totalTokens)} tok`, percent: hit })
  return <details className={css.root} data-turn-usage="">
    <summary><Clock3 /><span>{t('message.turnUsage.title')}</span><span className={css.summary}>{summary}</span><ChevronRight /></summary>
    <dl className={css.details} data-turn-usage-details="">
      {usage.routes?.length ? <><dt>{t('message.turnUsage.model')}</dt><dd>{usage.routes.map(route => `${route.provider} / ${route.model}`).join(', ')}</dd></> : null}
      <dt>{t('message.turnUsage.input')}</dt><dd>{t('message.turnUsage.count', { count: usage.uncachedInputTokens.toLocaleString() })}</dd>
      {usage.cacheReadTokens !== undefined && <><dt>{t('message.turnUsage.cacheRead')}</dt><dd>{t('message.turnUsage.count', { count: usage.cacheReadTokens.toLocaleString() })}</dd></>}
      {usage.cacheWriteTokens !== undefined && <><dt>{t('message.turnUsage.cacheWrite')}</dt><dd>{t('message.turnUsage.count', { count: usage.cacheWriteTokens.toLocaleString() })}</dd></>}
      <dt>{t('message.turnUsage.output')}</dt><dd>{t('message.turnUsage.count', { count: usage.outputTokens.toLocaleString() })}{usage.reasoningTokens !== undefined ? t('message.turnUsage.reasoning', { tokens: usage.reasoningTokens.toLocaleString() }) : ''}</dd>
      <dt>{t('message.turnUsage.total')}</dt><dd>{t('message.turnUsage.count', { count: usage.totalTokens.toLocaleString() })}</dd>
    </dl>
  </details>
}
