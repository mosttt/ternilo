import * as React from 'react'
import type { HostedWebTools } from '@/types'
import { Field, Input, Label, Select } from '@/components/ui/field'
import { useTranslate } from '@/i18n/provider'

const defaults: HostedWebTools = { web_search: false, web_fetch: false, max_uses: 5, max_content_tokens: 20000, allowed_domains: [], blocked_domains: [] }

export function HostedToolsEditor({ value, onChange }: { value?: HostedWebTools | null; onChange(value: HostedWebTools | null): void }) {
  const id = React.useId(), t = useTranslate('settings')
  const settings = value ?? defaults
  const [filter, setFilter] = React.useState<'allowed_domains' | 'blocked_domains'>(settings.blocked_domains.length ? 'blocked_domains' : 'allowed_domains')
  const update = (change: Partial<HostedWebTools>) => {
    const next = { ...settings, ...change }
    onChange(next.web_search || next.web_fetch ? next : null)
  }
  return <section className="mt-5 space-y-3 border-t pt-5" data-hosted-tools="">
    <div className="text-sm font-medium">{t('provider.hostedTools')}</div>
    <p className="text-xs leading-relaxed text-muted-foreground">{t('provider.hostedDescription')}</p>
    <div className="flex flex-wrap gap-5">
      <label className="flex items-center gap-2 text-sm"><input type="checkbox" checked={settings.web_search} onChange={event => update({ web_search: event.target.checked })} />{t('provider.hostedSearch')}</label>
      <label className="flex items-center gap-2 text-sm"><input type="checkbox" checked={settings.web_fetch} onChange={event => update({ web_fetch: event.target.checked })} />{t('provider.hostedFetch')}</label>
    </div>
    {value && <div className="grid gap-4 sm:grid-cols-2">
      <Field><Label htmlFor={`${id}-uses`}>{t('provider.hostedUses')}</Label><Input id={`${id}-uses`} type="number" min={1} max={20} value={settings.max_uses} onChange={event => update({ max_uses: Number(event.target.value) })} /></Field>
      {settings.web_fetch && <Field><Label htmlFor={`${id}-tokens`}>{t('provider.hostedContent')}</Label><Input id={`${id}-tokens`} type="number" min={1} value={settings.max_content_tokens} onChange={event => update({ max_content_tokens: Number(event.target.value) })} /></Field>}
      <Field><Label htmlFor={`${id}-filter`}>{t('provider.hostedFilter')}</Label><Select id={`${id}-filter`} value={filter} onValueChange={next => {
        setFilter(next as typeof filter); update({ allowed_domains: [], blocked_domains: [] })
      }}><option value="allowed_domains">{t('provider.hostedAllow')}</option><option value="blocked_domains">{t('provider.hostedBlock')}</option></Select></Field>
      <Field><Label htmlFor={`${id}-domains`}>{t('provider.hostedDomains')}</Label><Input key={filter} id={`${id}-domains`} defaultValue={settings[filter].join(', ')} onChange={event => update({ [filter]: event.target.value.split(',').map(value => value.trim()).filter(Boolean) })} placeholder="example.com" /></Field>
    </div>}
  </section>
}
