import { ModelPicker } from '@/components/workbench/model-picker'
import { Field, FieldDescription, Input, Label, Select, Textarea } from '@/components/ui/field'
import { Switch } from '@/components/ui/switch'
import type { ExecutionTarget } from '@/domain/execution-target'
import { useTranslate } from '@/i18n/provider'
import type { ModelSelection } from '@/types'
import { configRecord } from './plugin-config'

const roles = ['investigator', 'implementer', 'reviewer'] as const
const modes = ['independent', 'assisted', 'parallel'] as const
const modeKeys = {
  independent: 'coordination.mode.independent', assisted: 'coordination.mode.assisted', parallel: 'coordination.mode.parallel',
} as const
const descriptionKeys = {
  independent: 'coordination.description.independent', assisted: 'coordination.description.assisted', parallel: 'coordination.description.parallel',
} as const
const roleKeys = {
  investigator: 'coordination.role.investigator', implementer: 'coordination.role.implementer', reviewer: 'coordination.role.reviewer',
} as const

export function coordinationErrorKey(message: string) {
  if (message.includes('coordination parallel limit')) return 'coordination.error.parallel' as const
  if (message.includes('Independent coordination')) return 'coordination.error.independent' as const
  if (message.includes('max_parallel')) return 'coordination.error.limit' as const
  if (message.includes('agent role instructions exceed')) return 'coordination.error.instructions' as const
  if (message.includes('role') && (message.includes('authorization') || message.includes('Server model') || message.includes('snapshot differs'))) return 'coordination.error.model' as const
  return null
}

export function CoordinationFields({ value, disabled, id, target, onChange }: {
  value: unknown
  disabled: boolean
  id: string
  target?: ExecutionTarget
  onChange(value: Record<string, unknown>): void
}) {
  const t = useTranslate('settings')
  const config = configRecord(value)
  const mode = modes.includes(config.mode as typeof modes[number]) ? config.mode as typeof modes[number] : 'assisted'
  const edit = (patch: Record<string, unknown>) => {
    const { resolved_models: _resolved, ...settings } = config
    onChange({ ...settings, ...patch })
  }
  return <div className="grid min-w-0 gap-5" data-coordination-settings="">
    <Field>
      <Label htmlFor={`${id}-mode`}>{t('coordination.mode')}</Label>
      <Select id={`${id}-mode`} value={mode} disabled={disabled} onValueChange={next => edit({ mode: next })}>
        {modes.map(item => <option key={item} value={item}>{t(modeKeys[item])}</option>)}
      </Select>
      <FieldDescription>{t(descriptionKeys[mode])}</FieldDescription>
    </Field>
    {mode !== 'independent' && <>
      <Field>
        <Label htmlFor={`${id}-parallel`}>{t('coordination.parallel')}</Label>
        <Input id={`${id}-parallel`} type="number" min={1} max={32} value={String(config.max_parallel ?? (mode === 'parallel' ? 8 : 4))} disabled={disabled}
          onChange={event => edit({ max_parallel: Number(event.target.value) })} />
      </Field>
      <p className="text-xs leading-relaxed text-muted-foreground">{t('coordination.lead')}</p>
      {roles.map(role => {
        const settings = configRecord(config[role])
        const model = settings.model as ModelSelection | null | undefined
        const editRole = (patch: Record<string, unknown>) => edit({ [role]: { ...settings, ...patch } })
        return <fieldset key={role} className="grid min-w-0 gap-3 rounded-lg border p-3">
          <legend className="px-1 text-sm font-medium">{t(roleKeys[role])}</legend>
          <Field>
            <Label>{t('coordination.model')}</Label>
            <ModelPicker selectionOverride={model ?? { provider: 'profile_default' }}
              inheritedLabel={t('coordination.inherit')} targetOverride={target} roleOverride={role} disabled={disabled}
              onSelectOverride={next => editRole({ model: next.provider === 'profile_default' ? null : next })} />
          </Field>
          <div className="flex items-center gap-3">
            <Switch id={`${id}-${role}-readonly`} disabled={disabled} checked={Boolean(settings.read_only ?? role !== 'implementer')}
              onCheckedChange={read_only => editRole({ read_only })} />
            <Label htmlFor={`${id}-${role}-readonly`}>{t('coordination.readOnly')}</Label>
          </div>
          <Field>
            <Label htmlFor={`${id}-${role}-instructions`}>{t('coordination.instructions')}</Label>
            <Textarea id={`${id}-${role}-instructions`} disabled={disabled} value={String(settings.instructions ?? '')}
              onChange={event => editRole({ instructions: event.target.value })} />
          </Field>
        </fieldset>
      })}
      <FieldDescription>{t('coordination.scope')}</FieldDescription>
    </>}
  </div>
}
