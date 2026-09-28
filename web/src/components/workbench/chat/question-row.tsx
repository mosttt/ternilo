import { ChevronRight, CircleDot, ListChecks, ShieldCheck } from 'lucide-react'
import type { QuestionLifecycle } from '@/domain/events'
import type { Translate } from '@/i18n/runtime'
import { declarativeToolIcon } from '@/plugins/tool-presentations/declarative-contribution'
import css from './question-row.module.css'

type ChatTranslate = Translate<'chat'>

function title(lifecycle: QuestionLifecycle, t: ChatTranslate) {
  if (lifecycle.kind === 'tool-approval') return lifecycle.question.tool_approval?.presentation?.title ?? t('question.approvalTitle')
  if (lifecycle.kind === 'plan-review') return t('question.planReviewTitle')
  return t('question.title')
}

function prompt(lifecycle: QuestionLifecycle, t: ChatTranslate) {
  if (lifecycle.kind === 'tool-approval') {
    const approval = lifecycle.question.tool_approval
    return t('question.approvalPrompt', { tool: approval?.presentation?.title ?? approval?.tool_name ?? '' })
  }
  if (lifecycle.kind === 'plan-review') {
    return t('question.planReviewPrompt', { title: lifecycle.question.presentation?.title ?? '' })
  }
  return lifecycle.question.question
}

function optionLabel(lifecycle: QuestionLifecycle, value: string, index: number, t: ChatTranslate) {
  if (lifecycle.kind === 'tool-approval') {
    if (index === 0) return t('question.allowOnce')
    if (index === 1) return t('question.deny')
  }
  if (lifecycle.kind === 'plan-review') {
    if (index === 0 || value === lifecycle.question.presentation?.approve_label) return t('question.approvePlan')
    if (index === 1) return t('question.keepPlanning')
  }
  return value
}

function answerLabel(lifecycle: QuestionLifecycle, t: ChatTranslate) {
  const answer = lifecycle.answer
  if (!answer) return ''
  const selected = answer.selected.map(value => {
    const optionIndex = lifecycle.question.options.findIndex(option => option.label === value)
    return optionIndex >= 0 ? optionLabel(lifecycle, value, optionIndex, t) : value
  })
  if (answer.custom === 'Unavailable') return t('question.unavailable')
  if (answer.custom) selected.push(answer.custom)
  return selected.length > 0 ? selected.join(', ') : t('question.skipped')
}

function stateLabel(lifecycle: QuestionLifecycle, t: ChatTranslate) {
  if (lifecycle.state === 'pending') return t('question.pending')
  if (lifecycle.state === 'answered') return t('question.answered')
  return t('question.interrupted')
}

function interruptionDetail(lifecycle: QuestionLifecycle, t: ChatTranslate) {
  if (lifecycle.terminal?.type === 'turn_cancelled') return t('question.interruptedCancelled')
  if (lifecycle.terminal?.type === 'turn_failed') return t('question.interruptedFailed')
  return t('question.interruptedFinished')
}

export function QuestionRow({ lifecycle, onSelect, t }: {
  lifecycle: QuestionLifecycle
  onSelect(): void
  t: ChatTranslate
}) {
  const Icon = lifecycle.kind === 'tool-approval'
    ? lifecycle.question.tool_approval?.presentation
      ? declarativeToolIcon(lifecycle.question.tool_approval.presentation)
      : ShieldCheck
    : lifecycle.kind === 'plan-review' ? ListChecks : CircleDot
  const approvalReason = lifecycle.question.tool_approval?.reason
  return <section
    className={css.root}
    data-question-lifecycle=""
    data-question-id={lifecycle.id}
    data-question-kind={lifecycle.kind}
    data-state={lifecycle.state}
  >
    <button type="button" className={css.header} onClick={onSelect} aria-label={t('question.inspect', { title: title(lifecycle, t) })}>
      <Icon aria-hidden />
      <strong>{title(lifecycle, t)}</strong>
      <span className={css.state} data-question-state-label="">{stateLabel(lifecycle, t)}</span>
      <ChevronRight aria-hidden />
    </button>
    <div className={css.body}>
      <p className={css.prompt}>{prompt(lifecycle, t)}</p>
      {approvalReason && <p className={css.reason}>{t('question.reason', { reason: approvalReason })}</p>}
      {lifecycle.question.options.length > 0 && <div className={css.optionGroup}>
        <span className={css.label}>{t('question.options')}</span>
        <ul className={css.options} data-question-options="">
          {lifecycle.question.options.map((option, index) => <li key={`${option.label}-${index}`}>
            {optionLabel(lifecycle, option.label, index, t)}
            {option.description && <small>{option.description}</small>}
          </li>)}
        </ul>
      </div>}
      {lifecycle.state === 'answered' && <p className={css.verdict} data-question-answer="">
        <strong>{t('question.answer')}</strong><span>{answerLabel(lifecycle, t) || t('question.skipped')}</span>
      </p>}
      {lifecycle.state === 'interrupted' && <p className={css.interrupted} data-question-interruption="">{interruptionDetail(lifecycle, t)}</p>}
    </div>
  </section>
}
