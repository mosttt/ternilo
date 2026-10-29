import { ArrowUp, Check, LoaderCircle, X } from 'lucide-react'
import { useTranslate } from '@/i18n/provider'

type Phase = 'preparing' | 'downloading' | 'waiting_idle' | 'installing' | 'awaiting_reconnect' | 'completed' | 'failed' | 'rolled_back' | 'cancelled'
type Progress = { id: string; version: string; phase: Phase; revision: number; error: string | null }
export type UpdateStatus = {
  connected: boolean
  execution_connected?: boolean
  execution_protocol_version?: number | null
  server_execution_protocol_version?: number
  target_version: string
  availability: 'available' | 'current' | 'newer' | 'offline' | 'not_enabled' | 'unsupported' | 'manual_upgrade'
  info: { version: string; target: string | null; enabled: boolean; restart_method: string | null; progress: Progress | null } | null
  job: Progress | null
}
const availabilityKeys = {
  manual_upgrade: 'updates.availability.manualUpgrade',
  upgrade_required: 'updates.availability.upgradeRequired',
  protocol_mismatch: 'updates.availability.protocolMismatch',
  available: 'updates.availability.available',
  current: 'updates.availability.current',
  newer: 'updates.availability.newer',
  offline: 'updates.availability.offline',
  not_enabled: 'updates.availability.not_enabled',
  unsupported: 'updates.availability.unsupported',
} as const

export function clientUpdateFailureKey(error?: string | null) {
  if (error?.includes('client update blocked by an unobserved execution resource')) return 'updates.failure.unobservedExecution' as const
  if (error?.includes('configured service does not own this Ternilo process')) return 'updates.failure.serviceMismatch' as const
  return null
}

export function ComputerUpdateStatus({ status, error }: { status?: UpdateStatus; error?: string }) {
  const t = useTranslate('settings')
  if (!status && !error) return <span className="inline-flex items-center gap-2 text-sm text-muted-foreground" role="status"><LoaderCircle className="size-4 shrink-0 animate-spin" />{t('updates.checking')}</span>
  const incompatible = status?.execution_protocol_version != null && status.server_execution_protocol_version != null && status.execution_protocol_version !== status.server_execution_protocol_version
  const state = error ? 'error'
    : status!.connected && incompatible && status!.availability === 'available' ? 'upgrade_required'
      : status!.connected && incompatible && ['current', 'newer'].includes(status!.availability) ? 'protocol_mismatch'
        : status!.connected && !incompatible && status!.info?.version === status!.target_version ? 'current' : status!.availability
  const failureKey = clientUpdateFailureKey(error)
  const label = state === 'error' ? failureKey ? t(failureKey) : t('updates.error', { error: error! }) : t(availabilityKeys[state])
  const Icon = state === 'available' || state === 'upgrade_required' ? ArrowUp : state === 'current' ? Check : X
  return <span className="inline-flex flex-wrap items-center gap-x-2 gap-y-1 text-sm text-muted-foreground" data-computer-version-state={state} role="status">
    {status && <span className="break-words">{t('updates.versions', { current: status.info?.version ?? '—', target: status.target_version })}</span>}
    <span title={label} className={`inline-flex items-center gap-1 ${state === 'current' ? 'text-success' : state === 'available' ? 'text-primary' : ''}`}>
      <Icon className="size-4 shrink-0" role="img" aria-label={label} />
      {state !== 'available' && state !== 'current' && <span>{label}</span>}
    </span>
  </span>
}
