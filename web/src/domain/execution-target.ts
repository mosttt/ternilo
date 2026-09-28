export interface ExecutionTarget {
  tenantId?: string | null
  executorId?: string | null
  sessionId?: string | null
  workspaceId?: string | null
  placement?: 'local_node' | 'cloud'
}

export function executionTargetKey(target: ExecutionTarget = {}) {
  const scope = target.executorId ? `computer:${target.executorId}` : target.sessionId ? `session:${target.sessionId}` : target.workspaceId ? `workspace:${target.workspaceId}` : 'cloud'
  return target.tenantId ? `space:${target.tenantId}:${scope}` : scope
}

export function executionTargetHeaders(target: ExecutionTarget = {}) {
  return target.tenantId ? { 'x-ternilo-tenant': target.tenantId } : undefined
}

export function executionTargetPath(
  path: string,
  target: ExecutionTarget = {},
  parameters?: Record<string, string>,
) {
  const query = new URLSearchParams(parameters)
  if (target.executorId) query.set('executor_id', target.executorId)
  else if (target.sessionId) query.set('session_id', target.sessionId)
  else if (target.workspaceId) query.set('workspace_id', target.workspaceId)
  const suffix = query.toString()
  if (!suffix) return path
  return `${path}${path.includes('?') ? '&' : '?'}${suffix}`
}
