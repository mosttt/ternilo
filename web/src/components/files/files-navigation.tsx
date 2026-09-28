import * as React from 'react'
import { FileInput, Files, Folder, FolderOutput, MessageSquare } from 'lucide-react'
import { navigate } from '@/app/navigation'
import { Field, Label, Select } from '@/components/ui/field'
import { useTranslate } from '@/i18n/provider'
import { useWorkbench } from '@/state/workbench'
import { filesLocation, type FileFilters } from './files-api'
import css from './files-page.module.css'

export function FilesNavigation({ filters, mobile = false }: { filters: FileFilters; mobile?: boolean }) {
  const t = useTranslate('files')
  const { snapshot } = useWorkbench()
  const id = React.useId()
  const change = (values: Partial<FileFilters>) => navigate(filesLocation({ ...filters, ...values }))
  const workspaces = snapshot.workspaces.map(workspace => ({ id: workspace.workspace_id, name: workspace.title || workspace.path }))
  const sessions = snapshot.sessions.filter(session => !filters.workspace_id || session.workspace_id === filters.workspace_id)
    .map(session => ({ id: session.identity.session_id, name: session.title }))
  if (filters.workspace_id && !workspaces.some(workspace => workspace.id === filters.workspace_id)) workspaces.unshift({ id: filters.workspace_id, name: t('selectedWorkspace') })
  if (filters.session_id && !sessions.some(session => session.id === filters.session_id)) sessions.unshift({ id: filters.session_id, name: t('selectedSession') })
  const kinds = [{ id: '' as const, name: t('allKinds'), icon: Files }, { id: 'upload' as const, name: t('upload'), icon: FileInput }, { id: 'generated' as const, name: t('generated'), icon: FolderOutput }]
  if (mobile) return <details className={css.mobileFilters} data-files-mobile-filters="">
    <summary>{t('filters')}{(filters.workspace_id || filters.session_id || filters.kind) && <span>{t('filtered')}</span>}</summary>
    <div className={css.mobileFilterFields}>
      <Field><Label htmlFor={`${id}-workspace`}>{t('workspace')}</Label><Select id={`${id}-workspace`} value={filters.workspace_id} onValueChange={nextValue => change({ workspace_id: nextValue, session_id: '' })}><option value="">{t('allWorkspaces')}</option>{workspaces.map(item => <option value={item.id} key={item.id}>{item.name}</option>)}</Select></Field>
      <Field><Label htmlFor={`${id}-session`}>{t('session')}</Label><Select id={`${id}-session`} value={filters.session_id} onValueChange={nextValue => change({ session_id: nextValue })}><option value="">{t('allSessions')}</option>{sessions.map(item => <option value={item.id} key={item.id}>{item.name}</option>)}</Select></Field>
      <Field><Label htmlFor={`${id}-kind`}>{t('kind')}</Label><Select id={`${id}-kind`} value={filters.kind} onValueChange={nextValue => change({ kind: nextValue as FileFilters['kind'] })}>{kinds.map(item => <option value={item.id} key={item.id}>{item.name}</option>)}</Select></Field>
    </div>
  </details>
  return <nav className={css.filterNavigation} aria-label={t('filters')} data-files-navigation="">
    <section className={css.filterGroup} aria-label={t('kind')}><h2>{t('kind')}</h2>{kinds.map(item => <button key={item.id} type="button" className={css.filterLink} aria-pressed={filters.kind === item.id} data-file-kind={item.id || 'all'} onClick={() => change({ kind: item.id })}><item.icon /><span>{item.name}</span></button>)}</section>
    <section className={css.filterGroup} aria-label={t('workspace')}><h2>{t('workspace')}</h2>
      <button type="button" className={css.filterLink} aria-pressed={!filters.workspace_id} onClick={() => change({ workspace_id: '', session_id: '' })}><Folder /><span>{t('allWorkspaces')}</span></button>
      {workspaces.map(item => <button type="button" key={item.id} className={css.filterLink} title={item.name} aria-pressed={filters.workspace_id === item.id} data-file-workspace={item.id} onClick={() => change({ workspace_id: item.id, session_id: '' })}><Folder /><span>{item.name}</span></button>)}
    </section>
    <section className={css.filterGroup} aria-label={t('session')}><h2>{t('session')}</h2>
      <button type="button" className={css.filterLink} aria-pressed={!filters.session_id} onClick={() => change({ session_id: '' })}><MessageSquare /><span>{t('allSessions')}</span></button>
      {sessions.map(item => <button type="button" key={item.id} className={css.filterLink} title={item.name} aria-pressed={filters.session_id === item.id} data-file-session-filter={item.id} onClick={() => change({ session_id: item.id })}><MessageSquare /><span>{item.name}</span></button>)}
    </section>
  </nav>
}
