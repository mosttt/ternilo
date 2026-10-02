import * as React from 'react'
import { ArrowLeft, ChevronRight, Laptop } from 'lucide-react'
import { api } from '@/api/client'
import { isConnectionProvider } from '@/components/models/model-device-types'
import type { ModelComputer } from '@/components/models/model-sources'
import { Button } from '@/components/ui/button'
import { DropdownMenuItem, DropdownMenuLabel } from '@/components/ui/dropdown-menu'
import { loadProviderInventory, type ProviderInventorySnapshot } from '@/domain/provider-inventory'
import { useTranslate } from '@/i18n/provider'
import type { ModelSelection } from '@/types'
import { ProviderModelItems } from './provider-model-items'

export function ComputerModelItems({ tenantId, executionComputerId, selection, onSelect }: {
  tenantId: string; executionComputerId?: string; selection: ModelSelection; onSelect(model: ModelSelection): void
}) {
  const t = useTranslate('model')
  const [computers, setComputers] = React.useState<ModelComputer[]>([])
  const [computer, setComputer] = React.useState<ModelComputer | null>(null)
  const [inventory, setInventory] = React.useState<ProviderInventorySnapshot | null>(null)
  const [loading, setLoading] = React.useState(true)
  const [error, setError] = React.useState('')
  const [revision, reload] = React.useReducer(value => value + 1, 0)
  React.useEffect(() => {
    const controller = new AbortController()
    setLoading(true); setError(''); setInventory(null)
    const read = computer
      ? loadProviderInventory(true, { tenantId, executorId: computer.executor_id }).then(value => { if (!controller.signal.aborted) setInventory(value) })
      : api.request<ModelComputer[]>('/model-computers', { headers: { 'x-ternilo-tenant': tenantId }, signal: controller.signal })
        .then(value => { if (!controller.signal.aborted) setComputers(value.filter(item => item.can_configure && item.executor_id !== executionComputerId)) })
    void read.catch(cause => { if (!controller.signal.aborted) setError(cause instanceof Error ? cause.message : String(cause)) })
      .finally(() => { if (!controller.signal.aborted) setLoading(false) })
    return () => controller.abort()
  }, [tenantId, executionComputerId, computer?.executor_id, revision])
  return <div data-computer-model-picker="">
    <DropdownMenuLabel>{t('source.computer')}</DropdownMenuLabel>
    {computer && <DropdownMenuItem onSelect={event => { event.preventDefault(); setComputer(null) }}><ArrowLeft />{t('menu.back')} · {computer.name}</DropdownMenuItem>}
    {loading && <div className="px-3 py-2 text-xs text-muted-foreground" role="status">{t('provider.loading')}</div>}
    {error && <div className="px-3 py-2 text-xs text-destructive" role="alert">{error}<Button size="xs" variant="ghost" onClick={() => reload()}>{t('provider.retry')}</Button></div>}
    {!computer && computers.map(item => <DropdownMenuItem key={item.executor_id} disabled={!item.connected} onSelect={event => { event.preventDefault(); setComputer(item) }}>
      <Laptop /><span className="min-w-0 flex-1 truncate">{item.name}</span>{item.connected ? <ChevronRight /> : <span className="text-xs text-muted-foreground">{t('computer.offline')}</span>}
    </DropdownMenuItem>)}
    {!computer && !loading && !error && !computers.length && <p className="px-3 py-2 text-xs text-muted-foreground">{t('computer.empty')}</p>}
    {computer && inventory && <ProviderModelItems providers={inventory.providers.filter(provider => !isConnectionProvider(provider.id))} credentials={inventory.credentials} selection={selection} computerId={computer.executor_id} label={computer.name} onSelect={onSelect} />}
  </div>
}
