import type { LucideIcon } from 'lucide-react'
import type { ReactNode } from 'react'
import type { Translate } from '@/i18n/runtime'
import type { Profile, TenantRole } from '@/types'
import { ContributionRegistry } from './contribution-registry'

export interface SettingsVisibilityContext {
  platform: boolean
  instanceOwner?: boolean
  targetOwner?: boolean
  tenantRole: TenantRole | null
}

export interface SettingsRenderContext extends SettingsVisibilityContext {
  currentTenantId: string | null
  effectiveProfile?: Profile | null
  onSessionChanged(): Promise<void>
}

export interface SettingsSectionContribution {
  id: string
  order: number
  icon: LucideIcon
  label(t: Translate<'settings'>): string
  render(context: SettingsRenderContext): ReactNode
  visible?(context: SettingsVisibilityContext): boolean
}

export const settingsSectionRegistry = new ContributionRegistry<SettingsSectionContribution>(
  contribution => contribution.id,
  (left, right) => left.order - right.order,
)

export const registerSettingsSection = settingsSectionRegistry.register

export function visibleSettingsSections(
  context: SettingsVisibilityContext,
  entries: readonly SettingsSectionContribution[] = settingsSectionRegistry.getSnapshot(),
) {
  return entries.filter(entry => entry.visible?.(context) ?? true)
}
