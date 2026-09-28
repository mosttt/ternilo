import type { ChatTurn } from '@/domain/chat-turns'
import type { Translate } from '@/i18n/runtime'
import type { SessionProjection } from '@/types'
import { formatDuration } from '@/lib/utils'
import { AssistantMessageActions } from '../message-actions'
import { ProducedFiles } from '../produced-files'
import { TurnUsage } from './turn-usage'
import css from './turn-tail.module.css'

export function TurnTail({ turn, sessionId, projection, reloadMetadata, showActions, t }: {
  turn: ChatTurn
  sessionId: string
  projection: SessionProjection | null
  reloadMetadata(): Promise<void>
  showActions: boolean
  t: Translate<'chat'>
}) {
  const answer = turn.finalAnswerKey == null ? undefined : turn.items.find(item => item.key === turn.finalAnswerKey)
  const metrics = turn.metrics
  const hasMetrics = metrics.durationMs != null || metrics.ttftMs != null || metrics.tokensPerSecond != null
  if (!showActions && !turn.usage && !hasMetrics && !turn.deliverables.length) return null
  return <div className={css.root} data-turn-tail={turn.number}>
    {showActions && answer?.kind === 'assistant' && answer.content && <AssistantMessageActions event={answer.event} content={answer.content} projection={projection} reloadMetadata={reloadMetadata} />}
    <ProducedFiles sessionId={sessionId} deliverables={turn.deliverables} />
    {(turn.usage || hasMetrics) && <footer className={css.footer}>
      {turn.usage ? <TurnUsage usage={turn.usage} t={t} /> : <span />}
      <div className={css.metrics} data-turn-metrics="" aria-label={t('message.turnMetrics', { turn: turn.number })}>
        {metrics.durationMs != null && <span>{t('message.ranFor', { duration: formatDuration(metrics.durationMs) })}</span>}
        {metrics.ttftMs != null && <span>{t('message.ttft', { seconds: Math.round(metrics.ttftMs / 100) / 10 })}</span>}
        {metrics.tokensPerSecond != null && <span>{t('message.tokensPerSecond', { tps: Math.round(metrics.tokensPerSecond * 10) / 10 })}</span>}
      </div>
    </footer>}
  </div>
}
