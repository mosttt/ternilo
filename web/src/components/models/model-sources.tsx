import * as React from 'react'
import { Check, ChevronDown, Laptop, RefreshCw } from 'lucide-react'
import { Select as SelectPrimitive } from 'radix-ui'
import { api } from '@/api/client'
import { navigate, useSearch } from '@/app/navigation'
import { Button } from '@/components/ui/button'
import { ProviderLibrary } from '@/components/settings/provider-library'
import { ModelPicker } from '@/components/workbench/model-picker'
import { useTranslate } from '@/i18n/provider'
import { useWorkbench } from '@/state/workbench'
import type { ExecutionTarget } from '@/domain/execution-target'
import { invalidateProviderInventory } from '@/domain/provider-inventory'
import css from './model-service.module.css'

export function AccountModels() {
  const { serverIdentity } = useWorkbench()
  const t = useTranslate('modelService')
  const personal = serverIdentity?.personal_tenant_id
  if (!personal) return <p className={css.state} role="status">{t('loading')}</p>
  return <div className={css.page} data-provider-scope="account">
    <ProviderLibrary target={{ tenantId: personal }} scope="account" title={t('accountSourceTitle', { name: serverIdentity.user.username })} description={t('accountSourceDescription')} />
    <section className={css.notice} data-account-model-default="">
      <h3 className="text-sm font-semibold">{t('accountDefaultTitle')}</h3>
      <p className={css.hint}>{t('accountDefaultDescription')}</p>
      <div className="mt-3"><ModelPicker defaultTenantId={personal} /></div>
    </section>
  </div>
}

export function modelCenterPath(values: Record<string, string>, search = window.location.search) {
  const query = new URLSearchParams(search)
  for (const [key, value] of Object.entries(values)) {
    if (value) query.set(key, value)
    else query.delete(key)
  }
  return `/models?${query}`
}

export function SourcePicker({ label, value, options, onChange, disabled = false }: {
  label: string; value: string; options: Array<{ id: string; name: string }>; onChange(value: string): void; disabled?: boolean
}) {
  const id = React.useId()
  return <div className="grid min-w-0 gap-2">
    <label htmlFor={id} className="text-sm font-medium">{label}</label>
    <SelectPrimitive.Root value={value} onValueChange={onChange} disabled={disabled || !options.length}>
      <SelectPrimitive.Trigger id={id} className={css.sourceTrigger} aria-label={label} data-source-value={value}>
        <SelectPrimitive.Value placeholder={label} />
        <SelectPrimitive.Icon><ChevronDown /></SelectPrimitive.Icon>
      </SelectPrimitive.Trigger>
      <SelectPrimitive.Portal><SelectPrimitive.Content className={css.sourceMenu} position="popper" align="start" sideOffset={6} collisionPadding={12} data-ternilo-dismiss-layer="">
        <SelectPrimitive.Viewport>{options.map(option => <SelectPrimitive.Item key={option.id} value={option.id} textValue={option.name} className={css.sourceOption} data-source-id={option.id}>
          <SelectPrimitive.ItemText>{option.name}</SelectPrimitive.ItemText><SelectPrimitive.ItemIndicator><Check /></SelectPrimitive.ItemIndicator>
        </SelectPrimitive.Item>)}</SelectPrimitive.Viewport>
      </SelectPrimitive.Content></SelectPrimitive.Portal>
    </SelectPrimitive.Root>
  </div>
}

export function ComputerModels() {
  const { tenants, serverIdentity } = useWorkbench()
  const t = useTranslate('modelService')
  const search = useSearch()
  const parameters = new URLSearchParams(search)
  const tenantId = parameters.get('space') || serverIdentity?.personal_tenant_id || ''
  const expanded = parameters.get('computer')
  const tenant = tenants.find(item => item.tenant_id === tenantId)
  const [loaded, setState] = React.useState<{ tenantId: string; computers: ModelComputer[] } | null>(null)
  const computers = loaded?.tenantId === tenantId ? loaded.computers : []
  const [error, setError] = React.useState('')
  const [loading, setLoading] = React.useState(true)
  const [revision, reload] = React.useReducer(value => value + 1, 0)
  const refresh = () => {
    for (const computer of computers) invalidateProviderInventory(computerTarget(tenantId, computer))
    reload()
  }
  React.useEffect(() => {
    setError(''); setLoading(true)
    if (!tenant) { setLoading(false); return }
    const controller = new AbortController()
    void api.request<ModelComputer[]>('/model-computers', { headers: { 'x-ternilo-tenant': tenantId }, signal: controller.signal })
      .then(value => { if (!controller.signal.aborted) setState({ tenantId, computers: value }) })
      .catch(cause => { if (!controller.signal.aborted) setError(cause instanceof Error ? cause.message : String(cause)) })
      .finally(() => { if (!controller.signal.aborted) setLoading(false) })
    return () => controller.abort()
  }, [tenantId, Boolean(tenant), revision])
  return <div className={css.page} data-computer-models="">
    <p className={css.hint}>{t('computerSourceDescription')}</p>
    <div className={css.sourceSelectors}>
      <SourcePicker label={t('sourceSpace')} value={tenant ? tenantId : ''} options={tenants.map(item => ({ id: item.tenant_id, name: item.kind === 'personal' ? t('personalSpace') : item.display_name }))} onChange={space => navigate(modelCenterPath({ space, computer: '' }, search))} />
      <Button variant="outline" disabled={loading} onClick={refresh} aria-label={t('refresh')}><RefreshCw className={loading ? css.spinner : ''} /></Button>
    </div>
    {loading && !computers.length && <p className={css.state} role="status">{t('loading')}</p>}
    {error && <p className={css.state} role="alert">{error}<Button variant="outline" onClick={refresh}>{t('retry')}</Button></p>}
    {!loading && !error && !computers.length && <p className={css.state}>{t('noComputerSources')}</p>}
    {computers.map(computer => {
      const open = expanded === computer.executor_id
      const target = computerTarget(tenantId, computer)
      return <article key={`${tenantId}:${computer.executor_id}`} className={css.computer} data-model-computer={computer.executor_id}>
        <header className={css.computerHeader}>
          <Laptop className="size-5 shrink-0" /><div className={css.identity}><strong>{computer.executor_id}</strong><p>{t(computer.connected ? 'computerOnline' : 'computerOffline')} · {t(computer.can_configure ? 'computerOwned' : 'computerShared')}</p></div>
          <Button variant="outline" aria-expanded={open} onClick={() => navigate(modelCenterPath({ computer: open ? '' : computer.executor_id }, search))}>{t(open ? 'hideComputerModels' : 'showComputerModels')}</Button>
        </header>
        {open && <div className={css.computerBody}>
          {!computer.connected ? <p className={css.hint} role="status">{t('computerSourceOffline')}</p> : <div data-provider-scope="node">
            <ProviderLibrary target={target} scope="node" authorable={computer.can_configure} refreshRevision={revision} title={t('computerModelsTitle')} description={t(computer.can_configure ? 'computerProviderDescription' : 'computerSourceShared')} />
          </div>}
        </div>}
      </article>
    })}
  </div>
}

export interface ModelComputer {
  executor_id: string
  connected: boolean
  can_configure: boolean
  workspace_id: string | null
  session_id: string | null
}

function computerTarget(tenantId: string, computer: ModelComputer): ExecutionTarget {
  return { tenantId, ...(computer.workspace_id ? { workspaceId: computer.workspace_id } : computer.session_id ? { sessionId: computer.session_id } : { executorId: computer.executor_id }) }
}
