import * as React from 'react'
import { api } from '@/api/client'
import { useTranslate } from '@/i18n/provider'
import { useWorkbench } from '@/state/workbench'
import type { ExecutionTarget, ProjectRecord, Workspace } from '@/types'

export const NEW_PROJECT = '__new_project__'

function folderName(path: string) {
  return path.replace(/[\\/]+$/, '').split(/[\\/]/).at(-1) || path
}

export function usePlatformWorkspaceSetup({ open, onOpenChange, createSessionAfter }: {
  open: boolean
  onOpenChange(open: boolean): void
  createSessionAfter: boolean
}) {
  const { createWorkspace, createSession, notify, currentWorkspace, currentTenantRole,
    currentTenantId, tenants, serverIdentity, snapshot } = useWorkbench()
  const t = useTranslate('workspace')
  const managedExecution = serverIdentity?.instance.managed_execution_enabled === true
  const canCreateProject = currentTenantRole === 'owner' || currentTenantRole === 'admin'
  const [projects, setProjects] = React.useState<ProjectRecord[]>([])
  const [executors, setExecutors] = React.useState<ExecutionTarget[]>([])
  const [placement, setPlacement] = React.useState<'cloud' | 'local_node'>('local_node')
  const [projectId, setProjectId] = React.useState('')
  const [executorId, setExecutorId] = React.useState('')
  const [name, setName] = React.useState('')
  const [projectName, setProjectName] = React.useState('')
  const [path, setPath] = React.useState('')
  const [browserOpen, setBrowserOpen] = React.useState(false)
  const [loading, setLoading] = React.useState(false)
  const [saving, setSaving] = React.useState(false)
  const [error, setError] = React.useState('')
  const requestId = React.useRef(0)
  const submitting = React.useRef(false)
  const initialized = React.useRef(false)
  const createdWorkspace = React.useRef<Workspace | null>(null)
  const initialWorkspace = React.useRef(currentWorkspace)
  const suggestedName = React.useRef('')
  const selectedExecutor = executors.find(executor => executor.executor_id === executorId)
  const projectRestricted = placement === 'local_node' && Boolean(selectedExecutor?.project_id && selectedExecutor.project_id !== projectId)
  const boundProjectName = projects.find(project => project.project_id === selectedExecutor?.project_id)?.name ?? selectedExecutor?.project_id
  const projectReady = projectId === NEW_PROJECT ? canCreateProject && Boolean(projectName.trim())
    : projects.some(project => project.project_id === projectId)
  const canSubmit = !loading && !saving && !projectRestricted && projectReady && Boolean(name.trim())
    && (placement === 'cloud' ? managedExecution : Boolean(selectedExecutor?.connected && path))

  const load = React.useCallback(async (initial = false) => {
    const sequence = ++requestId.current
    setLoading(true)
    setError('')
    try {
      const [projectResponse, executorResponse] = await Promise.all([
        api.request<{ projects: ProjectRecord[] }>('/projects'),
        api.request<{ executors: ExecutionTarget[] }>('/execution-targets'),
      ])
      if (sequence !== requestId.current) return
      setProjects(projectResponse.projects)
      setExecutors(executorResponse.executors)
      if (initial || !initialized.current) {
        initialized.current = true
        const current = initialWorkspace.current
        const target = executorResponse.executors.find(item => item.executor_id === current?.node_id)
          ?? executorResponse.executors.find(item => item.connected) ?? executorResponse.executors[0]
        const nextPlacement = managedExecution && (current?.placement === 'cloud' || !target) ? 'cloud' : 'local_node'
        const permitted = nextPlacement === 'local_node' && target?.project_id
          ? projectResponse.projects.filter(item => item.project_id === target.project_id) : projectResponse.projects
        setPlacement(nextPlacement)
        setExecutorId(target?.executor_id ?? '')
        setProjectId(permitted.find(item => item.project_id === current?.project_id)?.project_id
          ?? permitted[0]?.project_id ?? (canCreateProject && !target?.project_id ? NEW_PROJECT : ''))
      }
    } catch (cause) {
      if (sequence === requestId.current) setError(cause instanceof Error ? cause.message : String(cause))
    } finally {
      if (sequence === requestId.current) setLoading(false)
    }
  }, [canCreateProject, managedExecution])

  React.useEffect(() => { initialWorkspace.current = currentWorkspace }, [currentWorkspace])
  React.useEffect(() => {
    if (!open) return
    initialized.current = false
    setPath('')
    setName('')
    suggestedName.current = ''
    setProjectName('')
    setBrowserOpen(false)
    createdWorkspace.current = null
    void load(true)
    return () => { requestId.current += 1 }
  }, [open, load])

  const changeExecutor = (id: string) => {
    const target = executors.find(item => item.executor_id === id)
    setExecutorId(id)
    setPath('')
    createdWorkspace.current = null
    if (target?.project_id) setProjectId(target.project_id)
  }
  const changePlacement = (value: 'cloud' | 'local_node') => {
    setPlacement(value)
    createdWorkspace.current = null
    if (value === 'local_node' && selectedExecutor?.project_id) setProjectId(selectedExecutor.project_id)
  }
  const choosePath = async (value: string) => {
    const folder = folderName(value)
    const names = new Set(snapshot.workspaces.filter(workspace => workspace.project_id === projectId
      && workspace.access?.is_owner !== false && workspace.node_id !== executorId).map(workspace => workspace.title))
    let suggested = folder
    if (names.has(suggested)) {
      const base = `${folder} (${selectedExecutor?.name ?? t('picker.executor.unavailable')})`
      suggested = base
      for (let number = 2; names.has(suggested); number++) suggested = `${base} (${number})`
    }
    const previous = suggestedName.current
    setName(current => !current || current === previous ? suggested : current)
    suggestedName.current = suggested
    setProjectName(current => !current || current === folderName(path) ? folder : current)
    setPath(value)
    createdWorkspace.current = null
  }

  const ensureProject = async () => {
    if (projectId !== NEW_PROJECT) return projectId
    const response = await api.request<{ project: ProjectRecord }>('/projects', {
      method: 'POST', body: { name: projectName.trim() },
    })
    setProjects(current => [...current, response.project])
    setProjectId(response.project.project_id)
    return response.project.project_id
  }
  const submit = async (projectOnly = false) => {
    if (submitting.current || (projectOnly ? !canCreateProject || !projectName.trim() : !canSubmit)) return
    submitting.current = true
    setSaving(true)
    setError('')
    try {
      const id = await ensureProject()
      if (projectOnly) {
        notify(t('picker.project.created'))
        return
      }
      const workspace = createdWorkspace.current ?? await createWorkspace({
        project_id: id, name: name.trim(), placement,
        ...(placement === 'local_node' ? { executor_id: executorId, path } : {}),
      })
      createdWorkspace.current = workspace
      if (createSessionAfter) await createSession(workspace.workspace_id)
      onOpenChange(false)
      notify(t(placement === 'cloud' ? 'picker.cloud.created' : 'picker.local.connected', { name: workspace.title }))
    } catch (cause) {
      setError(cause instanceof Error && cause.message === 'workspace name already exists in this project'
        ? t('picker.workspace.nameConflict') : cause instanceof Error ? cause.message : String(cause))
    } finally {
      submitting.current = false
      setSaving(false)
    }
  }

  return { projects, executors, placement, projectId, executorId, name, projectName,
    path, browserOpen, loading, saving, error, managedExecution, canCreateProject, projectRestricted, boundProjectName,
    selectedExecutor, canSubmit, workspaceCreated: createdWorkspace.current !== null,
    scopeName: tenants.find(tenant => tenant.tenant_id === currentTenantId)?.display_name,
    setProjectId, setName, setProjectName, setBrowserOpen, changeExecutor, changePlacement, choosePath, load, submit }
}
