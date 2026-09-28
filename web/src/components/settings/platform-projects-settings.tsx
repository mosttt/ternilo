import * as React from 'react'
import { Folder, LoaderCircle, Pencil, Plus, RefreshCw, Trash2 } from 'lucide-react'
import { ApiError, api } from '@/api/client'
import { navigate } from '@/app/navigation'
import { Button } from '@/components/ui/button'
import { Field, Input, Label } from '@/components/ui/field'
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from '@/components/ui/dialog'
import { useTranslate } from '@/i18n/provider'
import { useWorkbench } from '@/state/workbench'
import type { ProjectRecord } from '@/types'

export function PlatformProjectsSettings({ tenantId }: { tenantId: string }) {
  const translate = useTranslate('workspace')
  const common = useTranslate('common')
  const { snapshot, selectWorkspace, notify } = useWorkbench()
  const [projects, setProjects] = React.useState<ProjectRecord[]>([])
  const [name, setName] = React.useState('')
  const [loading, setLoading] = React.useState(true)
  const [saving, setSaving] = React.useState(false)
  const [error, setError] = React.useState('')
  const [renameTarget, setRenameTarget] = React.useState<ProjectRecord | null>(null)
  const [renameName, setRenameName] = React.useState('')
  const [deleteTarget, setDeleteTarget] = React.useState<ProjectRecord | null>(null)
  const [mutationError, setMutationError] = React.useState('')
  const submitting = React.useRef(false)
  const [revision, refresh] = React.useReducer(value => value + 1, 0)
  React.useEffect(() => {
    const controller = new AbortController()
    setLoading(true); setError('')
    void api.request<{ projects: ProjectRecord[] }>('/projects', { headers: { 'x-ternilo-tenant': tenantId }, signal: controller.signal })
      .then(result => { if (!controller.signal.aborted) setProjects(result.projects) })
      .catch(cause => { if (!controller.signal.aborted) setError(String(cause)) })
      .finally(() => { if (!controller.signal.aborted) setLoading(false) })
    return () => controller.abort()
  }, [tenantId, revision])
  const errorMessage = (cause: unknown) => {
    if (cause instanceof ApiError) {
      if (cause.status === 403) return translate('project.error.permission')
      if (cause.message === 'personal default project cannot be deleted') return translate('project.error.default')
      if (cause.message === 'the last project in a space cannot be deleted') return translate('project.error.last')
      if (cause.message === 'project has associated resources and cannot be deleted') return translate('project.error.resources')
      if (cause.message === 'project does not exist') return translate('project.error.missing')
      if (cause.message.startsWith('project name must contain')) return translate('project.error.name')
    }
    return cause instanceof Error ? cause.message : String(cause)
  }
  const create = async () => {
    if (submitting.current || !name.trim()) return
    submitting.current = true
    setSaving(true); setError('')
    try {
      await api.request('/projects', { method: 'POST', headers: { 'x-ternilo-tenant': tenantId }, body: { name: name.trim() } })
      setName(''); refresh(); notify(translate('picker.project.created'))
    } catch (cause) { setError(errorMessage(cause)) }
    finally { submitting.current = false; setSaving(false) }
  }
  const rename = async () => {
    if (submitting.current || !renameTarget || !renameName.trim()) return
    submitting.current = true
    setSaving(true); setMutationError('')
    try {
      const result = await api.request<{ project: ProjectRecord }>(`/projects/${encodeURIComponent(renameTarget.project_id)}`, {
        method: 'PATCH', headers: { 'x-ternilo-tenant': tenantId }, body: { name: renameName.trim() },
      })
      setProjects(current => current.map(project => project.project_id === result.project.project_id ? result.project : project))
      setRenameTarget(null); notify(translate('project.renamed'))
    } catch (cause) { setMutationError(errorMessage(cause)) }
    finally { submitting.current = false; setSaving(false) }
  }
  const remove = async () => {
    if (submitting.current || !deleteTarget) return
    submitting.current = true
    setSaving(true); setMutationError('')
    try {
      await api.request(`/projects/${encodeURIComponent(deleteTarget.project_id)}`, {
        method: 'DELETE', headers: { 'x-ternilo-tenant': tenantId },
      })
      setProjects(current => current.filter(project => project.project_id !== deleteTarget.project_id))
      setDeleteTarget(null); notify(translate('project.deleted'))
    } catch (cause) { setMutationError(errorMessage(cause)) }
    finally { submitting.current = false; setSaving(false) }
  }
  return <section className="grid gap-5" data-project-management="">
    <header className="flex flex-wrap items-start justify-between gap-4"><div className="min-w-0 flex-1"><h2 className="font-semibold">{translate('picker.project.manage')}</h2><p className="mt-2 text-sm leading-relaxed text-muted-foreground">{translate('picker.project.description')}</p></div><Button variant="outline" disabled={loading || saving} onClick={refresh}><RefreshCw />{translate('picker.refresh')}</Button></header>
    <form className="flex flex-wrap items-end gap-3" onSubmit={event => { event.preventDefault(); void create() }}><Field className="min-w-0 flex-1"><Label htmlFor="project-create-name">{translate('picker.project.placeholder')}</Label><Input id="project-create-name" value={name} maxLength={256} onChange={event => setName(event.target.value)} disabled={saving || loading} /></Field><Button disabled={saving || loading || !name.trim()}><Plus />{translate('picker.project.new')}</Button></form>
    {error && <p role="alert" className="text-sm text-destructive">{error}</p>}
    {loading ? <p role="status" className="text-sm text-muted-foreground">{translate('picker.loading')}</p> : projects.map(project => {
      const workspaces = snapshot.workspaces.filter(workspace => workspace.project_id === project.project_id)
      return <article key={project.project_id} className="min-w-0 rounded-xl border bg-card p-4" data-project-id={project.project_id}>
        <div className="flex flex-wrap items-start justify-between gap-3">
          <h3 className="flex min-w-0 items-start gap-2 font-medium"><Folder className="mt-1 size-4 shrink-0" /><span className="min-w-0 break-words [overflow-wrap:anywhere]">{project.name}</span></h3>
          <div className="flex flex-wrap gap-2">
            <Button type="button" variant="outline" size="sm" disabled={saving} onClick={() => { setMutationError(''); setRenameName(project.name); setRenameTarget(project) }}><Pencil />{translate('project.rename')}</Button>
            <Button type="button" variant="outline" size="sm" className="text-destructive hover:text-destructive" disabled={saving} onClick={() => { setMutationError(''); setDeleteTarget(project) }}><Trash2 />{translate('project.delete')}</Button>
          </div>
        </div>
        <p className="mt-2 text-xs text-muted-foreground">{translate('project.workspaces', { count: workspaces.length })}</p>
        <div className="mt-3 grid gap-1">{workspaces.map(workspace => <button key={workspace.workspace_id} type="button" className="rounded-lg px-3 py-2 text-left text-sm hover:bg-accent focus-visible:outline-2 focus-visible:outline-ring" onClick={() => { selectWorkspace(workspace.workspace_id); navigate('/') }}><span>{workspace.title}</span>{workspace.node_id && <span className="ml-2 text-xs text-muted-foreground">{workspace.node_id}</span>}</button>)}</div>
      </article>
    })}
    <Dialog open={Boolean(renameTarget)} onOpenChange={open => { if (!open && !submitting.current) setRenameTarget(null) }}>
      <DialogContent className="max-w-md">
        <DialogHeader><DialogTitle>{translate('project.rename')}</DialogTitle><DialogDescription>{translate('project.renameDescription')}</DialogDescription></DialogHeader>
        <form className="grid gap-4" onSubmit={event => { event.preventDefault(); void rename() }}>
          <Field><Label htmlFor="project-rename-name">{translate('picker.project.placeholder')}</Label><Input id="project-rename-name" value={renameName} maxLength={256} disabled={saving} autoFocus onFocus={event => event.currentTarget.select()} onChange={event => setRenameName(event.target.value)} onKeyDown={event => { if (event.key === 'Enter' && event.nativeEvent.isComposing) event.preventDefault() }} /></Field>
          {mutationError && <p role="alert" className="text-sm text-destructive">{mutationError}</p>}
          <DialogFooter><Button type="button" variant="outline" disabled={saving} onClick={() => setRenameTarget(null)}>{common('cancel')}</Button><Button type="submit" disabled={saving || !renameName.trim() || renameName.trim() === renameTarget?.name}>{saving ? common('saving') : common('save')}</Button></DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
    <Dialog open={Boolean(deleteTarget)} onOpenChange={open => { if (!open && !submitting.current) setDeleteTarget(null) }}>
      <DialogContent className="max-w-md [overflow-wrap:anywhere]" data-settings-dialog="">
        <DialogHeader><DialogTitle>{translate('project.deleteTitle')}</DialogTitle><DialogDescription>{translate('project.deleteDescription', { name: deleteTarget?.name ?? '' })}</DialogDescription></DialogHeader>
        {mutationError && <p role="alert" className="text-sm text-destructive">{mutationError}</p>}
        <DialogFooter>
          <Button type="button" variant="outline" disabled={saving} onClick={() => setDeleteTarget(null)}>{common('cancel')}</Button>
          <Button type="button" variant="outline" className="text-destructive hover:text-destructive" disabled={saving} onClick={() => void remove()}>
            {saving && <LoaderCircle className="animate-spin" />}{translate(saving ? 'project.deleting' : 'project.delete')}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  </section>
}
