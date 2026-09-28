import type { Translate } from '@/i18n/runtime'
import type { LocalSession, Workspace } from '@/types'

export function workspaceDisplayPath(
  workspace: Workspace | null | undefined,
  session: LocalSession | null | undefined,
  t: Translate<'workspace'>,
) {
  if (workspace?.placement === 'cloud') return t('displayPath.cloud', { name: workspace.title })
  if (workspace?.placement === 'local_node') return t('displayPath.computer', { name: workspace.title })
  if (workspace) return workspace.path
  if (session?.access && !session.access.is_owner) {
    if (session.placement === 'cloud') return t('displayPath.cloud', { name: t('group.ungrouped') })
    if (session.placement === 'local_node') return t('displayPath.computer', { name: t('group.ungrouped') })
  }
  if (session?.placement === 'cloud') return t('displayPath.removedCloud')
  if (session?.placement === 'local_node') return t('displayPath.removedComputer')
  return session?.workspace_path ?? ''
}
