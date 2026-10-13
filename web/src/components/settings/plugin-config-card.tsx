import { usePluginDraft, type PluginDraft } from '@/state/plugin-drafts'
import { pluginFieldCopy } from './plugin-field-copy'
import { ToolSchemaFields } from './tool-schema-fields'
import * as React from 'react'
import { ChevronDown, RotateCcw } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { Field, FieldDescription, Input, Label, Select, Textarea } from '@/components/ui/field'
import { Switch } from '@/components/ui/switch'
import { useTranslate } from '@/i18n/provider'
import type { PluginCatalogEntry, PluginEntry } from '@/types'
import { cn } from '@/lib/utils'
import {
  configRecord,
  fieldText,
  parseFieldText,
  pluginConfigFields,
  schemaType,
} from './plugin-config'
import styles from './plugin-config-card.module.css'
import { CoordinationFields, coordinationErrorKey } from './coordination-fields'
import type { ExecutionTarget } from '@/domain/execution-target'

function initialTexts(config: Record<string, unknown>, fields: ReturnType<typeof pluginConfigFields>) {
  return Object.fromEntries(fields.map((field) => {
    const type = schemaType(field.schema)
    return [field.key, fieldText(config[field.key] ?? field.schema.default, type)]
  }))
}

function effectiveToolCallLimit(configured: unknown, host: number | undefined) {
  if (typeof configured !== 'number' || !Number.isInteger(configured) || configured < 0
    || host === undefined || !Number.isInteger(host) || host < 0) return undefined
  if (host === 0) return configured
  return configured === 0 ? host : Math.min(configured, host)
}

