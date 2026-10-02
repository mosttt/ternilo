import * as React from 'react'
import { Check } from 'lucide-react'
import type { CredentialInventory, ModelSelection, ProviderProfile } from '@/types'
import { providerIsUsable } from '@/domain/provider-readiness'
import { isConnectionProvider } from '@/components/models/model-device-types'
import { DropdownMenuGroup, DropdownMenuItem, DropdownMenuLabel } from '@/components/ui/dropdown-menu'
import { useTranslate } from '@/i18n/provider'
import { cn } from '@/lib/utils'
import css from './model-picker.module.css'

export function ProviderModelItems({ providers, credentials, selection, accountOwner, computerId, label, onSelect }: {
  providers: ProviderProfile[]; credentials: CredentialInventory | null; selection: ModelSelection;
  accountOwner?: string; computerId?: string; label?: string; onSelect(model: ModelSelection): void
}) {
  const t = useTranslate('model')
  if (!providers.length) return null
  return <div className={css.sourceGroup} data-model-source={computerId ? 'computer' : accountOwner ? 'account' : 'node'}>
    {label && <DropdownMenuLabel className={css.sourceTitle}>{label}</DropdownMenuLabel>}
    {providers.map(provider => {
      const usable = providerIsUsable(provider, credentials)
      return <React.Fragment key={provider.id}>
        <DropdownMenuGroup aria-label={provider.display_name} data-model-provider={provider.id}>
          <DropdownMenuLabel className={css.providerTitle}>{provider.display_name}{isConnectionProvider(provider.id) && provider.base_url && <span className="mt-0.5 block break-all font-normal" data-model-server="">{new URL(provider.base_url).origin}</span>}</DropdownMenuLabel>
          {provider.models.map(model => {
            const active = (computerId ? selection.provider === 'computer_provider' && selection.executor_id === computerId : accountOwner ? selection.provider === 'account_provider' && selection.owner_user_id === accountOwner : selection.provider === 'named_provider') && 'provider_id' in selection && selection.provider_id === provider.id && selection.model === model.id
            return <DropdownMenuItem disabled={!usable} key={model.id} className={css.modelOption} data-selected={active || undefined} onSelect={() => onSelect(computerId ? { provider: 'computer_provider', executor_id: computerId, provider_id: provider.id, model: model.id } : accountOwner ? { provider: 'account_provider', owner_user_id: accountOwner, provider_id: provider.id, model: model.id } : { provider: 'named_provider', provider_id: provider.id, model: model.id })}>
              <span className={cn('size-2 rounded-full border', usable && 'border-success bg-success')} aria-hidden="true" />
              <div className="min-w-0 flex-1"><div className="truncate">{model.display_name || model.id}</div><div className="truncate font-mono text-[10px] text-muted-foreground">{model.id}</div></div>
              {!usable && <span className="text-[10px] text-muted-foreground">{t('provider.unavailable')}</span>}
              {active && <Check />}
            </DropdownMenuItem>
          })}
        </DropdownMenuGroup>
      </React.Fragment>
    })}
  </div>
}
