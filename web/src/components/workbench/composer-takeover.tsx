import * as React from 'react'
import {
  Check, ChevronDown, ChevronLeft, ChevronRight, ChevronUp, Code2, Pencil, ShieldAlert, SkipForward,
} from 'lucide-react'
import type { Translate } from '@/i18n/runtime'
import { useTranslate } from '@/i18n/provider'
import type { PendingQuestion, UserQuestionAnswer } from '@/types'
import css from './composer-takeover.module.css'
import { isPlanReview, PlanReviewPanel } from './plan-review-panel'
import { AssistantMarkdown } from './chat/assistant-markdown'
import { presentationDisplayValue, presentationValue } from '@/domain/tool-presentation'
import { declarativeToolIcon } from '@/plugins/tool-presentations/declarative-contribution'

type SharedProps = {
  onAnswer(id: string, answer: UserQuestionAnswer): Promise<void>
  onError(message: string): void
  t: Translate<'conversation'>
}

function errorMessage(cause: unknown): string {
  return cause instanceof Error ? cause.message : String(cause)
}

function ToolApproval({ item, disabled, onAnswer, onError, onInspectApproval, t }: {
  disabled: boolean
  item: PendingQuestion
  onInspectApproval?(callId: string): void
} & SharedProps) {
  const [submitting, setSubmitting] = React.useState(false)
  const [details, setDetails] = React.useState(false)
  const approval = item.question.tool_approval!
  const ApprovalIcon = declarativeToolIcon(approval.presentation)
  const allow = item.question.options.find(option => /allow|approve|允许|批准/i.test(option.label)) ?? item.question.options[0]
  const deny = item.question.options.find(option => /deny|reject|拒绝/i.test(option.label)) ?? item.question.options[1]
  const answer = async (value: string | undefined) => {
    if (!value || submitting || disabled) return
    setSubmitting(true)
    try {
      await onAnswer(item.question.id, { selected: [value] })
    } catch (cause) {
      onError(errorMessage(cause))
      setSubmitting(false)
    }
  }
  return (
    <section className={css.approval} data-tool-approval="" aria-labelledby={`approval-${item.question.id}`}>
      <header>
        <span className={css.approvalIcon}>{approval.presentation ? <ApprovalIcon /> : <ShieldAlert />}</span>
        <div>
          <div className={css.eyebrow}>{t('approval.title')}</div>
          <h2 id={`approval-${item.question.id}`}>{approval.presentation?.title ?? approval.tool_name}</h2>
        </div>
      </header>
      <p>{approval.reason || item.question.question}</p>
      <button
        type="button"
        className={css.detailButton}
        aria-expanded={onInspectApproval ? undefined : details}
        onClick={() => onInspectApproval ? onInspectApproval(approval.call_id) : setDetails(value => !value)}
      >
        <Code2 />
        {t('approval.details')}
        {details ? <ChevronUp /> : <ChevronDown />}
      </button>
      {details && (
        <div className={css.approvalDetails}>
          <span>{t('approval.callId')}</span><code>{approval.call_id}</code>
          {(approval.presentation?.input_summary ?? []).map((field, index) => <React.Fragment key={`${field.label}-${index}`}>
            <span>{field.label}</span><code>{presentationDisplayValue(presentationValue(approval.arguments, field.path)) || '—'}</code>
          </React.Fragment>)}
          <pre>{JSON.stringify(approval.arguments, null, 2)}</pre>
        </div>
      )}
      {disabled && <p role="note">{t('question.permissionDenied')}</p>}
      <footer className={css.approvalActions}>
        <button type="button" className={css.deny} disabled={disabled || submitting || !deny} onClick={() => void answer(deny?.label)}>{t('approval.deny')}</button>
        <button type="button" className={css.allow} disabled={disabled || submitting || !allow} onClick={() => void answer(allow?.label)}><Check />{t('approval.allow')}</button>
      </footer>
    </section>
  )
}

