import * as React from 'react'
import { LoaderCircle } from 'lucide-react'
import { ApiError, api } from '@/api/client'
import type { ProjectRecord, Workspace } from '@/types'
import { useWorkbench } from '@/state/workbench'
import { useTranslate } from '@/i18n/provider'
import { Button } from '@/components/ui/button'
import { ChoiceSelect } from '@/components/ui/choice-select'
import { Field, Label } from '@/components/ui/field'
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from '@/components/ui/dialog'
import { ResourceSharingDialog } from './resource-sharing-dialog'

interface WorkspaceProject {
  project_id: string
  project_name: string
  execution_project_id: string
  revision: number
  inherit_project_sharing: boolean
  can_move: boolean
}

export function WorkspaceProjectDialog({ workspace, tenantId, onClose, onChanged }: {
  workspace: Workspace
  tenantId: string
  onClose(): void
  onChanged(): Promise<void>
}) {
  const t = useTranslate('workspace')
  const common = useTranslate('common')
  const { tenants, serverIdentity } = useWorkbench()
  const canShare = tenants?.find(tenant => tenant.tenant_id === tenantId)?.kind === 'team' && serverIdentity?.instance.mode === 'multi_user'
  const [snapshot, setSnapshot] = React.useState<WorkspaceProject | null>(null)
  const [projects, setProjects] = React.useState<ProjectRecord[]>([])
  const [target, setTarget] = React.useState('')
  const [inherit, setInherit] = React.useState(false)
  const [confirmed, setConfirmed] = React.useState(false)
  const [busy, setBusy] = React.useState(false)
  const [error, setError] = React.useState('')
  const [inspecting, setInspecting] = React.useState(false)
  const [revision, reload] = React.useReducer(value => value + 1, 0)
  const submitting = React.useRef(false)
  const path = `/workspaces/${encodeURIComponent(workspace.workspace_id)}/project`
  const headers = React.useMemo(() => ({ 'x-ternilo-tenant': tenantId }), [tenantId])

  React.useEffect(() => {
    const controller = new AbortController()
    setSnapshot(null); setTarget(''); setInherit(false); setConfirmed(false); setError('')
    void Promise.all([
      api.request<WorkspaceProject>(path, { headers, signal: controller.signal }),
      api.request<{ projects: ProjectRecord[] }>('/projects', { headers, signal: controller.signal }),
    ]).then(([current, choices]) => {
      if (!controller.signal.aborted) { setSnapshot(current); setProjects(choices.projects) }
    }).catch(cause => { if (!controller.signal.aborted) setError(cause instanceof Error ? cause.message : String(cause)) })
    return () => controller.abort()
  }, [path, headers, revision])

  const destination = projects.find(project => project.project_id === target)
  const choices = projects.filter(project => project.project_id !== snapshot?.project_id)
  const submit = async () => {
    if (submitting.current || !snapshot?.can_move || !destination || !confirmed) return
    submitting.current = true; setBusy(true); setError('')
    try {
      await api.request<WorkspaceProject>(path, { method: 'PUT', headers, body: {
        project_id: destination.project_id,
        expected_project_id: snapshot.project_id,
        expected_revision: snapshot.revision,
        inherit_project_sharing: inherit,
      } })
      await onChanged()
      onClose()
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause))
      if (cause instanceof ApiError && cause.status === 409) setConfirmed(false)
    } finally { submitting.current = false; setBusy(false) }
  }

  return <><Dialog open onOpenChange={open => { if (!open && !submitting.current) onClose() }}>
    <DialogContent className="flex max-h-[90dvh] max-w-lg flex-col overflow-hidden" onEscapeKeyDown={event => { if (busy) event.preventDefault() }}>
      <DialogHeader>
        <DialogTitle>{t('move.title')}</DialogTitle>
        <DialogDescription>{t('move.description', { name: workspace.title })}</DialogDescription>
      </DialogHeader>
      <div className="grid min-h-0 gap-4 overflow-y-auto py-1">
        <p className="text-sm text-muted-foreground">{t('move.execution')}</p>
        {!snapshot && !error && <p role="status" className="flex items-center gap-2 text-sm"><LoaderCircle className="size-4 animate-spin" />{t('move.loading')}</p>}
        {snapshot && <>
          <p className="text-sm">{t('move.current')}：{snapshot.project_name}</p>
          {!snapshot.can_move ? <p role="alert" className="text-sm text-destructive">{t('move.denied')}</p> : <>
            <Field><Label htmlFor="workspace-move-project">{t('move.target')}</Label>
              <ChoiceSelect id="workspace-move-project" value={target} disabled={busy} placeholder={t('move.choose')}
                options={choices.map(project => ({ value: project.project_id, label: project.name }))}
                onValueChange={value => { setTarget(value); setConfirmed(false); setInherit(false); setError('') }} />
            </Field>
            {!choices.length && <p className="text-sm text-muted-foreground">{t('move.noTargets')}</p>}
            <p className="text-sm text-muted-foreground">{t('move.sharing')}</p>
            {canShare && <>
              <label className="flex min-h-10 items-start gap-2 text-sm"><input type="checkbox" className="mt-1" checked={inherit} disabled={busy || !destination}
                onChange={event => { setInherit(event.target.checked); setConfirmed(false) }} />{t('move.inherit')}</label>
              <Button type="button" variant="outline" className="justify-self-start" disabled={busy || !destination} onClick={() => setInspecting(true)}>{t('sharing.viewProjectRules')}</Button>
            </>}
            {destination && <label className="flex min-h-10 items-start gap-2 text-sm"><input type="checkbox" className="mt-1" checked={confirmed} disabled={busy}
              onChange={event => setConfirmed(event.target.checked)} />{t('move.confirm', { name: destination.name })}</label>}
          </>}
        </>}
        {error && <p role="alert" className="text-sm text-destructive">{error}</p>}
        {error && <Button type="button" variant="outline" disabled={busy} onClick={reload}>{t('move.refresh')}</Button>}
      </div>
      <DialogFooter>
        <Button type="button" variant="outline" disabled={busy} onClick={onClose}>{common('cancel')}</Button>
        <Button type="button" disabled={busy || !snapshot?.can_move || !destination || !confirmed} onClick={() => void submit()}>
          {busy && <LoaderCircle className="size-4 animate-spin" />}{t('move.submit')}
        </Button>
      </DialogFooter>
    </DialogContent>
  </Dialog>
    {inspecting && destination && <ResourceSharingDialog target={{ kind: 'project', id: destination.project_id, title: destination.name, tenantId }} onClose={() => setInspecting(false)} onChanged={onChanged} />}
  </>
}
