import * as React from 'react'
import { PlatformServiceAccounts } from './platform-service-accounts'
import { useSearch } from '@/app/navigation'
import { PlatformProjectsSettings } from './platform-projects-settings'
import { useTranslate } from '@/i18n/provider'
import { PlatformComputersSettings } from './platform-computers-settings'
import { PlatformAuditSettings, PlatformQuotaSettings } from './platform-governance-settings'
import { PlatformMembersSettings } from './platform-members-settings'
import { PlatformGroupsSettings } from './platform-groups-settings'
import { PlatformUsageSettings } from './platform-usage-settings'
import { SectionHeader, SettingsTabPanel, SettingsTabs } from './settings-ui'
import { InvitationPanel } from '@/components/admin/invitation-panel'
import type { TenantRole } from '@/types'
import styles from './settings-layout.module.css'

type PlatformTab = 'serviceAccounts' | 'members' | 'groups' | 'computers' | 'projects' | 'quota' | 'usage' | 'audit'

export function PlatformSettings({ tenantId, role, kind }: { tenantId: string; role: Extract<TenantRole, 'admin' | 'owner'>; kind: 'personal' | 'team' }) {
  const t = useTranslate('settings')
  const serviceT = useTranslate('serviceAccounts')
  const adminT = useTranslate('admin')
  const workspaceT = useTranslate('workspace')
  const search = useSearch()
  const [tab, setTab] = React.useState<PlatformTab>(() => new URLSearchParams(search).get('tab') === 'projects' ? 'projects' : kind === 'team' ? 'members' : 'computers')
  React.useEffect(() => { if (new URLSearchParams(search).get('tab') === 'projects') setTab('projects') }, [search])
  const tabs = React.useMemo(() => [
    ...(kind === 'team' ? [{ id: 'members', label: t('platform.members') }, { id: 'groups', label: t('groups.title') }] : []),
    { id: 'serviceAccounts', label: serviceT('title') },
    { id: 'computers', label: t('platform.computers') },
    { id: 'projects', label: workspaceT('picker.project') },
    { id: 'quota', label: t('platform.quota') },
    { id: 'usage', label: t('platform.usage') },
    { id: 'audit', label: t('platform.audit') },
  ], [kind, t, workspaceT, serviceT])

  return (
    <div className={styles.section} data-platform-settings="">
      <SectionHeader title={adminT('space.manage')} description={adminT(kind === 'personal' ? 'space.personalDescription' : 'space.teamDescription')} />
      <SettingsTabs
        label={t('platform.tabs')}
        tabs={tabs}
        active={tab}
        onChange={id => setTab(id as PlatformTab)}
      />
      <SettingsTabPanel id="members" active={tab}>
        {tab === 'members' && kind === 'team' ? <div className="grid gap-6"><InvitationPanel key={tenantId} tenantId={tenantId} /><PlatformMembersSettings key={tenantId} tenantId={tenantId} actorRole={role} /></div> : null}
      </SettingsTabPanel>
      <SettingsTabPanel id="groups" active={tab}>
        {tab === 'groups' && kind === 'team' ? <PlatformGroupsSettings key={tenantId} tenantId={tenantId} /> : null}
      </SettingsTabPanel>
      <SettingsTabPanel id="serviceAccounts" active={tab}>
        {tab === 'serviceAccounts' ? <PlatformServiceAccounts key={tenantId} tenantId={tenantId} /> : null}
      </SettingsTabPanel>
      <SettingsTabPanel id="computers" active={tab}>
        {tab === 'computers' ? <PlatformComputersSettings key={tenantId} tenantId={tenantId} /> : null}
      </SettingsTabPanel>
      <SettingsTabPanel id="projects" active={tab}>
        {tab === 'projects' ? <PlatformProjectsSettings key={tenantId} tenantId={tenantId} /> : null}
      </SettingsTabPanel>
      <SettingsTabPanel id="quota" active={tab}>
        {tab === 'quota' ? <PlatformQuotaSettings key={tenantId} tenantId={tenantId} editable={role === 'owner'} /> : null}
      </SettingsTabPanel>
      <SettingsTabPanel id="usage" active={tab}>
        {tab === 'usage' ? <PlatformUsageSettings key={tenantId} tenantId={tenantId} /> : null}
      </SettingsTabPanel>
      <SettingsTabPanel id="audit" active={tab}>
        {tab === 'audit' ? <PlatformAuditSettings key={tenantId} tenantId={tenantId} /> : null}
      </SettingsTabPanel>
    </div>
  )
}
