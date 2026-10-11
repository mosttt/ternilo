import { Field, FieldDescription, Input, Label } from '@/components/ui/field'
import { Switch } from '@/components/ui/switch'
import { useTranslate } from '@/i18n/provider'
import { configRecord } from './plugin-config'

export function ToolSchemaFields({ value, disabled, id, onChange }: {
  value: unknown
  disabled: boolean
  id: string
  onChange(value: Record<string, unknown>): void
}) {
  const t = useTranslate('settings')
  const config = configRecord(value)
  const enabled = Boolean(config.enabled)
  return <div className="grid min-w-0 gap-4" data-tool-schema-settings="">
    <Field>
      <div className="flex items-center gap-3">
        <Switch id={`${id}-enabled`} disabled={disabled} checked={enabled}
          onCheckedChange={enabled => onChange({ ...config, enabled })} />
        <Label htmlFor={`${id}-enabled`}>{t('toolSchemas.enabled')}</Label>
      </div>
      <FieldDescription>{t('toolSchemas.description')}</FieldDescription>
    </Field>
    {enabled && <Field>
      <Label htmlFor={`${id}-loaded`}>{t('toolSchemas.loaded')}</Label>
      <Input id={`${id}-loaded`} type="number" min={1} max={128} disabled={disabled}
        value={String(config.max_loaded ?? 32)} onChange={event => onChange({ ...config, max_loaded: Number(event.target.value) })} />
      <FieldDescription>{t('toolSchemas.loadedDescription')}</FieldDescription>
    </Field>}
  </div>
}
