import { FileText, Folder, MessageSquareText } from 'lucide-react'
import type { SubmissionReference } from '@/types'
import { cn } from '@/lib/utils'
import css from './reference-chips.module.css'

export function SubmissionReferenceChips({
  references,
  align = 'start',
  compact = false,
}: {
  references: readonly SubmissionReference[]
  align?: 'start' | 'end'
  compact?: boolean
}) {
  if (!references.length) return null
  return (
    <div
      className={cn(css.root, align === 'end' && css.end, compact && css.compact)}
      data-submission-references=""
    >
      {references.map((reference, index) => {
        const file = reference.kind === 'file'
        const directory = file && reference.file_kind === 'directory'
        const Icon = file ? directory ? Folder : FileText : MessageSquareText
        const label = file ? reference.path : reference.label
        return (
          <span
            key={`${reference.kind}:${file ? reference.path : reference.session_id}:${index}`}
            className={css.chip}
            data-reference-kind={reference.kind}
            title={label}
          >
            <Icon aria-hidden />
            <span>{label}</span>
          </span>
        )
      })}
    </div>
  )
}
