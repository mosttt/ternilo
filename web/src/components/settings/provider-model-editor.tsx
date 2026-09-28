import * as React from 'react'
import { ChevronDown, DownloadCloud, LoaderCircle, Plus, Trash2 } from 'lucide-react'
import { reasoningEfforts } from '@/domain/model-reasoning'
import type {
  ProviderModel,
  ProviderModelDefaults,
  ProviderModelReasoning,
  ProviderModelValues,
  ReasoningEffort,
} from '@/types'
import { Button } from '@/components/ui/button'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { Field, FieldDescription, Input, Label, Select } from '@/components/ui/field'
import { Switch } from '@/components/ui/switch'
import { useTranslate } from '@/i18n/provider'
import { cn } from '@/lib/utils'
import styles from './settings-layout.module.css'

export interface ModelReasoningDraft {
  defaultEffort: ReasoningEffort
  efforts: Partial<Record<ReasoningEffort, string | null>>
}

export interface ModelSettingsDraft {
  contextWindow: string
  maxOutputTokens: string
  reasoning?: ModelReasoningDraft
}

export interface ProviderModelDraft {
  id: string
  displayName: string
  upstream: ProviderModelValues
  overrides: {
    contextWindow?: string
    maxOutputTokens?: string
    reasoning?: ModelReasoningDraft | null
  }
}

export interface ProviderModelValidationCopy {
  invalidCapacity(raw: string): string
  positiveCapacity(raw: string): string
  missingId(index: number): string
  missingEffort(id: string): string
  defaultNotEnabled(id: string): string
}

export function capacityText(value: number | null | undefined) {
  if (!value) return ''
  if (value % 1_000_000 === 0) return `${value / 1_000_000}M`
  if (value % 1_000 === 0) return `${value / 1_000}K`
  return String(value)
}

export function parseCapacity(raw: string, copy: ProviderModelValidationCopy) {
  const value = raw.trim().toUpperCase()
  if (!value) return null
  const match = value.match(/^(\d+)([KM])?$/)
  if (!match) throw new Error(copy.invalidCapacity(raw))
  const multiplier = match[2] === 'K' ? 1_000 : match[2] === 'M' ? 1_000_000 : 1
  const parsed = Number(match[1]) * multiplier
  if (!Number.isSafeInteger(parsed) || parsed <= 0) throw new Error(copy.positiveCapacity(raw))
  return parsed
}

export function settingsDraft(settings: ProviderModelDefaults): ModelSettingsDraft {
  return {
    contextWindow: capacityText(settings.context_window),
    maxOutputTokens: capacityText(settings.max_output_tokens),
    ...(settings.reasoning ? {
      reasoning: {
        defaultEffort: settings.reasoning.default_effort,
        efforts: { ...settings.reasoning.efforts },
      },
    } : {}),
  }
}

export function defaultSettingsDraft(): ModelSettingsDraft {
  return { contextWindow: '128K', maxOutputTokens: '16K' }
}

export function modelDraft(model: ProviderModel): ProviderModelDraft {
  const overrides: ProviderModelValues = model.settings.mode === 'automatic' ? model.settings.overrides
    : model.settings.mode === 'override' ? {
      context_window: model.settings.context_window,
      max_output_tokens: model.settings.max_output_tokens,
      reasoning: model.settings.reasoning ? { mode: 'enabled', configuration: model.settings.reasoning } : { mode: 'disabled' },
    } : {}
  return {
    id: model.id,
    displayName: model.display_name ?? '',
    upstream: model.settings.mode === 'automatic' ? model.settings.upstream : {},
    overrides: {
      ...(overrides.context_window != null ? { contextWindow: capacityText(overrides.context_window) } : {}),
      ...(overrides.max_output_tokens != null ? { maxOutputTokens: capacityText(overrides.max_output_tokens) } : {}),
      ...(overrides.reasoning ? { reasoning: overrides.reasoning.mode === 'disabled' ? null : {
        defaultEffort: overrides.reasoning.configuration.default_effort,
        efforts: { ...overrides.reasoning.configuration.efforts },
      } } : {}),
    },
  }
}

