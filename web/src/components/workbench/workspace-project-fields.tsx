import { FolderCog, Plus } from 'lucide-react'
import { ChoiceSelect } from '@/components/ui/choice-select'
import { Button } from '@/components/ui/button'
import { Field, Input, Label } from '@/components/ui/field'
import { useTranslate } from '@/i18n/provider'
import { NEW_PROJECT, type usePlatformWorkspaceSetup } from './use-platform-workspace-setup'
import styles from './directory-picker.module.css'

type WorkspaceSetup = ReturnType<typeof usePlatformWorkspaceSetup>

export function WorkspaceProjectFields({ setup, onManageProjects }: { setup: WorkspaceSetup; onManageProjects(): void }) {
  const t = useTranslate('workspace')
  const { projects, projectId, projectName, canCreateProject } = setup
  return <>
    <Field>
      <Label htmlFor="workspace-project">{t('picker.project')}</Label>
      <ChoiceSelect id="workspace-project" value={projectId} onValueChange={setup.setProjectId} placeholder={t('picker.project.unavailable')} options={[
        ...projects.map(project => ({ value: project.project_id, label: project.name })),
        ...(canCreateProject ? [{ value: NEW_PROJECT, label: t('picker.project.new') }] : []),
      ]} />
      <p className={styles.description}>{t('picker.project.description')}</p>
      {canCreateProject && <Button type="button" size="sm" variant="outline" className={`${styles.secondaryAction} justify-self-start`} onClick={onManageProjects}><FolderCog aria-hidden="true" />{t('picker.project.manage')}</Button>}
      {!projects.length && !canCreateProject && <p className={styles.empty}>{t('picker.project.memberEmpty')}</p>}
      {setup.projectRestricted && <p role="alert" className={styles.warning}>{t('picker.project.restricted', { name: setup.boundProjectName ?? '' })}</p>}
    </Field>
    {projectId === NEW_PROJECT && canCreateProject && <Field>
      <Label htmlFor="new-cloud-project">{t('picker.project.placeholder')}</Label>
      <Input id="new-cloud-project" value={projectName} onChange={event => setup.setProjectName(event.target.value)} placeholder={t('picker.project.placeholder')} />
      <div className={styles.projectOnly}>
        <span>{t('picker.project.onlyDescription')}</span>
        <Button type="button" size="sm" variant="outline" className={styles.secondaryAction} disabled={setup.loading || !projectName.trim()} onClick={() => void setup.submit(true)}>
          <Plus aria-hidden="true" />{t('picker.project.only')}
        </Button>
      </div>
    </Field>}
    <Field>
      <Label htmlFor="workspace-name">{t('picker.workspace.name')}</Label>
      <Input id="workspace-name" value={setup.name} onChange={event => setup.setName(event.target.value)} placeholder={t('picker.workspace.placeholder')} />
      <p className={styles.description}>{t('picker.workspace.nameHint')}</p>
    </Field>
  </>
}
