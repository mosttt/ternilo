import * as React from 'react'
import { agentTeamApi } from './agent-team-api'
import type { SessionLiveActivity } from '@/api/live-client'
import { emptyPrice, estimateUsage, mergeUsage, modelUsage, teamSessionIds, usageKey, type ModelUsage, type UsagePrice } from '@/domain/team-usage'
import type { Translate } from '@/i18n/runtime'
import type { AgentTeamSnapshot, LocalSession, SessionEvent } from '@/types'
import css from './agent-team-usage.module.css'

const PRICE_STORAGE = 'ternilo.usage-estimate-prices.v1'
function readPrices(): Record<string, UsagePrice> {
  try {
    const parsed: unknown = JSON.parse(localStorage.getItem(PRICE_STORAGE) ?? '{}')
    if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) return {}
    return Object.fromEntries(Object.entries(parsed).filter((entry): entry is [string, UsagePrice] => {
      const value = entry[1]
      return value && typeof value === 'object' && Object.keys(emptyPrice).every(key => typeof value[key] === 'string')
    }))
  } catch { return {} }
}

type MemberUsage = Record<string, ModelUsage[] | null>
export function AgentTeamUsage({ snapshot, sessionId, sessions, events, liveActivity, refreshRevision, t }: {
  snapshot: AgentTeamSnapshot
  sessionId: string
  sessions: readonly LocalSession[]
  events: readonly SessionEvent[]
  liveActivity: Readonly<Record<string, SessionLiveActivity>>
  refreshRevision: number
  t: Translate<'observability'>
}) {
  const labels = { input: t('team.usage.input'), output: t('team.usage.output'), cached: t('team.usage.cached'), cacheWrite: t('team.usage.cacheWrite'), reasoning: t('team.usage.reasoning') }
  const ids = teamSessionIds(snapshot, sessionId, sessions)
  const mappingKey = JSON.stringify(snapshot.members.map(member => [member.id, ids.get(member.id) ?? null]))
  const activityKey = JSON.stringify([...ids.values()].map(id => [id, Boolean(liveActivity[id]?.running)]))
  const lastResponse = events.findLast(event => event.type === 'assistant_message')?.seq
  const [result, setResult] = React.useState<{ key: string; values: MemberUsage } | null>(null)
  const [loading, setLoading] = React.useState(true)
  const [currency, setCurrency] = React.useState('USD')
  const [prices, setPrices] = React.useState(readPrices)
  const [priceError, setPriceError] = React.useState(false)
  React.useEffect(() => {
    const controller = new AbortController()
    setLoading(true)
    const entries = JSON.parse(mappingKey) as [string, string | null][]
    const load = async () => {
      const values: MemberUsage = {}
      for (let offset = 0; offset < entries.length; offset += 4) {
        await Promise.all(entries.slice(offset, offset + 4).map(async ([member, id]) => {
          if (!id) { values[member] = null; return }
          try { values[member] = modelUsage(await agentTeamApi.events(id, controller.signal)) }
          catch { values[member] = null }
        }))
        if (controller.signal.aborted) return
      }
      setResult({ key: mappingKey, values }); setLoading(false)
    }
    void load()
    return () => controller.abort()
  }, [mappingKey, activityKey, lastResponse, refreshRevision])
  const values = result?.key === mappingKey ? result.values : {}
  const groups = mergeUsage(Object.values(values).flatMap(value => value ?? []))
  const unavailable = snapshot.members.filter(member => values[member.id] == null).length
  const priceFor = (group: ModelUsage) => prices[JSON.stringify([currency, usageKey(group)])] ?? emptyPrice
  const estimates = groups.map(group => estimateUsage(group, priceFor(group)))
  const total = !loading && !unavailable && estimates.every(value => value !== null) ? estimates.reduce<number>((sum, value) => sum + (value ?? 0), 0) : null
  const amount = (value: number | null) => value === null ? t('team.usage.unknown') : `${currency} ${value.toLocaleString(undefined, { maximumFractionDigits: 6 })}`
  const tokens = (value: number) => value.toLocaleString()
  const facts = (items: ModelUsage[]) => {
    const sum = (field: 'input' | 'output' | 'cached' | 'cacheWrite' | 'reasoning' | 'unmeasured' | 'replayed') => items.reduce((total, item) => total + item[field], 0)
    return <>
      <dl className={css.facts}>
        {(['input', 'output', 'cached', 'cacheWrite', 'reasoning'] as const).map(field => <div key={field}><dt>{labels[field]}</dt><dd>{tokens(sum(field))}</dd></div>)}
      </dl>
      {sum('unmeasured') > 0 && <p>{t('team.usage.unmeasured', { count: sum('unmeasured') })}</p>}
      {sum('replayed') > 0 && <p>{t('team.usage.replayed', { count: sum('replayed') })}</p>}
    </>
  }
  return <div className={css.root} data-agent-team-usage="" aria-busy={loading}>
    <p>{t('team.usage.description')}</p>
    {loading && <p role="status">{t('team.usage.loading')}</p>}
    {!loading && unavailable > 0 && <p role="status">{t('team.usage.unavailable', { count: unavailable })}</p>}
    <section className={css.card} aria-label={t('team.usage.total')}>
      <h3>{t('team.usage.total')}</h3>{facts(groups)}
      <p data-usage-estimate="">{t('team.usage.estimate')}: {amount(total)}</p>
    </section>
    <details className={css.card}>
      <summary>{t('team.usage.prices')}</summary>
      <p>{t('team.usage.pricesHint')}</p>
      <label className={css.currency}>{t('team.usage.currency')}<select value={currency} onChange={event => setCurrency(event.target.value)}>{['USD', 'CNY', 'KRW'].map(value => <option key={value}>{value}</option>)}</select></label>
      {groups.map(group => <fieldset key={usageKey(group)} className={css.prices}>
        <legend>{group.provider} / {group.model}</legend>
        {(Object.keys(emptyPrice) as (keyof UsagePrice)[]).map(field => <label key={field}>{labels[field]}<input type="number" min="0" step="any" inputMode="decimal" value={priceFor(group)[field]} onChange={event => {
          const key = JSON.stringify([currency, usageKey(group)])
          const next = { ...prices, [key]: { ...priceFor(group), [field]: event.target.value } }
          setPrices(next)
          try { localStorage.setItem(PRICE_STORAGE, JSON.stringify(next)); setPriceError(false) } catch { setPriceError(true) }
        }} /></label>)}
      </fieldset>)}
      {priceError && <p role="alert">{t('team.usage.priceSaveFailed')}</p>}
    </details>
    {snapshot.members.map(member => <section key={member.id} className={css.card} data-usage-member={member.id}>
      <h3>{member.label} <small>{member.role === 'lead' ? t('team.role.lead') : t('team.usage.child')}</small></h3>
      {values[member.id] ? <>{facts(values[member.id]!)}{values[member.id]!.map(group => <p key={usageKey(group)}>{group.provider} / {group.model} · {amount(estimateUsage(group, priceFor(group)))}</p>)}</> : <p>{loading ? t('team.usage.loading') : t('team.usage.unknown')}</p>}
    </section>)}
  </div>
}
