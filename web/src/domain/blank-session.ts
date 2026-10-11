import type { LocalSession, PermissionPreset } from '@/types'
import { ownsResource } from './resource-access'

export function reusableBlankSession(session: LocalSession, workspaceId: string, preset: string, permissions: PermissionPreset, local: boolean) {
  return session.workspace_id === workspaceId && session.blank === true && session.archived_at_ms == null
    && !session.parent_session_id && !session.subagent && session.title === 'New session'
    && session.agent_preset === preset && session.permissions === permissions && session.mode === 'execute'
    && (session.placement === 'cloud'
      ? session.created_at_ms === session.updated_at_ms
      : session.profile_plugins.length === 0)
    && ownsResource(session.access, local)
}
