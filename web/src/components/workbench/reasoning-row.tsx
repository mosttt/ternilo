import * as React from 'react'
import { Brain } from 'lucide-react'
import type { AssistantReasoning } from '@/domain/events'
import { useTranslate } from '@/i18n/provider'
import { formatDuration } from '@/lib/utils'
import { ChatDisclosure, DisclosureSeparator } from './chat/chat-disclosure'
import css from './reasoning-row.module.css'

function firstLine(text: string) {
  const newline = text.indexOf('\n')
  return newline === -1 ? text : text.slice(0, newline)
}

function latestLine(text: string) {
  const visible = text.trimEnd()
  const newline = visible.lastIndexOf('\n')
  return newline === -1 ? visible : visible.slice(newline + 1)
}

export interface ReasoningDisclosure {
  open: boolean
  onToggle(): void
}

export function ReasoningRow({ reasoning, disclosure }: { reasoning: AssistantReasoning; disclosure?: ReasoningDisclosure }) {
  const t = useTranslate('chat')
  const [open, setOpen] = React.useState(false)
  const [now, setNow] = React.useState(Date.now())
  const summaryRef = React.useRef<HTMLSpanElement>(null)
  const summary = reasoning.running ? latestLine(reasoning.text) : firstLine(reasoning.text)
  const duration = reasoning.running ? Math.max(0, now - reasoning.startedAt) : reasoning.durationMs

  React.useEffect(() => {
    if (!reasoning.running) return
    const timer = window.setInterval(() => setNow(Date.now()), 100)
    return () => window.clearInterval(timer)
  }, [reasoning.running, reasoning.startedAt])

  React.useLayoutEffect(() => {
    const element = summaryRef.current
    if (!element) return
    element.scrollLeft = reasoning.running ? element.scrollWidth - element.clientWidth : 0
  }, [reasoning.running, summary])

  return <div className={css.root} data-reasoning-row="" data-state={reasoning.running ? 'running' : 'complete'}>
    {reasoning.running && <span className={css.visuallyHidden}>{t('row.running')}</span>}
    <ChatDisclosure
      icon={<Brain />}
      title={t('message.think')}
      summary={<><DisclosureSeparator /><span ref={summaryRef} className={css.summary} data-reasoning-summary="" data-follow-end={reasoning.running || undefined}>{summary}</span>{duration != null && <span className={css.duration} data-reasoning-duration="">{formatDuration(duration)}</span>}</>}
      open={disclosure?.open ?? open}
      onToggle={disclosure?.onToggle ?? (() => setOpen(value => !value))}
      rowClassName={css.row}
    >
      <div className={css.body} data-reasoning-body="">{reasoning.text}</div>
    </ChatDisclosure>
  </div>
}
