import * as React from 'react'
import { FileInput, GitMerge } from 'lucide-react'
import type { Translate } from '@/i18n/runtime'
import type { ReferenceContextCompleteness } from '@/types'
import { ChatDisclosure, DisclosureSeparator } from './chat-disclosure'
import css from './context-injection-row.module.css'

type ChatTranslate = Translate<'chat'>

function ContextBody({ content }: { content: string }) {
  return <pre className={css.body} data-context-body="">{content}</pre>
}

export function SystemPromptRow({ content, step, t }: {
  content: string
  step?: number
  t: ChatTranslate
}) {
  const [open, setOpen] = React.useState(false)
  return <section className={css.root} data-system-prompt-row="" data-system-prompt-step={step ?? ''}>
    <ChatDisclosure
      icon={<FileInput />}
      title={t('message.systemPrompt')}
      open={open}
      onToggle={() => setOpen(value => !value)}
    >
      <div data-system-prompt-body=""><ContextBody content={content} /></div>
    </ChatDisclosure>
  </section>
}

export function ContextInjectionRow({ content, source, dialect, referenceLabel, completeness, t }: {
  content: string
  source: string
  dialect?: string
  referenceLabel?: string
  completeness?: ReferenceContextCompleteness
  t: ChatTranslate
}) {
  const [open, setOpen] = React.useState(false)
  return <section className={css.root} data-context-injection="">
    <ChatDisclosure
      icon={<GitMerge />}
      title={t('message.contextInjection')}
      summary={<>
        <DisclosureSeparator />
        <span className={css.source} data-context-source="">{source}</span>
        {referenceLabel && <><DisclosureSeparator /><span className={css.summary} data-context-reference="">{referenceLabel}</span></>}
        {dialect && <><DisclosureSeparator /><span className={css.summary}>{dialect}</span></>}
        {completeness && <><DisclosureSeparator /><span className={css.summary} data-context-completeness="">
          {t('message.contextCompleteness', {
            retained: completeness.retained_items,
            omitted: completeness.omitted_items,
          })}
          {completeness.truncated ? ` · ${t('message.contextTruncated')}` : ''}
        </span></>}
      </>}
      open={open}
      onToggle={() => setOpen(value => !value)}
    >
      <ContextBody content={content} />
    </ChatDisclosure>
  </section>
}
