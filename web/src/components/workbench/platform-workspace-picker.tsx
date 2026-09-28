import { Cloud, FolderOpen, Laptop, LoaderCircle, RotateCcw } from 'lucide-react'
import { navigate } from '@/app/navigation'
import { Button } from '@/components/ui/button'
import { Dialog, DialogContent, DialogDescription, DialogTitle } from '@/components/ui/dialog'
import { Field, Label, Select } from '@/components/ui/field'
import { useTranslate } from '@/i18n/provider'
import { LocalDirectoryPicker } from './local-directory-picker'
import { NEW_PROJECT, usePlatformWorkspaceSetup } from './use-platform-workspace-setup'
import { WorkspaceProjectFields } from './workspace-project-fields'
import styles from './directory-picker.module.css'

export function PlatformWorkspacePicker({ open, onOpenChange, createSessionAfter = false }: {
  open: boolean
  onOpenChange(open: boolean): void
  createSessionAfter?: boolean
}) {
  const t = useTranslate('workspace')
  const setup = usePlatformWorkspaceSetup({ open, onOpenChange, createSessionAfter })
  const { placement, saving, loading, browserOpen, executorId, selectedExecutor, path } = setup
  const newProject = setup.projectId === NEW_PROJECT
  const result = newProject
    ? t(createSessionAfter ? 'picker.result.newSession' : 'picker.result.newWorkspace')
    : t(createSessionAfter ? 'picker.result.session' : 'picker.result.workspace')

  return <>
    <Dialog open={open && !browserOpen} onOpenChange={next => { if (!saving) onOpenChange(next) }}>
      <DialogContent showClose={!saving} className={styles.dialog} aria-busy={saving || loading || undefined}>
        <header className={styles.header}>
          <DialogTitle className={styles.title}>{t('picker.placement.title')}</DialogTitle>
          <DialogDescription className={styles.description}>{t('picker.placement.description')}</DialogDescription>
          {setup.scopeName && <p className={styles.scope}>{t('picker.scope', { name: setup.scopeName })}</p>}
        </header>
        <form onSubmit={event => { event.preventDefault(); void setup.submit() }}>
          <fieldset disabled={saving || setup.workspaceCreated} className={styles.formBody}>
            {setup.managedExecution && <div className={styles.placementGrid}>
              {(['local_node', 'cloud'] as const).map(value => <button
                type="button" key={value} aria-pressed={placement === value}
                className={`${styles.placementCard} ${placement === value ? styles.placementCardActive : ''}`}
                onClick={() => setup.changePlacement(value)}
              >
                <span className={styles.placementTitle}>{value === 'cloud' ? <Cloud aria-hidden="true" /> : <Laptop aria-hidden="true" />}{t(value === 'cloud' ? 'picker.placement.cloud' : 'picker.placement.local')}</span>
                <span className={styles.placementDescription}>{t(value === 'cloud' ? 'picker.placement.cloudDescription' : 'picker.placement.localDescription')}</span>
              </button>)}
            </div>}
            <div className={styles.fields}>
              {placement === 'local_node' && <>
                <Field>
                  <div className={styles.fieldHeading}>
                    <Label htmlFor="workspace-executor">{t('picker.executor')}</Label>
                    <Button type="button" variant="ghost" size="sm" disabled={loading} onClick={() => void setup.load()}>
                      <RotateCcw aria-hidden="true" />{t('picker.refresh')}
                    </Button>
                  </div>
                  {setup.executors.length ? <Select id="workspace-executor" className={styles.select} value={executorId} disabled={saving || setup.workspaceCreated} onValueChange={value => setup.changeExecutor(value)}>
                    {!selectedExecutor && <option disabled value={executorId}>{t('picker.executor.unavailable')}</option>}
                    {setup.executors.map(executor => <option value={executor.executor_id} key={executor.executor_id}>
                      {t('picker.executor.option', { id: executor.executor_id, status: t(executor.connected ? 'status.online' : 'status.offline') })}
                    </option>)}
                  </Select> : !loading && <div className={styles.empty}>
                    <p>{t('picker.executor.empty')}</p>
                    <Button type="button" variant="outline" className={styles.secondaryAction} onClick={() => { onOpenChange(false); navigate('/settings/computers') }}>
                      <Laptop aria-hidden="true" />{t('picker.executor.connect')}
                    </Button>
                  </div>}
                  {selectedExecutor && !selectedExecutor.connected && <p role="alert" className={styles.warning}>{t('picker.executor.offline')}</p>}
                </Field>
                <Field>
                  <Label>{t('picker.directory')}</Label>
                  <div className={styles.directory}>
                    {path && <code className={styles.directoryPath} data-selected-directory="">{path}</code>}
                    <Button type="button" variant="outline" className={styles.secondaryAction} disabled={loading || !selectedExecutor?.connected} onClick={() => setup.setBrowserOpen(true)}>
                      <FolderOpen aria-hidden="true" />{t(path ? 'picker.directory.change' : 'picker.local.choose')}
                    </Button>
                  </div>
                  {selectedExecutor && <p className={styles.description}>{t('picker.directory.onComputer', { name: executorId })}</p>}
                </Field>
              </>}
              <WorkspaceProjectFields setup={setup} onManageProjects={() => { onOpenChange(false); navigate('/spaces/current?tab=projects') }} />
            </div>
          </fieldset>
          {setup.error && <div className={styles.errorRow}>
            <p role="alert" className={styles.error}>{setup.error}</p>
            <Button type="button" size="sm" variant="outline" className={styles.secondaryAction} disabled={saving || loading} onClick={() => void setup.load()}>
              <RotateCcw aria-hidden="true" />{t('picker.retry')}
            </Button>
          </div>}
          <footer className={styles.footer}>
            <p className={styles.result}>{setup.workspaceCreated ? t('picker.result.sessionRetry') : result}</p>
            <Button type="button" variant="outline" className={styles.secondaryAction} onClick={() => onOpenChange(false)} disabled={saving}>{t('picker.cancel')}</Button>
            <Button type="submit" disabled={!setup.canSubmit}>
              {saving && <LoaderCircle className={styles.spin} aria-hidden="true" />}
              {t(setup.workspaceCreated ? 'picker.submit.retrySession' : newProject
                ? createSessionAfter ? 'picker.submit.projectSession' : 'picker.submit.projectWorkspace'
                : createSessionAfter ? 'picker.submit.session' : 'picker.submit.workspace')}
            </Button>
          </footer>
        </form>
      </DialogContent>
    </Dialog>
    {executorId && <LocalDirectoryPicker
      key={executorId}
      open={open && browserOpen}
      onOpenChange={setup.setBrowserOpen}
      apiBase={`/executors/${encodeURIComponent(executorId)}/directories`}
      initialPath={path || undefined}
      onChoosePath={setup.choosePath}
      title={t('picker.nodeBrowser.title', { id: executorId })}
      description={t('picker.nodeBrowser.description')}
      confirmLabel={t('picker.directory.confirm')}
    />}
  </>
}
