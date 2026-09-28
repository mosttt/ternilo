import { Field, Label, Select } from '@/components/ui/field'
import { useTranslate } from '@/i18n/provider'
import type { ModelEntitlement } from './model-service-api'
import type { ModelDeviceScope, ModelDeviceProvider } from './model-device-types'
import { QuotaFacts } from './model-service-ui'

export function ModelDeviceScopeEditor({ scope, onChange, grants, providers = [] }: {
  scope: ModelDeviceScope
  onChange(scope: ModelDeviceScope): void
  grants: ModelEntitlement[]
  providers?: ModelDeviceProvider[]
}) {
  const t = useTranslate('modelService')
  const toggle = (grantId: string, models: string[]) => {
    if (scope.kind !== 'selected') return
    onChange({ ...scope, grants: [
      ...scope.grants.filter(entry => entry.grant_id !== grantId),
      ...(models.length ? [{ grant_id: grantId, model_ids: models }] : []),
    ] })
  }
  const toggleProvider = (providerId: string, models: string[]) => {
    if (scope.kind !== 'selected') return
    onChange({ ...scope, providers: [
      ...(scope.providers ?? []).filter(entry => entry.provider_id !== providerId),
      ...(models.length ? [{ provider_id: providerId, model_ids: models }] : []),
    ] })
  }
  return <div className="space-y-3">
    <Field><Label htmlFor="device-model-scope">{t('deviceAllowance')}</Label>
      <Select id="device-model-scope" value={scope.kind} onValueChange={nextValue => onChange(nextValue === 'account' ? { kind: 'account' } : { kind: 'selected', grants: [] })}>
        <option value="account">{t('deviceScopeAccount')}</option>
        <option value="selected">{t('deviceScopeSelected')}</option>
      </Select>
    </Field>
    {scope.kind === 'account' ? <div className="space-y-3"><p className="text-sm leading-relaxed text-muted-foreground">{t('deviceScopeAccountHint')}</p>
      <label className="flex min-h-10 items-start gap-2 text-sm"><input className="mt-1 shrink-0" type="checkbox" checked={Boolean(scope.include_account_providers)} onChange={event => onChange({ ...scope, include_account_providers: event.target.checked })} /><span>{t('deviceIncludeAccount')}<small className="mt-1 block text-xs text-muted-foreground">{t('deviceAccountCost')}</small></span></label>
      </div>
      : <><h3 className="text-sm font-medium">{t('platformSourceTab')}</h3>{grants.map(({ grant, models }) => {
        const selected = scope.grants.find(entry => entry.grant_id === grant.grant_id)?.model_ids ?? []
        return <fieldset key={grant.grant_id} className="space-y-2 rounded-xl border p-3" data-device-grant={grant.grant_id}>
          <label className="flex min-h-10 items-center gap-2 text-sm font-medium"><input type="checkbox" checked={selected.length > 0} disabled={!models.length}
            onChange={event => toggle(grant.grant_id, event.target.checked ? models.map(model => model.model_id) : [])} />{grant.name}</label>
          <QuotaFacts quota={grant.quota} />
          {models.map(model => <label key={model.model_id} className="flex min-h-10 items-center gap-2 break-words text-sm">
            <input type="checkbox" checked={selected.includes(model.model_id)} onChange={event => toggle(grant.grant_id,
              event.target.checked ? [...selected, model.model_id] : selected.filter(id => id !== model.model_id))} />{model.display_name}
          </label>)}
        </fieldset>
      })}<h3 className="pt-2 text-sm font-medium">{t('accountSourceTab')}</h3><p className="text-xs leading-relaxed text-muted-foreground">{t('deviceAccountCost')}</p>
      {providers.map(provider => {
        const selected = scope.providers?.find(entry => entry.provider_id === provider.provider_id)?.model_ids ?? []
        return <fieldset key={provider.provider_id} className="space-y-2 rounded-xl border p-3" data-device-provider={provider.provider_id}>
          <label className="flex min-h-10 items-center gap-2 text-sm font-medium"><input type="checkbox" checked={selected.length > 0} onChange={event => toggleProvider(provider.provider_id, event.target.checked ? provider.models.map(model => model.model_id) : [])} />{provider.provider_name}</label>
          {provider.models.map(model => <label key={model.model_id} className="flex min-h-10 items-center gap-2 break-words text-sm"><input type="checkbox" checked={selected.includes(model.model_id)} onChange={event => toggleProvider(provider.provider_id, event.target.checked ? [...selected, model.model_id] : selected.filter(id => id !== model.model_id))} /><span>{model.display_name}<code className="block text-xs text-muted-foreground">{model.model_id}</code></span></label>)}
        </fieldset>
      })}{!providers.length && <p className="text-sm text-muted-foreground">{t('deviceNoAccountProviders')}</p>}</>}
  </div>
}