function reasoningValue(
  reasoning: ModelReasoningDraft,
  label: string,
  copy: ProviderModelValidationCopy,
): ProviderModelReasoning {
  const enabledEfforts = reasoningEfforts.filter(effort => Object.hasOwn(reasoning.efforts, effort))
  if (!enabledEfforts.length) throw new Error(copy.missingEffort(label))
  if (!enabledEfforts.includes(reasoning.defaultEffort)) throw new Error(copy.defaultNotEnabled(label))
  return {
    default_effort: reasoning.defaultEffort,
    efforts: Object.fromEntries(enabledEfforts.map(effort => [effort, reasoning.efforts[effort] || null])),
  }
}

export function providerModelDefaults(
  draft: ModelSettingsDraft,
  label: string,
  copy: ProviderModelValidationCopy,
): ProviderModelDefaults {
  const contextWindow = parseCapacity(draft.contextWindow, copy)
  const maxOutputTokens = parseCapacity(draft.maxOutputTokens, copy)
  if (!contextWindow) throw new Error(copy.positiveCapacity(draft.contextWindow))
  if (!maxOutputTokens) throw new Error(copy.positiveCapacity(draft.maxOutputTokens))
  return {
    context_window: contextWindow,
    max_output_tokens: maxOutputTokens,
    ...(draft.reasoning ? { reasoning: reasoningValue(draft.reasoning, label, copy) } : {}),
  }
}

export function providerModel(
  draft: ProviderModelDraft,
  index: number,
  copy: ProviderModelValidationCopy,
): ProviderModel {
  const id = draft.id.trim()
  if (!id) throw new Error(copy.missingId(index + 1))
  const capacity = (value: string) => {
    const parsed = parseCapacity(value, copy)
    if (parsed === null) throw new Error(copy.positiveCapacity(value))
    return parsed
  }
  return {
    id,
    display_name: draft.displayName.trim() || null,
    settings: {
      mode: 'automatic',
      upstream: draft.upstream,
      overrides: {
        ...(draft.overrides.contextWindow !== undefined ? { context_window: capacity(draft.overrides.contextWindow) } : {}),
        ...(draft.overrides.maxOutputTokens !== undefined ? { max_output_tokens: capacity(draft.overrides.maxOutputTokens) } : {}),
        ...(draft.overrides.reasoning === null ? { reasoning: { mode: 'disabled' as const } }
          : draft.overrides.reasoning ? { reasoning: { mode: 'enabled' as const, configuration: reasoningValue(draft.overrides.reasoning, id, copy) } } : {}),
      },
    },
  }
}

function defaultReasoning(): ModelReasoningDraft {
  return { defaultEffort: 'medium', efforts: { low: 'low', medium: 'medium', high: 'high' } }
}

