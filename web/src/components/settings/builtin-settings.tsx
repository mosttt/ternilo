import { Blocks, Bot, KeyRound, Laptop, Settings2, SlidersHorizontal, Sparkles } from 'lucide-react'
import { registerSettingsSection } from '@/plugins/settings-registry'
import { AboutSettings } from './about-settings'
import { CredentialsSettings } from './credentials-settings'
import { GeneralSettings } from './general-settings'
import { ModelsSettings } from './models-settings'
import { PlatformComputersSettings } from './platform-computers-settings'
import { PluginsSettings } from './plugins-settings'
import { PresetsSettings } from './presets-settings'

registerSettingsSection({
  id: 'general', order: 10, icon: Settings2,
  label: t => t('nav.general'),
  render: () => <GeneralSettings />,
})
registerSettingsSection({
  id: 'models', order: 20, icon: Sparkles,
  label: t => t('nav.models'),
  visible: context => !context.platform && context.targetOwner !== false,
  render: context => <ModelsSettings effectiveProfile={context.effectiveProfile} />,
})
registerSettingsSection({
  id: 'plugins', order: 30, icon: Blocks,
  label: t => t('nav.plugins'),
  visible: context => context.targetOwner !== false,
  render: context => <PluginsSettings onChanged={context.onSessionChanged} />,
})
registerSettingsSection({
  id: 'presets', order: 40, icon: Bot,
  label: t => t('nav.presets'),
  visible: context => context.targetOwner !== false,
  render: () => <PresetsSettings />,
})
registerSettingsSection({
  id: 'credentials', order: 50, icon: KeyRound,
  label: t => t('nav.credentials'),
  visible: context => context.targetOwner !== false,
  render: () => <CredentialsSettings />,
})
registerSettingsSection({
  id: 'computers', order: 55, icon: Laptop,
  label: t => t('nav.computers'),
  visible: context => context.platform && context.tenantRole !== null && context.tenantRole !== 'viewer',
  render: context => context.currentTenantId
    ? <PlatformComputersSettings tenantId={context.currentTenantId} scope="owned" />
    : null,
})
registerSettingsSection({
  id: 'about', order: 70, icon: SlidersHorizontal,
  label: t => t('nav.about'),
  render: () => <AboutSettings />,
})
