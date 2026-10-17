import { MarkdownWorkspaceImages } from './markdown-workspace-images'
import { copyText } from '@/lib/clipboard'
import * as React from 'react'
import { renderMarkdown, renderStreamingMarkdown } from '../../../../rich-text.source.js'
import type { Translate } from '@/i18n/runtime'
import css from './assistant-markdown.module.css'
import { openStreamingFence, StreamingCodeBlock } from './streaming-code-block'
import { WorkspaceDisplayContext } from '../workspace-display-context'
import { displayWorkspacePath } from '@/domain/workspace-markdown'

type ChatTranslate = Translate<'chat'>

export function AssistantMarkdown({ source, streaming, interrupted, t }: {
  source: string
  streaming: boolean
  interrupted?: boolean
  t: ChatTranslate
}) {
  const workspacePath = React.useContext(WorkspaceDisplayContext)
  const bodyRef = React.useRef<HTMLDivElement>(null)
  const labels = React.useMemo(() => ({
      copy: t('message.copy'),
      copyCode: t('message.copyCode'),
      taskCompleted: t('message.taskCompleted'),
      taskPending: t('message.taskPending'),
  }), [t])
  const openFence = React.useMemo(() => streaming ? openStreamingFence(source) : null, [source, streaming])
  const html = React.useMemo(() => openFence ? ''
    : streaming ? renderStreamingMarkdown(source, labels) : renderMarkdown(source, labels),
  [source, streaming, labels, openFence])
  const prefixHtml = React.useMemo(() => openFence?.prefix ? renderMarkdown(openFence.prefix, labels) : '', [openFence?.prefix, labels])
  const bodyHtml = React.useMemo(() => ({ __html: displayWorkspacePath(html, workspacePath) }), [html, workspacePath])
  const prefixBodyHtml = React.useMemo(() => ({ __html: displayWorkspacePath(prefixHtml, workspacePath) }), [prefixHtml, workspacePath])
  const copiedTimer = React.useRef<number | null>(null)
  React.useEffect(() => () => {
    if (copiedTimer.current !== null) window.clearTimeout(copiedTimer.current)
  }, [])

  if (!source && !interrupted) return null
  return <div ref={bodyRef} className={css.root} data-streaming={streaming || undefined}>
    {source && <div
      className={css.body}
      {...(openFence ? {} : { dangerouslySetInnerHTML: bodyHtml })}
      onClick={event => {
        const button = (event.target as HTMLElement).closest<HTMLButtonElement>('.markdown-copy')
        if (!button) return
        const code = button.closest('.markdown-code')?.querySelector('code')?.textContent ?? ''
        const showCopyResult = (text: string) => {
          button.textContent = text
          if (copiedTimer.current !== null) window.clearTimeout(copiedTimer.current)
          copiedTimer.current = window.setTimeout(() => {
            button.textContent = t('message.copy')
            button.setAttribute('aria-label', t('message.copyCode'))
            copiedTimer.current = null
          }, 1_000)
        }
        void copyText(code).then(() => {
          button.setAttribute('aria-label', t('message.copied'))
          showCopyResult(t('message.copied'))
        }).catch(() => {
          button.setAttribute('aria-label', t('message.copyFailed'))
          showCopyResult(t('message.copyFailed'))
        })
      }}
    >{openFence && <>
      {prefixHtml && <div dangerouslySetInnerHTML={prefixBodyHtml} />}
      <div className="markdown-body markdown-streaming">
        <StreamingCodeBlock code={openFence.code} lang={openFence.lang} copyLabel={t('message.copy')} copyAria={t('message.copyCode')} />
      </div>
    </>}</div>}
    <MarkdownWorkspaceImages container={bodyRef} markup={bodyHtml.__html + prefixBodyHtml.__html} t={t} />
    {interrupted && <span className={css.stopped}>{t('message.stopped')}</span>}
  </div>
}
