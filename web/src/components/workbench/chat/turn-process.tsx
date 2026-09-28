import { ChevronRight } from 'lucide-react'
import type { ChatTurnProcessCounts } from '@/domain/chat-turns'
import type { Translate } from '@/i18n/runtime'
import css from './turn-process.module.css'

export function TurnProcessControl({ turn, counts, open, onToggle, t }: {
  turn: number
  counts: ChatTurnProcessCounts
  open: boolean
  onToggle(): void
  t: Translate<'chat'>
}) {
  const label = [
    counts.tools ? t('message.turnProcess.toolCalls.other', { count: counts.tools }) : '',
    counts.messages ? t('message.turnProcess.messages.other', { count: counts.messages }) : '',
    counts.subagents ? t('message.turnProcess.subagents.other', { count: counts.subagents }) : '',
  ].filter(Boolean).join(t('message.turnProcess.separator')) || t('message.turnProcess.thoughtForAWhile')
  return <button
    type="button"
    className={css.control}
    data-turn-process={turn}
    data-turn-process-messages={counts.messages}
    data-turn-process-tool-calls={counts.tools}
    data-turn-process-subagents={counts.subagents}
    aria-expanded={open}
    onClick={event => { event.currentTarget.focus(); onToggle() }}
  >
    <span>{label}</span><ChevronRight data-open={open || undefined} aria-hidden />
  </button>
}
