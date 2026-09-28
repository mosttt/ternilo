import * as React from 'react'
import { ChevronDown } from 'lucide-react'
import { deriveJobs, type JobView } from '@/domain/observability'
import { useTranslate } from '@/i18n/provider'
import type { SessionEvent } from '@/types'
import css from './job-list-action.module.css'

function live(job: JobView) { return job.status === 'running' }

function duration(ms: number, t: ReturnType<typeof useTranslate<'observability'>>) {
  const total = Math.max(0, Math.floor(ms / 1_000))
  const seconds = total % 60
  const minutes = Math.floor(total / 60) % 60
  const hours = Math.floor(total / 3_600)
  if (hours > 0) return t('duration.hours', { hours, minutes })
  if (minutes > 0) return t('duration.minutes', { minutes, seconds })
  return t('duration.seconds', { seconds })
}

export function JobListAction({ events }: { events: readonly SessionEvent[] }) {
  const t = useTranslate('observability')
  const jobs = React.useMemo(() => deriveJobs(events), [events])
  const liveCount = jobs.filter(live).length
  const [open, setOpen] = React.useState(false)
  const [now, setNow] = React.useState(Date.now())
  const root = React.useRef<HTMLDivElement>(null)
  const trigger = React.useRef<HTMLButtonElement>(null)

  React.useEffect(() => {
    if (!open) return
    const close = (event: PointerEvent) => {
      if (!root.current?.contains(event.target as Node)) setOpen(false)
    }
    document.addEventListener('pointerdown', close)
    return () => document.removeEventListener('pointerdown', close)
  }, [open])

  React.useEffect(() => {
    if (!open || liveCount === 0) return
    setNow(Date.now())
    const timer = window.setInterval(() => setNow(Date.now()), 1_000)
    return () => window.clearInterval(timer)
  }, [liveCount, open])

  React.useEffect(() => {
    if (!jobs.length) setOpen(false)
  }, [jobs.length])

  if (!jobs.length) return null
  const countKey = liveCount > 0
    ? liveCount === 1 ? 'jobs.count.live.one' : 'jobs.count.live.other'
    : jobs.length === 1 ? 'jobs.count.idle.one' : 'jobs.count.idle.other'
  const label = t(countKey, { count: liveCount || jobs.length })
  return <div
    ref={root}
    className={css.root}
    data-job-list-action=""
    onKeyDown={event => {
      if (event.key !== 'Escape' || !open) return
      event.preventDefault()
      setOpen(false)
      trigger.current?.focus()
    }}
  >
    <button
      ref={trigger}
      type="button"
      className={css.trigger}
      aria-expanded={open}
      aria-label={label}
      onClick={() => { setNow(Date.now()); setOpen(value => !value) }}
    >
      {liveCount > 0 && <span className={css.liveDot} aria-hidden="true" />}
      <span>{label}</span>
      <ChevronDown data-open={open || undefined} aria-hidden="true" />
    </button>
    {open && <ul className={css.menu} aria-label={t('jobs.list')}>
      {jobs.map(job => {
        const elapsed = live(job) ? now - job.startedAt : (job.finishedAt ?? job.startedAt) - job.startedAt
        return <li className={live(job) ? css.row : `${css.row} ${css.settled}`} key={job.id} data-job-status={job.status}>
          <span className={css.dot} aria-hidden="true" />
          <span className={css.kind}>{t('jobs.kind')}</span>
          <code className={css.command} title={job.command}>{job.command}</code>
          <span className={css.status} title={job.detail}>{job.detail ?? t(`jobs.status.${job.status}`)}</span>
          <time className={css.duration}>{duration(elapsed, t)}</time>
        </li>
      })}
    </ul>}
  </div>
}