function QuestionFlow({ items, disabled, onAnswer, onError, t }: { items: PendingQuestion[]; disabled: boolean } & SharedProps) {
  const [index, setIndex] = React.useState(0)
  const [drafts, setDrafts] = React.useState<Record<string, QuestionDraft>>({})
  const [submitting, setSubmitting] = React.useState(false)
  const [minimized, setMinimized] = React.useState(false)
  const [feedback, setFeedback] = React.useState('')
  const chatT = useTranslate('chat')
  const safeIndex = Math.min(index, Math.max(0, items.length - 1))
  const item = items[safeIndex]!
  const question = item.question
  const draft = drafts[question.id] ?? EMPTY_DRAFT

  React.useEffect(() => {
    if (index >= items.length) setIndex(Math.max(0, items.length - 1))
  }, [index, items.length])

  const replaceDraft = (questionId: string, value: QuestionDraft) => {
    setDrafts(current => ({ ...current, [questionId]: value }))
    setFeedback('')
  }

  const answered = (value: QuestionDraft) => value.selected.length > 0 || value.custom.trim() !== ''
  const completed = (value: QuestionDraft) => answered(value) || value.skipped

  const submit = async (nextDrafts = drafts) => {
    if (submitting || disabled) return
    const missing = items.findIndex(candidate => !completed(nextDrafts[candidate.question.id] ?? EMPTY_DRAFT))
    if (missing >= 0) {
      setIndex(missing)
      setFeedback(t('question.incomplete'))
      return
    }
    setSubmitting(true)
    try {
      await Promise.all(items.map(candidate => {
        const value = nextDrafts[candidate.question.id] ?? EMPTY_DRAFT
        const custom = value.custom.trim()
        return onAnswer(candidate.question.id, {
          selected: value.skipped ? [] : value.selected,
          ...(value.skipped || custom === '' ? {} : { custom }),
        })
      }))
    } catch (cause) {
      onError(errorMessage(cause))
      setSubmitting(false)
    }
  }

  const choose = (label: string) => {
    if (disabled) return
    if (question.multi_select) {
      setDrafts(current => {
        const currentDraft = current[question.id] ?? EMPTY_DRAFT
        const selected = currentDraft.selected.includes(label)
          ? currentDraft.selected.filter(value => value !== label)
          : [...currentDraft.selected, label]
        return { ...current, [question.id]: { ...currentDraft, selected, skipped: false } }
      })
      setFeedback('')
      return
    }
    replaceDraft(question.id, { selected: [label], custom: '', skipped: false })
    if (safeIndex < items.length - 1) setIndex(safeIndex + 1)
  }

  const continueFlow = () => {
    if (disabled) return
    if (!answered(draft)) {
      setFeedback(t('question.unanswered'))
      return
    }
    if (safeIndex < items.length - 1) {
      setIndex(safeIndex + 1)
      setFeedback('')
      return
    }
    void submit()
  }

  const skip = () => {
    if (disabled) return
    const nextDrafts = {
      ...drafts,
      [question.id]: { selected: [], custom: '', skipped: true },
    }
    setDrafts(nextDrafts)
    setFeedback('')
    if (safeIndex < items.length - 1) setIndex(safeIndex + 1)
    else void submit(nextDrafts)
  }

  return (
    <section className={`${css.question} ${minimized ? css.minimized : ''}`} data-question-takeover="" aria-labelledby={`question-${question.id}`}>
      <header>
        <div className={css.heading}>
          <div className={css.eyebrow}>{question.header || t('question.waiting')}</div>
          <h2 id={`question-${question.id}`} data-question-heading="">{question.question}</h2>
        </div>
        <button type="button" className={css.iconButton} aria-label={minimized ? t('question.expand') : t('question.collapse')} onClick={() => setMinimized(value => !value)}>
          {minimized ? <ChevronUp /> : <ChevronDown />}
        </button>
      </header>
      {!minimized && (
        <>
          <div className={css.questionBody} data-question-body="">
            {question.detail && <div className={css.questionDetail}><AssistantMarkdown source={question.detail} streaming={false} t={chatT} /></div>}
            {question.options.length > 0 && (
              <div className={css.options} role={question.multi_select ? 'group' : 'radiogroup'} aria-label={question.question}>
                {question.options.map((option, optionIndex) => {
                  const selected = draft.selected.includes(option.label)
                  const display = parseRecommendedLabel(option.label)
                  return <button
                    type="button"
                    data-question-option=""
                    role={question.multi_select ? 'checkbox' : 'radio'}
                    aria-checked={selected}
                    aria-label={display.label}
                    key={`${option.label}-${optionIndex}`}
                    className={selected ? css.selectedOption : ''}
                    disabled={submitting || disabled}
                    onClick={() => choose(option.label)}
                  >
                    <span className={css.optionMark} data-question-option-mark="" aria-hidden="true">{question.multi_select ? selected ? <Check /> : null : optionIndex + 1}</span>
                    <span className={css.optionCopy} data-question-option-copy="">
                      <span className={css.optionLabel} data-question-option-label="">{display.label}{display.recommended && <em>{t('question.recommended')}</em>}</span>
                      {option.description && <small>{option.description}</small>}
                    </span>
                  </button>
                })}
              </div>
            )}
            <div className={css.customAnswer}>
              {question.options.length > 0 && <Pencil aria-hidden="true" />}
              <textarea
                rows={1}
                value={draft.custom}
                disabled={submitting || disabled}
                placeholder={question.options.length > 0 ? t('question.other') : t('question.input')}
                aria-label={t('question.input')}
                onChange={event => replaceDraft(question.id, {
                  ...draft,
                  selected: question.multi_select ? draft.selected : [],
                  custom: event.target.value,
                  skipped: false,
                })}
                onKeyDown={event => {
                  if (event.key !== 'Enter' || event.shiftKey || event.nativeEvent.isComposing || event.nativeEvent.keyCode === 229) return
                  event.preventDefault()
                  continueFlow()
                }}
              />
            </div>
          </div>
          <footer className={css.questionFooter}>
            <div className={css.pager}>
              <button type="button" aria-label={t('question.previous')} disabled={safeIndex === 0 || submitting} onClick={() => { setIndex(value => value - 1); setFeedback('') }}><ChevronLeft /></button>
              <span>{safeIndex + 1} / {items.length}</span>
              <button type="button" aria-label={t('question.next')} disabled={safeIndex === items.length - 1 || submitting} onClick={() => { setIndex(value => value + 1); setFeedback('') }}><ChevronRight /></button>
            </div>
            <span className={css.feedback} role="status">{disabled ? t('question.permissionDenied') : feedback}</span>
            <div className={css.questionActions}>
              <button type="button" className={css.skip} disabled={submitting || disabled} onClick={skip}><SkipForward />{t('question.skip')}</button>
              <button type="button" className={css.continue} disabled={disabled || submitting || !answered(draft)} onClick={continueFlow}>
                {safeIndex === items.length - 1 ? t('question.submit') : t('question.next')}
              </button>
            </div>
          </footer>
        </>
      )}
    </section>
  )
}

