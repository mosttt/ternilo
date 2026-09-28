import { Check, Search } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/field'
import { DropdownMenuGroup, DropdownMenuItem, DropdownMenuLabel } from '@/components/ui/dropdown-menu'
import { useModelPage } from '@/components/models/model-service-ui'
import { cloudModelOptionsPath, type CloudModelOption } from '@/domain/cloud-model-inventory'
import type { ExecutionTarget } from '@/domain/execution-target'
import type { ModelSelection } from '@/types'
import { useTranslate } from '@/i18n/provider'
import css from './model-picker.module.css'

export function PlatformModelItems({ target, selection, onSelect }: {
  target: ExecutionTarget; selection: ModelSelection; onSelect(model: ModelSelection): void
}) {
  const t = useTranslate('model')
  const directory = useTranslate('modelService')
  const state = useModelPage<CloudModelOption>(cloudModelOptionsPath(target), 'options', target.tenantId)
  const groups = new Map<string, { name: string; models: CloudModelOption[] }>()
  for (const option of state.items) {
    const group = groups.get(option.grant_id) ?? { name: option.grant_name, models: [] }
    group.models.push(option)
    groups.set(option.grant_id, group)
  }
  return <div className={`${css.sourceGroup} ${css.platformItems}`} data-model-source="platform">
    <DropdownMenuLabel className={css.sourceTitle}>{t('cloud.platform')}</DropdownMenuLabel>
    <form className="mx-2 mb-2 flex gap-1" role="search" aria-label={t('cloud.search')} onSubmit={event => { event.preventDefault(); state.search() }}>
      <Input className="h-8 min-w-0 text-xs" aria-label={t('cloud.search')} placeholder={t('cloud.search')} value={state.draft} onChange={event => state.setDraft(event.target.value)} onKeyDown={event => { if (event.key !== 'Escape' && event.key !== 'Tab') event.stopPropagation() }} />
      <Button type="submit" variant="ghost" size="icon-sm" aria-label={directory('search')}><Search /></Button>
    </form>
    {state.loading ? <p className="px-3 py-2 text-xs text-muted-foreground" role="status">{directory('loading')}</p>
      : state.error ? <div className="px-3 py-2 text-xs" role="alert"><p className="text-destructive">{state.error}</p><Button size="xs" variant="ghost" onClick={state.reload}>{directory('retry')}</Button></div>
        : !state.items.length ? <p className="px-3 py-2 text-xs text-muted-foreground">{t('cloud.empty')}</p> : [...groups].map(([grantId, group]) => <DropdownMenuGroup key={grantId} aria-label={group.name} data-model-grant={grantId}>
          <DropdownMenuLabel className={css.providerTitle}>{t('cloud.budget', { name: group.name })}</DropdownMenuLabel>
          {group.models.map(({ model }) => <DropdownMenuItem key={model.model_id} className={css.modelOption} data-selected={selection.provider === 'platform_model' && selection.grant_id === grantId && selection.model_id === model.model_id || undefined} onSelect={() => onSelect({ provider: 'platform_model', grant_id: grantId, model_id: model.model_id })}>
            <span className="size-2 shrink-0 rounded-full border border-success bg-success" aria-hidden="true" />
            <div className="min-w-0 flex-1"><div className="truncate">{model.display_name}</div><div className="truncate font-mono text-[10px] text-muted-foreground">{model.model_id}</div></div>
            {selection.provider === 'platform_model' && selection.grant_id === grantId && selection.model_id === model.model_id && <Check />}
          </DropdownMenuItem>)}
        </DropdownMenuGroup>)}
    {(state.nextCursor || state.page > 1) && <div className="flex justify-between gap-2 px-2 py-2">
      <Button variant="ghost" size="xs" disabled={state.loading || state.page === 1} onClick={state.previous}>{t('menu.previous')}</Button>
      <Button variant="ghost" size="xs" disabled={state.loading || !state.nextCursor} onClick={() => { if (state.nextCursor) state.next(state.nextCursor) }}>{t('menu.next')}</Button>
    </div>}
  </div>
}