export function ProviderSettingsEditor({
  value,
  onChange,
  idPrefix,
  name,
  reasoningOnly = false,
}: {
  value: ModelSettingsDraft
  onChange(value: ModelSettingsDraft): void
  idPrefix: string
  name: string
  reasoningOnly?: boolean
}) {
  const t = useTranslate('settings')
  const updateReasoning = (mutate: (value: ModelReasoningDraft) => ModelReasoningDraft) => {
    if (value.reasoning) onChange({ ...value, reasoning: mutate(value.reasoning) })
  }
  const toggleEffort = (effort: ReasoningEffort, checked: boolean) => {
    updateReasoning(current => {
      const efforts = { ...current.efforts }
      if (checked) efforts[effort] = effort
      else delete efforts[effort]
      const enabled = reasoningEfforts.filter(candidate => Object.hasOwn(efforts, candidate))
      return {
        efforts,
        defaultEffort: current.defaultEffort === effort ? (enabled[0] ?? 'medium') : current.defaultEffort,
      }
    })
  }

  return <div className="space-y-4">
    {!reasoningOnly && <>
    <div className="grid gap-4 sm:grid-cols-2">
      <Field><Label htmlFor={`${idPrefix}-context`}>{t('provider.contextWindow')}</Label><Input id={`${idPrefix}-context`} value={value.contextWindow} placeholder="128K" onChange={event => onChange({ ...value, contextWindow: event.target.value })} /></Field>
      <Field><Label htmlFor={`${idPrefix}-output`}>{t('provider.maxOutput')}</Label><Input id={`${idPrefix}-output`} value={value.maxOutputTokens} placeholder="16K" onChange={event => onChange({ ...value, maxOutputTokens: event.target.value })} /></Field>
    </div>
    <div className="flex items-center justify-between gap-4 rounded-lg border bg-muted/20 p-3">
      <div><div className="text-sm font-medium">{t('provider.reasoning')}</div><p className="mt-1 text-xs text-muted-foreground">{t('provider.reasoningDescription')}</p></div>
      <Switch aria-label={t('provider.enableReasoning', { name })} checked={Boolean(value.reasoning)} onCheckedChange={checked => onChange({ ...value, reasoning: checked ? defaultReasoning() : undefined })} />
    </div>
    </>}
    {value.reasoning ? <div className="space-y-4">
      <Field className="max-w-xs">
        <Label htmlFor={`${idPrefix}-default-effort`}>{t('provider.defaultEffort')}</Label>
        <Select id={`${idPrefix}-default-effort`} value={value.reasoning.defaultEffort} onValueChange={nextValue => updateReasoning(current => ({ ...current, defaultEffort: nextValue as ReasoningEffort }))}>
          {reasoningEfforts.filter(effort => Object.hasOwn(value.reasoning?.efforts ?? {}, effort)).map(effort => <option value={effort} key={effort}>{effort}</option>)}
        </Select>
        <FieldDescription>{t('provider.defaultEffortDescription')}</FieldDescription>
      </Field>
      <div className="overflow-hidden rounded-lg border">
        <div className="grid grid-cols-[auto_minmax(0,1fr)] gap-3 border-b bg-muted/35 px-3 py-2 text-xs text-muted-foreground sm:grid-cols-[7.5rem_minmax(0,1fr)]"><span>{t('provider.unifiedEffort')}</span><span>{t('provider.actualValue')}</span></div>
        {reasoningEfforts.map(effort => {
          const enabled = Object.hasOwn(value.reasoning?.efforts ?? {}, effort)
          return <div className="grid grid-cols-[auto_minmax(0,1fr)] items-center gap-3 border-b p-3 last:border-b-0 sm:grid-cols-[7.5rem_minmax(0,1fr)]" key={effort}>
            <label className="flex items-center gap-2 text-sm"><input type="checkbox" checked={enabled} onChange={event => toggleEffort(effort, event.target.checked)} /><code>{effort}</code></label>
            <Input className="font-mono text-xs" aria-label={t('provider.actualValueAria', { name: effort })} disabled={!enabled} value={value.reasoning?.efforts[effort] ?? ''} placeholder={t('provider.actualValuePlaceholder')} onChange={event => updateReasoning(current => ({ ...current, efforts: { ...current.efforts, [effort]: event.target.value || null } }))} />
          </div>
        })}
      </div>
      <FieldDescription>{t('provider.effortMappingDescription')}</FieldDescription>
    </div> : null}
  </div>
}

