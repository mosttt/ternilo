import type { UserQuestion } from '@/types'
import type { Translate } from '@/i18n/runtime'
import { useTranslate } from '@/i18n/provider'
import { asRecord, cn } from '@/lib/utils'
import css from './approval-explanation.module.css'

export function ApprovalExplanation({ approval, className, t: suppliedTranslate }: {
  approval: NonNullable<UserQuestion['tool_approval']>
  className?: string
  t?: Translate<'conversation'>
}) {
  const translate = useTranslate('conversation')
  const t = suppliedTranslate ?? translate
  const args = asRecord(approval.arguments)
  const explanation = asRecord(args.approval)
  const fields = [
    { key: 'action', label: t('approval.action') },
    { key: 'reason', label: t('approval.reason') },
    { key: 'impact', label: t('approval.impact') },
  ]
  const structured = fields.every(({ key }) => typeof explanation[key] === 'string' && explanation[key].trim())
  const shell = approval.tool_name === 'shell'
  const reason = approval.reason.trim()
  const technical = reason.startsWith('the command requests access outside the workspace sandbox:')
    || reason === 'the shell call requests one-time access outside the workspace sandbox'
  return <div className={cn(css.root, className)} data-approval-explanation="">
    {shell && structured ? <dl>
      {fields.map(({ key, label }) => <div key={key}><dt>{label}</dt><dd>{String(explanation[key])}</dd></div>)}
    </dl> : <p>{shell && (!reason || technical) ? t('approval.missingExplanation') : reason}</p>}
    {shell && args.full_access === true && <p className={css.scope}>{t('approval.hostAccess')}</p>}
    {shell && typeof args.command === 'string' && <details data-approval-command="">
      <summary>{t('approval.command')}</summary>
      <pre><code>{args.command}</code></pre>
    </details>}
  </div>
}
