import * as React from 'react'
import { api } from '@/api/client'
import { useWorkbench } from '@/state/workbench'
import { useTranslate } from '@/i18n/provider'
import { workspaceDisplayPath } from '@/domain/workspace-display'
import type { ProjectRecord } from '@/types'
import { workspacePlacementLabel } from './workspace-location'

export const WorkspaceDisplayContext = React.createContext<string | null>(null)

export function useWorkspaceDisplay() {
  const { currentWorkspace: workspace, currentSession: session, platform, accountScope, currentTenantId, currentTenantRole } = useWorkbench()
  const t = useTranslate('workspace')
  const key = JSON.stringify([accountScope, currentTenantId, workspace?.workspace_id])
  const remote = platform && workspace?.placement === 'local_node'
  const readable = remote && workspace.access?.is_owner !== false && workspace.status !== 'offline' && workspace.status !== 'error'
  const [resolved, setResolved] = React.useState<{ key: string; path: string | null; status: string } | null>(null)
  const [project, setProject] = React.useState<{ key: string; name: string } | null>(null)
  React.useEffect(() => {
    if (!readable || !workspace) return
    const controller = new AbortController()
    void api.request<{ status: string; path: string | null }>(`/workspaces/${encodeURIComponent(workspace.workspace_id)}/location`, {
      signal: controller.signal, cache: 'no-store', ...(currentTenantId ? { headers: { 'x-ternilo-tenant': currentTenantId } } : {}),
    }).then(value => { if (!controller.signal.aborted) setResolved({ key, path: value.path, status: value.status }) })
      .catch(() => { if (!controller.signal.aborted) setResolved({ key, path: null, status: 'unavailable' }) })
    return () => controller.abort()
  }, [readable, key, workspace?.workspace_id, currentTenantId])
  React.useEffect(() => {
    if (!platform || !workspace?.project_id || (currentTenantRole !== 'owner' && currentTenantRole !== 'admin')) return
    const controller = new AbortController()
    void api.request<{ projects: ProjectRecord[] }>('/projects', { signal: controller.signal, ...(currentTenantId ? { headers: { 'x-ternilo-tenant': currentTenantId } } : {}) })
      .then(value => { if (!controller.signal.aborted) setProject({ key, name: value.projects.find(item => item.project_id === workspace.project_id)?.name ?? '' }) })
      .catch(() => { if (!controller.signal.aborted) setProject(null) })
    return () => controller.abort()
  }, [platform, key, workspace?.project_id, currentTenantId, currentTenantRole])
  const location = readable && resolved?.key === key ? resolved : null
  const path = !platform ? workspace?.path ?? session?.workspace_path ?? '' : location?.status === 'available' ? location.path ?? '' : ''
  const identity = workspace ? [workspacePlacementLabel(workspace, t), workspace.title].join(' · ') : workspaceDisplayPath(workspace, session, t)
  const hint = remote ? workspace.access?.is_owner === false ? t('location.ownerOnly') : !readable || location?.status === 'offline' ? t('location.offline') : !location ? t('location.loading') : t('location.unavailable') : platform ? t('location.cloud') : ''
  return {
    label: identity,
    path,
    hint,
    project: project?.key === key ? project.name : '',
    replacement: platform && session?.placement === 'local_node' ? path || t('location.currentWorkspace', { name: workspace?.title || t('group.ungrouped') }) : null,
  }
}
