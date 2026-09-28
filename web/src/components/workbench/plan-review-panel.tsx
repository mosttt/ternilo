import * as React from 'react'
import { Check, MessageSquareText, Pencil } from 'lucide-react'
import type { Translate } from '@/i18n/runtime'
import type { PendingQuestion, UserQuestionAnswer } from '@/types'
import { useTranslate } from '@/i18n/provider'
import { AssistantMarkdown } from './chat/assistant-markdown'
import css from './plan-review-panel.module.css'

type PlanReviewQuestion = PendingQuestion & {
  question: PendingQuestion['question'] & {
    presentation: {
      kind: 'plan_review'
      title: string
      plan: string
      approve_label: string
    }
  }
}

function errorMessage(cause: unknown): string {
  return cause instanceof Error ? cause.message : String(cause)
}

export function isPlanReview(item: PendingQuestion): item is PlanReviewQuestion {
  return item.question.tool_approval == null && item.question.presentation?.kind === 'plan_review'
}

export function PlanReviewPanel({ item, disabled = false, onAnswer, t }: {
  item: PlanReviewQuestion
  disabled?: boolean
  onAnswer(id: string, answer: UserQuestionAnswer): Promise<void>
  t: Translate<'conversation'>
}) {
  const chatT = useTranslate('chat')
  const { question } = item
  const { presentation } = question
  const reviseLabel = question.options.find(option => option.label !== presentation.approve_label)?.label
  const [submitting, setSubmitting] = React.useState(false)
  const [error, setError] = React.useState('')
  const [discussionOpen, setDiscussionOpen] = React.useState(false)
  const [feedback, setFeedback] = React.useState('')
  const feedbackRef = React.useRef<HTMLTextAreaElement>(null)

  const answer = async (value: UserQuestionAnswer) => {
    if (submitting || disabled) return
    setSubmitting(true)
    setError('')
    try {
      await onAnswer(question.id, value)
    } catch (cause) {
      setError(errorMessage(cause))
      setSubmitting(false)
    }
  }

  const discuss = () => {
    if (disabled) return
    setDiscussionOpen(true)
    setError('')
    requestAnimationFrame(() => feedbackRef.current?.focus())
  }

  const sendFeedback = () => {
    const value = feedback.trim()
    if (value) void answer({ selected: [], custom: value })
  }

  return (
    <section
      className={css.card}
      data-plan-review=""
      aria-label={question.question}
    >
      <header className={css.strip}>
        <span className={css.dot} aria-hidden="true" />
        <strong>{t('plan.header')}</strong>
      </header>

      <div className={css.body} data-plan-review-scroll="" tabIndex={0}>
        <AssistantMarkdown source={presentation.plan} streaming={false} t={chatT} />
      </div>

      <footer className={css.footer}>
        {discussionOpen && (
          <div className={css.discussion}>
            <label htmlFor={`plan-feedback-${question.id}`}>{t('plan.feedbackLabel')}</label>
            <textarea
              ref={feedbackRef}
              id={`plan-feedback-${question.id}`}
              rows={2}
              value={feedback}
              disabled={disabled || submitting}
              placeholder={t('plan.feedbackPlaceholder')}
              onChange={event => setFeedback(event.target.value)}
              onKeyDown={event => {
                if (event.key !== 'Enter' || event.shiftKey || event.nativeEvent.isComposing || event.nativeEvent.keyCode === 229) return
                event.preventDefault()
                sendFeedback()
              }}
            />
            <div className={css.discussionActions}>
              <button type="button" disabled={disabled || submitting} onClick={() => setDiscussionOpen(false)}>{t('plan.cancelFeedback')}</button>
              <button type="button" className={css.feedbackSubmit} disabled={disabled || submitting || !feedback.trim()} onClick={sendFeedback}>{t('plan.sendFeedback')}</button>
            </div>
          </div>
        )}

        <div className={css.feedback} role="status" aria-live="polite">{disabled ? t('question.permissionDenied') : error}</div>
        <div className={css.actions}>
          <button type="button" className={css.discuss} data-plan-review-action="discuss" disabled={disabled || submitting} onClick={discuss}>
            <MessageSquareText aria-hidden="true" />
            {t('plan.discuss')}
          </button>
          {reviseLabel && (
            <button type="button" className={css.revise} data-plan-review-action="revise" title={reviseLabel} disabled={disabled || submitting} onClick={() => void answer({ selected: [reviseLabel] })}>
              <Pencil aria-hidden="true" />
              {t('plan.revise')}
            </button>
          )}
          <button type="button" className={css.approve} data-plan-review-action="approve" title={presentation.approve_label} disabled={disabled || submitting} onClick={() => void answer({ selected: [presentation.approve_label] })}>
            <Check aria-hidden="true" />
            {t('plan.approve')}
          </button>
        </div>
      </footer>
    </section>
  )
}