function ModelSettingsEditor({ defaults, model, onChange, idPrefix }: {
  defaults: ModelSettingsDraft
  model: ProviderModelDraft
  onChange(overrides: ProviderModelDraft['overrides']): void
  idPrefix: string
}) {
  const t = useTranslate('settings')
  const { upstream, overrides } = model
  const automatic: ModelSettingsDraft = {
    contextWindow: upstream.context_window != null ? capacityText(upstream.context_window) : defaults.contextWindow,
    maxOutputTokens: upstream.max_output_tokens != null ? capacityText(upstream.max_output_tokens) : defaults.maxOutputTokens,
    reasoning: upstream.reasoning?.mode === 'disabled' ? undefined : upstream.reasoning?.mode === 'enabled'
      ? { defaultEffort: upstream.reasoning.configuration.default_effort, efforts: { ...upstream.reasoning.configuration.efforts } } : defaults.reasoning,
  }
  const source = (present: boolean) => t(present ? 'provider.sourceUpstream' : 'provider.sourceDefault')
  const reasoningMode = overrides.reasoning === undefined ? 'automatic' : overrides.reasoning === null ? 'disabled' : 'custom'
  return <div className="space-y-5" data-model-settings={model.id}>
    <p className="text-xs text-muted-foreground">{t('provider.fieldPriority')}</p>
    <div className="grid gap-4 sm:grid-cols-2">
      {([
        { field: 'contextWindow', upstreamField: 'context_window', label: 'provider.contextWindow', suffix: 'context' },
        { field: 'maxOutputTokens', upstreamField: 'max_output_tokens', label: 'provider.maxOutput', suffix: 'output' },
      ] as const).map(({ field, upstreamField, label, suffix }) => {
        const custom = overrides[field] !== undefined
        return <Field key={field}>
          <Label htmlFor={`${idPrefix}-${suffix}`}>{t(label)}</Label>
          <Select aria-label={t('provider.fieldSource', { field: t(label) })} value={custom ? 'custom' : 'automatic'} onValueChange={value => onChange({ ...overrides, [field]: value === 'custom' ? automatic[field] : undefined })}>
            <option value="automatic">{t('provider.sourceAutomatic', { source: source(upstream[upstreamField] != null) })}</option>
            <option value="custom">{t('provider.sourceCustom')}</option>
          </Select>
          <Input id={`${idPrefix}-${suffix}`} value={overrides[field] ?? automatic[field]} readOnly={!custom} onChange={event => onChange({ ...overrides, [field]: event.target.value })} />
        </Field>
      })}
    </div>
    <Field className="max-w-sm">
      <Label htmlFor={`${idPrefix}-reasoning-source`}>{t('provider.reasoning')}</Label>
      <Select id={`${idPrefix}-reasoning-source`} value={reasoningMode} onValueChange={value => onChange({ ...overrides, reasoning: value === 'automatic' ? undefined : value === 'disabled' ? null : {
        ...(automatic.reasoning ?? defaultReasoning()), efforts: { ...(automatic.reasoning ?? defaultReasoning()).efforts },
      } })}>
        <option value="automatic">{t('provider.sourceAutomatic', { source: source(upstream.reasoning != null) })}</option>
        <option value="custom">{t('provider.sourceCustom')}</option>
        <option value="disabled">{t('provider.reasoningDisabled')}</option>
      </Select>
    </Field>
    {overrides.reasoning ? <ProviderSettingsEditor reasoningOnly idPrefix={idPrefix} name={model.id} value={{ ...automatic, reasoning: overrides.reasoning }} onChange={value => onChange({ ...overrides, reasoning: value.reasoning })} />
      : <p className="rounded-lg border bg-muted/15 px-3 py-2 text-xs text-muted-foreground" data-effective-reasoning="">
        {reasoningMode === 'automatic' && automatic.reasoning
          ? t('provider.automaticReasoning', { effort: automatic.reasoning.defaultEffort, levels: Object.keys(automatic.reasoning.efforts).join(' / ') })
          : t('provider.reasoningDisabled')}
      </p>}
  </div>
}

export function mergeDiscoveredModels(
  existing: ProviderModelDraft[],
  candidates: ProviderModel[],
  picked: ReadonlySet<string>,
): ProviderModelDraft[] {
  const models = existing.filter(model => model.id.trim() || model.displayName.trim() || Object.values(model.overrides).some(value => value !== undefined))
  const known = new Map(models.map((model, index) => [model.id.trim(), index]))
  for (const candidate of candidates) {
    if (!picked.has(candidate.id)) continue
    const discovered = modelDraft(candidate)
    const index = known.get(candidate.id)
    if (index !== undefined) models[index] = { ...models[index], upstream: discovered.upstream }
    else { known.set(candidate.id, models.length); models.push(discovered) }
  }
  return models
}