export function PluginConfigCard({
  entry,
  expanded = false,
  draftKey,
  metadata,
  overridden,
  title,
  showToggle = true,
  openRequest,
  saveLabel,
  savingLabel,
  disabledReason,
  hostToolCallLimit,
  target,
  onToggle,
  onSave,
  onReset,
}: {
  entry: PluginEntry
  expanded?: boolean
  draftKey?: string
  metadata: PluginCatalogEntry
  overridden: boolean
  title?: string
  showToggle?: boolean
  openRequest?: number
  saveLabel?: string
  savingLabel?: string
  disabledReason?: string
  hostToolCallLimit?: number
  target?: ExecutionTarget
  onToggle?(enabled: boolean): Promise<void>
  onSave(entry: PluginEntry): Promise<void>
  onReset?(): Promise<void>
}) {
  const t = useTranslate('settings')
  const common = useTranslate('common')
  const baseline = JSON.stringify(configRecord(entry.config))
  const baselineText = JSON.stringify(configRecord(entry.config), null, 2)
  const schemaFingerprint = JSON.stringify(metadata.config_schema)
  const fields = React.useMemo(() => pluginConfigFields(metadata.config_schema), [schemaFingerprint])
  const baselineDraft = React.useMemo(() => configRecord(entry.config), [baseline])
  const baselineTexts = React.useMemo(
    () => initialTexts(baselineDraft, fields),
    [baselineDraft, fields],
  )
  const [open, setOpen] = React.useState(false)
  const initial: PluginDraft = { draft: baselineDraft, texts: baselineTexts, rawText: baselineText, rawInvalid: false, touched: new Set() }
  const [editor, setEditor] = usePluginDraft(draftKey, baseline, initial)
  const { draft, texts, rawText, rawInvalid, touched } = editor
  const updateField = <K extends keyof PluginDraft>(key: K, action: React.SetStateAction<PluginDraft[K]>) => {
    setEditor(current => ({ ...current, [key]: typeof action === 'function' ? (action as (value: PluginDraft[K]) => PluginDraft[K])(current[key]) : action }))
  }
  const setDraft = (value: React.SetStateAction<PluginDraft['draft']>) => updateField('draft', value)
  const setTexts = (value: React.SetStateAction<PluginDraft['texts']>) => updateField('texts', value)
  const setRawText = (value: string) => updateField('rawText', value)
  const setRawInvalid = (value: boolean) => updateField('rawInvalid', value)
  const setTouched = (value: React.SetStateAction<PluginDraft['touched']>) => updateField('touched', value)
  const [busy, setBusy] = React.useState(false)
  const [error, setError] = React.useState('')
  React.useEffect(() => {
    if (openRequest !== undefined) setOpen(true)
  }, [openRequest])
  const restore = () => { setEditor(initial); setError('') }
  const visible = expanded || open
  const Heading = expanded ? 'div' : 'button'

  const preview = React.useMemo(() => {
    const next = { ...draft }
    if (rawInvalid) return { next, invalid: '__raw' }
    for (const key of touched) {
      const field = fields.find((candidate) => candidate.key === key)
      if (!field || schemaType(field.schema) === 'boolean') continue
      try { next[key] = parseFieldText(field.schema, texts[key] ?? '') }
      catch { return { next, invalid: key } }
    }
    for (const field of fields) {
      if (field.required && next[field.key] === undefined && field.schema.default === undefined) {
        return { next, invalid: field.key }
      }
    }
    return { next, invalid: '' }
  }, [draft, fields, rawInvalid, texts, touched])
  const dirty = touched.size > 0 || JSON.stringify(draft) !== baseline || rawText !== baselineText

  const save = async () => {
    if (preview.invalid) { setError(t('plugins.invalidField', { name: preview.invalid })); return }
    setBusy(true)
    setError('')
    try {
      await onSave({ ...entry, config: preview.next })
      setEditor({ draft: preview.next, texts: initialTexts(preview.next, fields), rawText: JSON.stringify(preview.next, null, 2), rawInvalid: false, touched: new Set() })
      setOpen(false)
    } catch (cause) {
      const message = cause instanceof Error ? cause.message : String(cause)
      const localized = entry.kind === 'ternilo.subagents.in_process' ? coordinationErrorKey(message) : null
      setError(localized ? t(localized) : entry.kind === 'ternilo.tools.registry' && message.includes('tool schema') ? t('toolSchemas.invalid') : message)
    } finally {
      setBusy(false)
    }
  }

  const resetOverride = async () => {
    if (!onReset) return
    setBusy(true)
    setError('')
    try {
      await onReset()
      restore()
      setOpen(false)
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause))
    } finally {
      setBusy(false)
    }
  }

  const toggle = async (enabled: boolean) => {
    if (!onToggle) return
    setBusy(true)
    setError('')
    try { await onToggle(enabled) }
    catch (cause) { setError(cause instanceof Error ? cause.message : String(cause)) }
    finally { setBusy(false) }
  }

  const editText = (key: string, text: string) => {
    setTexts((current) => ({ ...current, [key]: text }))
    setTouched((current) => new Set(current).add(key))
  }

  const resetField = (key: string, defaultValue: unknown, type: string) => {
    setDraft((current) => {
      const next = { ...current }
      delete next[key]
      return next
    })
    setTexts((current) => ({ ...current, [key]: fieldText(defaultValue, type) }))
    setTouched((current) => {
      const next = new Set(current)
      next.delete(key)
      return next
    })
  }

  return (
    <article className={cn(styles.card, visible && styles.open)} data-plugin-id={entry.id}>
      <div className={styles.header}>
        <Heading
          {...(!expanded ? { type: 'button' as const, 'aria-expanded': open, 'aria-label': `${open ? common('collapse') : common('expand')}: ${title ?? entry.id}`, onClick: () => setOpen(value => !value) } : {})}
          className={styles.disclosure}
        >
          <span className="min-w-0 flex-1">
            <span className="flex flex-wrap items-center gap-2">
              <strong className="text-sm">{title ?? entry.id}</strong>
              <code className="max-w-full break-all rounded bg-muted px-1.5 py-0.5 text-[10px] text-muted-foreground">{entry.kind}</code>
              {dirty ? <span className="rounded bg-warning/15 px-1.5 py-0.5 text-[10px] text-warning">{t('plugins.unsaved')}</span> : null}
              {overridden ? <span className="rounded bg-primary/15 px-1.5 py-0.5 text-[10px] text-primary">{t('plugins.overridden')}</span> : null}
            </span>
            <span className="mt-1 block text-xs leading-relaxed text-muted-foreground">{metadata.description || t('plugins.noDescription')}</span>
          </span>
          {!expanded && <ChevronDown className={cn('size-4 shrink-0 transition-transform', open && 'rotate-180')} />}
        </Heading>
        {showToggle ? (
          <Switch
            checked={entry.enabled}
            disabled={busy || Boolean(disabledReason)}
            aria-label={t(entry.enabled ? 'plugins.disable' : 'plugins.enable', { name: title ?? entry.id })}
            onCheckedChange={(checked) => void toggle(checked)}
          />
        ) : null}
      </div>
      {visible ? (
        <div className={styles.body}>
          {!expanded && (metadata.requires.length || metadata.provides.length) ? (
            <dl className="mb-5 grid gap-3 rounded-lg bg-muted/25 p-3 text-xs sm:grid-cols-2">
              <div><dt className="text-muted-foreground">{t('plugins.requiresLabel')}</dt><dd className="mt-1 break-words font-mono">{metadata.requires.join(' · ') || '—'}</dd></div>
              <div><dt className="text-muted-foreground">{t('plugins.providesLabel')}</dt><dd className="mt-1 break-words font-mono">{metadata.provides.join(' · ') || '—'}</dd></div>
            </dl>
          ) : null}

          {fields.length ? (
            <div className="grid gap-5">
              {fields.map((field) => {
                if (entry.kind === 'ternilo.tools.registry' && field.key === 'schema_loading') {
                  return <ToolSchemaFields key={field.key} id={`plugin-${entry.id}-schemas`} value={draft.schema_loading} disabled={busy}
                    onChange={schema_loading => setDraft(current => ({ ...current, schema_loading }))} />
                }
                if (entry.kind === 'ternilo.subagents.in_process' && field.key === 'coordination') {
                  return <CoordinationFields key={field.key} id={`plugin-${entry.id}-coordination`} target={target}
                    value={draft.coordination} childLimit={preview.next.max_children ?? fields.find(item => item.key === 'max_children')?.schema.default} disabled={busy}
                    onChange={coordination => setDraft(current => ({ ...current, coordination }))} />
                }
                const type = schemaType(field.schema)
                const value = draft[field.key]
                const hasOverride = value !== undefined || touched.has(field.key)
                const { label, description } = pluginFieldCopy(entry.kind, field, t)
                const invalid = preview.invalid === field.key
                const configuredLimit = preview.next[field.key] ?? field.schema.default
                const effectiveToolLimit = entry.kind === 'ternilo.agent.react' && field.key === 'max_tool_calls' && !invalid
                  ? effectiveToolCallLimit(configuredLimit, hostToolCallLimit)
                  : undefined
                const hostConstrains = Boolean(hostToolCallLimit && typeof configuredLimit === 'number'
                  && (configuredLimit === 0 || configuredLimit > hostToolCallLimit))
                return (
                  <Field key={field.key}>
                    <div className="flex min-w-0 items-center gap-2">
                      <Label htmlFor={`plugin-${entry.id}-${field.key}`} className="min-w-0 flex-1">{label}{field.required ? ' *' : ''}</Label>
                      {hasOverride ? (
                        <Button type="button" size="xs" variant="ghost" disabled={busy} onClick={() => resetField(field.key, field.schema.default, type)}>
                          <RotateCcw />{t('plugins.resetField')}
                        </Button>
                      ) : null}
                    </div>
                    {type === 'boolean' ? (
                      <Switch
                        id={`plugin-${entry.id}-${field.key}`}
                        checked={Boolean(value ?? field.schema.default)}
                        disabled={busy}
                        onCheckedChange={(checked) => setDraft((current) => ({ ...current, [field.key]: checked }))}
                      />
                    ) : field.schema.enum?.length ? (
                      <Select
                        id={`plugin-${entry.id}-${field.key}`}
                        value={texts[field.key] ?? ''}
                        disabled={busy}
                        aria-invalid={invalid || undefined}
                        onValueChange={(nextValue) => editText(field.key, nextValue)}
                      >
                        {field.schema.enum.map((choice) => <option key={JSON.stringify(choice)} value={String(choice)}>{String(choice)}</option>)}
                      </Select>
                    ) : type === 'object' || type === 'array' ? (
                      <Textarea
                        id={`plugin-${entry.id}-${field.key}`}
                        className="min-h-28 font-mono text-xs"
                        value={texts[field.key] ?? ''}
                        disabled={busy}
                        aria-invalid={invalid || undefined}
                        onChange={(event) => editText(field.key, event.target.value)}
                      />
                    ) : (
                      <Input
                        id={`plugin-${entry.id}-${field.key}`}
                        type="text"
                        inputMode={type === 'number' || type === 'integer' ? 'numeric' : undefined}
                        value={texts[field.key] ?? ''}
                        disabled={busy}
                        aria-invalid={invalid || undefined}
                        onChange={(event) => editText(field.key, event.target.value)}
                      />
                    )}
                    <FieldDescription className={invalid ? 'text-destructive' : undefined}>
                      {invalid ? t('plugins.invalidField', { name: label }) : description}
                    </FieldDescription>
                    {effectiveToolLimit !== undefined && <FieldDescription data-tool-call-limit="">
                      {effectiveToolLimit === 0 ? t('plugins.toolLimit.unlimited') : t('plugins.toolLimit.effective', { limit: effectiveToolLimit })}
                      {hostConstrains ? ` ${t('plugins.toolLimit.host')}` : ''}
                    </FieldDescription>}
                  </Field>
                )
              })}
            </div>
          ) : Object.keys(configRecord(entry.config)).length ? (
            <Field>
              <Label htmlFor={`plugin-${entry.id}-raw`}>{t('plugins.rawConfig')}</Label>
              <Textarea
                id={`plugin-${entry.id}-raw`}
                className="min-h-36 font-mono text-xs"
                value={rawText}
                disabled={busy}
                aria-invalid={preview.invalid === '__raw' || undefined}
                onChange={(event) => {
                  setRawText(event.target.value)
                  try {
                    const parsed = JSON.parse(event.target.value)
                    if (typeof parsed !== 'object' || parsed === null || Array.isArray(parsed)) throw new Error('object')
                    setDraft(configRecord(parsed))
                    setRawInvalid(false)
                  } catch {
                    setRawInvalid(true)
                  }
                }}
              />
              {rawInvalid ? <FieldDescription className="text-destructive">{t('plugins.invalidJson')}</FieldDescription> : null}
            </Field>
          ) : <p className="text-xs text-muted-foreground">{t('plugins.noConfig')}</p>}

          {error ? <p className="mt-4 text-xs text-destructive" role="alert">{error}</p> : null}
          {disabledReason ? <p className="mt-4 text-xs leading-relaxed text-muted-foreground" role="status">{disabledReason}</p> : null}
          <div className="mt-5 flex flex-wrap items-center justify-end gap-2 border-t pt-4">
            {overridden && onReset ? <Button type="button" variant="ghost" disabled={busy || Boolean(disabledReason)} onClick={() => void resetOverride()}><RotateCcw />{t('plugins.resetOverride')}</Button> : null}
            <Button type="button" variant="outline" disabled={!dirty || busy} onClick={restore}>{t('plugins.discard')}</Button>
            <Button type="button" disabled={!dirty || busy || Boolean(preview.invalid) || Boolean(disabledReason)} onClick={() => void save()}>
              {busy ? savingLabel ?? common('saving') : saveLabel ?? common('save')}
            </Button>
          </div>
        </div>
      ) : null}
    </article>
  )
}
