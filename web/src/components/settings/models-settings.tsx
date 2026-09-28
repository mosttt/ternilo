import * as React from 'react'
import { useWorkbench } from '@/state/workbench'
import type { Profile } from '@/types'
import { ModelPicker } from '@/components/workbench/model-picker'
import { useTranslate } from '@/i18n/provider'
import { ProviderLibrary } from './provider-library'
import styles from './settings-layout.module.css'
export { validateProviderDraft } from './provider-library'

export function ModelsSettings({ effectiveProfile }: { effectiveProfile?: Profile | null }) {
  const { currentSession, currentWorkspace } = useWorkbench()
  const t = useTranslate('settings')
  const target = React.useMemo(() => ({ sessionId: currentSession?.identity.session_id, workspaceId: currentWorkspace?.workspace_id }), [currentSession?.identity.session_id, currentWorkspace?.workspace_id])
  return <div className="space-y-8">
    <ProviderLibrary target={target} scope="local" title={t('nav.models')} description={t('models.description')} />
    <section className={styles.group}>
      <h3 className="text-sm font-semibold">{t('models.current')}</h3>
      <p className="mt-1 text-xs leading-relaxed text-muted-foreground">{t('models.currentDescription')}</p>
      <div className="mt-4 min-w-0 rounded-xl border bg-card p-4">{currentSession ? <><ModelPicker effectiveProfile={effectiveProfile} /><p className="mt-1 text-xs text-muted-foreground">{t('models.reasoningScope')}</p></> : <p className="text-sm text-muted-foreground">{t('models.noSession')}</p>}</div>
    </section>
  </div>
}