export function ProviderModelEditor({
  defaults,
  models,
  onChange,
  onDiscover,
}: {
  defaults: ModelSettingsDraft
  models: ProviderModelDraft[]
  onChange(models: ProviderModelDraft[]): void
  onDiscover(): Promise<ProviderModel[]>
}) {
  const fieldId = React.useId()
  const t = useTranslate('settings')
  const common = useTranslate('common')
  const [expanded, setExpanded] = React.useState<Set<number>>(() => new Set())
  const [discovering, setDiscovering] = React.useState(false)
  const [discoveryError, setDiscoveryError] = React.useState('')
  const [candidates, setCandidates] = React.useState<ProviderModel[] | null>(null)
  const [picked, setPicked] = React.useState<Set<string>>(() => new Set())
  const [search, setSearch] = React.useState('')
  const candidateList = React.useRef<HTMLDivElement>(null)
  const query = search.trim().toLowerCase()
  const visibleCandidates = React.useMemo(() => (candidates ?? []).filter(candidate => (
    candidate.id.toLowerCase().includes(query) || candidate.display_name?.toLowerCase().includes(query)
  )), [candidates, query])
  React.useEffect(() => { if (candidateList.current) candidateList.current.scrollTop = 0 }, [query])
  const update = (index: number, next: Partial<ProviderModelDraft>) => onChange(models.map((model, at) => at === index ? { ...model, ...next } : model))

  const discover = async () => {
    setDiscovering(true); setDiscoveryError('')
    try {
      const found = await onDiscover()
      if (!found.length) { setDiscoveryError(t('provider.discoverEmpty')); return }
      setCandidates(found)
      setPicked(new Set(found.map(model => model.id)))
      setSearch('')
    } catch (cause) {
      setDiscoveryError(cause instanceof Error ? cause.message : String(cause))
    } finally { setDiscovering(false) }
  }
  const adopt = () => {
    if (!candidates) return
    const merged = mergeDiscoveredModels(models, candidates, picked)
    const expandedIds = new Set(models.filter((_model, index) => expanded.has(index)).map(model => model.id))
    setExpanded(new Set(merged.flatMap((model, index) => expandedIds.has(model.id) ? [index] : [])))
    onChange(merged)
    setCandidates(null)
  }
  const remove = (index: number) => {
    onChange(models.filter((_item, at) => at !== index))
    setExpanded(current => new Set([...current].flatMap(at => at < index ? [at] : at > index ? [at - 1] : [])))
  }
  const allPicked = visibleCandidates.length > 0 && visibleCandidates.every(candidate => picked.has(candidate.id))
  const toggleMatches = () => setPicked(current => {
    const next = new Set(current)
    const remove = visibleCandidates.every(candidate => current.has(candidate.id))
    for (const candidate of visibleCandidates) {
      if (remove) next.delete(candidate.id)
      else next.add(candidate.id)
    }
    return next
  })

  return <section className="mt-5 border-t pt-5" aria-label={t('provider.catalog')}>
    <div className="mb-3 flex flex-wrap items-center justify-between gap-2">
      <div><div className="text-sm font-medium">{t('provider.catalog')}</div><p className="mt-1 text-xs text-muted-foreground">{t('provider.catalogDescription')}</p></div>
      <Button type="button" size="xs" variant="outline" disabled={discovering} onClick={() => void discover()}>{discovering ? <LoaderCircle className="animate-spin" /> : <DownloadCloud />}{discovering ? t('provider.discovering') : t('provider.discover')}</Button>
    </div>
    {discoveryError ? <p className="mb-3 text-xs text-destructive" role="alert">{discoveryError}</p> : null}
    <div className="space-y-2">
      {models.map((model, index) => {
        const open = expanded.has(index)
        return <div className="rounded-lg border bg-background/35" key={index}>
          <div className="grid grid-cols-[minmax(0,1fr)_auto_auto] gap-2 p-3 sm:grid-cols-[minmax(0,1fr)_minmax(0,1fr)_auto_auto] sm:items-center">
            <Input className="col-span-3 sm:col-span-1" aria-label={`${t('provider.modelId')} ${index + 1}`} value={model.id} placeholder={t('provider.modelId')} onChange={event => update(index, { id: event.target.value, ...(event.target.value.trim() !== model.id.trim() ? { upstream: {} } : {}) })} />
            <Input className="col-span-3 sm:col-span-1" aria-label={`${t('provider.modelDisplayName')} ${index + 1}`} value={model.displayName} placeholder={t('provider.modelDisplayName')} onChange={event => update(index, { displayName: event.target.value })} />
            <Button type="button" size="icon-sm" variant="ghost" className="col-start-2 sm:col-start-auto" aria-label={t('provider.modelDetails', { index: index + 1 })} aria-expanded={open} onClick={() => setExpanded(previous => { const next = new Set(previous); if (!next.delete(index)) next.add(index); return next })}><ChevronDown className={cn('transition-transform', open && 'rotate-180')} /></Button>
            <Button type="button" size="icon-sm" variant="ghost" aria-label={t('provider.deleteModel', { index: index + 1 })} onClick={() => remove(index)}><Trash2 /></Button>
          </div>
          {open ? <div className="border-t p-4">
            <ModelSettingsEditor idPrefix={`${fieldId}-model-${index}`} defaults={defaults} model={model} onChange={overrides => update(index, { overrides })} />
          </div> : null}
        </div>
      })}
    </div>
    <Button type="button" className="mt-3" size="sm" variant="outline" onClick={() => {
      onChange([...models, { id: '', displayName: '', upstream: {}, overrides: {} }])
      setExpanded(previous => new Set([...previous, models.length]))
    }}><Plus />{t('provider.addModel')}</Button>
    <Dialog open={candidates !== null} onOpenChange={open => { if (!open) setCandidates(null) }}>
      <DialogContent className={cn(styles.settingsDialog, 'max-w-lg')} data-settings-dialog="">
        <DialogHeader><DialogTitle>{t('provider.discoverTitle')}</DialogTitle><DialogDescription>{t('provider.discoverDescription')}</DialogDescription></DialogHeader>
        <div className="flex items-center gap-2">
          <Input type="search" className="min-w-0 flex-1" aria-label={t('provider.searchDiscovered')} placeholder={t('provider.searchDiscovered')} value={search} onChange={event => setSearch(event.target.value)} onKeyDown={event => { if (event.key === 'Enter') event.preventDefault() }} />
          <Button type="button" size="sm" variant="ghost" className="shrink-0" disabled={visibleCandidates.length === 0} onClick={toggleMatches}>{query ? allPicked ? t('provider.deselectMatches') : t('provider.selectMatches') : allPicked ? t('provider.deselectAll') : t('provider.selectAll')}</Button>
        </div>
        <p className="text-xs text-muted-foreground" role="status">{t('provider.discoverySummary', { shown: visibleCandidates.length, total: candidates?.length ?? 0, selected: picked.size })}</p>
        <div ref={candidateList} className="max-h-[45dvh] space-y-1 overflow-y-auto rounded-lg border p-2" data-discovered-models="">
          {visibleCandidates.length === 0 && <p className="px-2 py-5 text-center text-sm text-muted-foreground">{t('provider.noMatchingModels')}</p>}
          {visibleCandidates.map(candidate => <label className="flex min-h-10 items-center gap-3 rounded-md px-2 hover:bg-muted" key={candidate.id}>
            <input type="checkbox" checked={picked.has(candidate.id)} onChange={() => setPicked(current => { const next = new Set(current); if (!next.delete(candidate.id)) next.add(candidate.id); return next })} />
            <span className="min-w-0 flex-1 truncate text-sm">{candidate.display_name || candidate.id}</span><code className="truncate text-[10px] text-muted-foreground">{candidate.id}</code>
          </label>)}
        </div>
        <DialogFooter><Button variant="outline" onClick={() => setCandidates(null)}>{common('cancel')}</Button><Button onClick={adopt}>{t('provider.addSelected')}</Button></DialogFooter>
      </DialogContent>
    </Dialog>
  </section>
}
