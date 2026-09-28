import { CloudCog, Laptop } from 'lucide-react'
import { useTranslate } from '@/i18n/provider'
import { workspaceDisplayPath } from '@/domain/workspace-display'
import { useWorkbench } from '@/state/workbench'
import { GroupHeader, SectionHeader, SettingRow } from './settings-ui'
import styles from './settings-layout.module.css'

export function AboutSettings() {
  const { catalog, currentSession, currentWorkspace, remote, platform } = useWorkbench()
  const t = useTranslate('settings')
  const workspaceT = useTranslate('workspace')
  const connection = platform ? t('about.control') : remote ? t('about.remote') : t('about.local')
  const connectionDescription = platform
    ? t('about.controlDescription')
    : remote ? t('about.remoteDescription') : t('about.localDescription')
  const placement = !currentWorkspace
    ? '—'
    : currentWorkspace.placement === 'cloud' ? t('about.cloud') : t('about.computer')
  return (
    <div className={styles.section}>
      <SectionHeader title={t('nav.about')} description={t('about.description')} />
      <section className="overflow-hidden rounded-xl border bg-card px-4">
        <SettingRow
          title={t('about.connection')}
          description={connectionDescription}
        >
          <span className="inline-flex items-center gap-2 text-sm">
            {platform || remote ? <CloudCog className="size-4" /> : <Laptop className="size-4" />}
            {connection}
          </span>
        </SettingRow>
        <SettingRow
          title={t('about.workspace')}
          description={workspaceDisplayPath(currentWorkspace, currentSession, workspaceT) || t('about.notSelected')}
        >
          <span className="text-sm">{placement}</span>
        </SettingRow>
        <SettingRow title={t('about.catalog')} description={t('about.catalogDescription')}>
          <code className="text-xs">{catalog?.revision ?? '—'}</code>
        </SettingRow>
        <SettingRow
          title={t('about.session')}
          description={currentSession?.identity.session_id ?? t('about.notSelected')}
        >
          <span className="text-sm">
            {currentSession?.mode === 'plan' ? t('about.plan') : t('about.execution')}
          </span>
        </SettingRow>
      </section>
      <section className={styles.group}>
        <GroupHeader title={t('about.boundary')} />
        <div className="rounded-xl border bg-muted/20 p-4 text-sm leading-relaxed text-muted-foreground">
          {t('about.boundaryText')}
        </div>
      </section>
    </div>
  )
}