type QuestionDraft = { selected: string[]; custom: string; skipped: boolean }
const EMPTY_DRAFT: QuestionDraft = { selected: [], custom: '', skipped: false }

export function parseRecommendedLabel(label: string) {
  const suffix = /\s*(?:\((?:recommended|\u63a8\u8350)\)|\uFF08(?:recommended|\u63a8\u8350)\uFF09)\s*$/i
  return suffix.test(label)
    ? { label: label.replace(suffix, ''), recommended: true }
    : { label, recommended: false }
}

export function ComposerTakeover({ questions, canAnswer = () => true, onAnswer, onError, onInspectApproval, t }: {
  questions: PendingQuestion[]
  canAnswer?(item: PendingQuestion): boolean
  onInspectApproval?(callId: string): void
} & SharedProps) {
  const approvals = questions.filter(item => item.question.tool_approval)
  const planReviews = questions.filter(isPlanReview)
  const ordinary = questions.filter(item => !item.question.tool_approval && !isPlanReview(item))
  return (
    <>
      {approvals.map(item => <ToolApproval key={item.question.id} item={item} disabled={!canAnswer(item)} onAnswer={onAnswer} onError={onError} onInspectApproval={onInspectApproval} t={t} />)}
      {planReviews.map(item => <PlanReviewPanel key={item.question.id} item={item} disabled={!canAnswer(item)} onAnswer={onAnswer} t={t} />)}
      {ordinary.length > 0 && <QuestionFlow items={ordinary} disabled={!ordinary.every(canAnswer)} onAnswer={onAnswer} onError={onError} t={t} />}
    </>
  )
}
