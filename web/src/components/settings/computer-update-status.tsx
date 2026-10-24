import { ArrowUp, Check, LoaderCircle, X } from 'lucide-react'
import { useTranslate } from '@/i18n/provider'

type Phase = 'preparing' | 'downloading' | 'waiting_idle' | 'installing' | 'awaiting_reconnect' | 'completed' | 'failed' | 'rolled_back' | 'cancelled'
type Progress = { id: string; version: string; phase: Phase; revision: number; error: string | null }
export type UpdateStatus = {
  connected: boolean
  target_version: string
  availability: 'available' | 'current' | 'newer' | 'offline' | 'not_enabled' | 'unsupported'
  info: { version: string; target: string | null; enabled: boolean; restart_method: string | null; progress: Progress | null } | null
  job: Progress | null
}
const availabilityKeys = {
  available: 'updates.availability.available',
  current: 'updates.availability.current',
  newer: 'updates.availability.newer',
  offline: 'updates.availability.offline',
  not_enabled: 'updates.availability.not_enabled',
  unsupported: 'updates.availability.unsupported',
} as const

export function ComputerUpdateStatus({ status, error }: { status?: UpdateStatus; error?: string }) {
  const t = useTranslate('settings')
  if (!status && !error) return <span className="inline-flex items-center gap-2 text-sm text-muted-foreground" role="status"><LoaderCircle className="size-4 shrink-0 animate-spin" />{t('updates.checking')}</span>
  const state = error ? 'error'
    : status!.connected && status!.info?.version === status!.target_version ? 'current'
      : status!.availability
  const label = state === 'error' ? t('updates.error', { error: error! }) : t(availabilityKeys[state])
  const Icon = state === 'available' ? ArrowUp : state === 'current' ? Check : X
  return <span className="inline-flex flex-wrap items-center gap-x-2 gap-y-1 text-sm text-muted-foreground" data-computer-version-state={state} role="status">
    {status && <span className="break-words">{t('updates.versions', { current: status.info?.version ?? '—', target: status.target_version })}</span>}
    <span title={label} className={`inline-flex items-center gap-1 ${state === 'current' ? 'text-success' : state === 'available' ? 'text-primary' : ''}`}>
      <Icon className="size-4 shrink-0" role="img" aria-label={label} />
      {state !== 'available' && state !== 'current' && <span>{label}</span>}
    </span>
  </span>
}
