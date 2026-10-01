import type { LocalSession, ResourceAccess, ResourcePermissions, UserQuestion, Workspace } from '@/types'

export function resourcePermissions(access: ResourceAccess | undefined, legacyCanOperate: boolean): ResourcePermissions {
  return access?.permissions ?? { view: true, submit: legacyCanOperate, stop: legacyCanOperate, configure: legacyCanOperate }
}

export function ownsResource(access: ResourceAccess | undefined, legacyCanOperate: boolean): boolean {
  return access ? access.is_owner && access.permissions.configure : legacyCanOperate
}

export function ownsExecutionConfiguration(workspace: Workspace | null | undefined, session: LocalSession | null | undefined, legacyCanOperate: boolean): boolean {
  const access = workspace?.placement === 'local_node' ? workspace.access : session?.access ?? workspace?.access
  return access ? access.is_execution_owner && access.permissions.configure : legacyCanOperate
}

// The server rechecks the persisted question; this controls its browser actions.
export function canAnswerQuestion(question: UserQuestion, permissions: ResourcePermissions, isOwner: boolean): boolean {
  if (!permissions.view) return false
  switch (question.tool_approval?.tool_name) {
    case 'extension_set_enabled':
    case 'extension_revoke': return isOwner && permissions.configure
    case 'extension_set_mounted':
    case 'exit_plan_mode': return permissions.configure
    case 'interrupt_agent':
    case 'job_kill':
    case 'terminal_close':
    case 'schedule_delete': return permissions.stop
  }
  return question.presentation?.kind === 'plan_review' ? permissions.configure : permissions.submit
}
