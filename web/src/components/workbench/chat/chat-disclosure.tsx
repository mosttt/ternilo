import type { ReactNode } from 'react'
import { ChevronRight } from 'lucide-react'
import { cn } from '@/lib/utils'
import css from './chat-disclosure.module.css'

export function DisclosureSeparator() {
  return <span className={css.separator} aria-hidden />
}

export function ChatDisclosure({
  icon,
  title,
  summary,
  open,
  expandable = true,
  onToggle,
  children,
  className,
  rowClassName,
}: {
  icon: ReactNode
  title: ReactNode
  summary?: ReactNode
  open: boolean
  expandable?: boolean
  onToggle(): void
  children?: ReactNode
  className?: string
  rowClassName?: string
}) {
  return <div className={cn(css.root, className)} data-open={open || undefined}>
    <button
      type="button"
      className={cn(css.row, rowClassName)}
      data-disclosure-row=""
      aria-expanded={expandable ? open : undefined}
      disabled={!expandable}
      onClick={onToggle}
    >
      <span className={css.leading} aria-hidden>{icon}</span>
      <span className={css.title}>{title}</span>
      {summary}
      {expandable && <ChevronRight className={css.chevron} aria-hidden />}
    </button>
    {open && children}
  </div>
}
