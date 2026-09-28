import { describe, expect, it } from 'vitest'
import type { Translate } from '@/i18n/runtime'
import type { LocalSession, Workspace } from '@/types'
import { workspaceDisplayPath } from './workspace-display'

const t = ((key: string, params?: Record<string, unknown>) => `${key}:${params?.name ?? ''}`) as Translate<'workspace'>

describe('workspace display path', () => {
  it('keeps local filesystem paths and localizes Control placement labels', () => {
    const base: Workspace = { workspace_id: 'w', path: '/home/user/code', title: 'Code', created_at_ms: 1, updated_at_ms: 1 }
    expect(workspaceDisplayPath(base, null, t)).toBe('/home/user/code')
    expect(workspaceDisplayPath({ ...base, placement: 'cloud' }, null, t)).toBe('displayPath.cloud:Code')
    expect(workspaceDisplayPath({ ...base, placement: 'local_node' }, null, t)).toBe('displayPath.computer:Code')
    expect(workspaceDisplayPath({ ...base, placement: 'local_node', node_id: 'a' }, null, t)).toBe('displayPath.computer:Code')
  })

  it('does not render a backend-language snapshot after a Control Workspace is removed', () => {
    const session = { placement: 'cloud', workspace_path: '云端 / 已移除 Workspace' } as LocalSession
    expect(workspaceDisplayPath(null, session, t)).toBe('displayPath.removedCloud:')
    expect(workspaceDisplayPath(null, { ...session, placement: 'local_node' }, t)).toBe('displayPath.removedComputer:')
  })

  it('does not call a hidden parent workspace removed for a shared session', () => {
    const session: LocalSession = {
      identity: { tenant_id: 'tenant', user_id: 'user', agent_id: 'agent', session_id: 'session' },
      workspace_id: 'workspace', title: 'Session', placement: 'cloud', workspace_path: '云端 / 未分组',
      permissions: 'read_only', model: { provider: 'profile_default' }, agent_preset: 'standard',
      preset_plugins: [], profile_plugins: [], mode: 'execute', created_at_ms: 1, updated_at_ms: 1,
      access: { owner_user_id: 'owner', is_owner: false, sources: [], role_limited: false, permissions: { view: true, submit: false, stop: false, configure: false } },
    }
    expect(workspaceDisplayPath(null, session, t)).toBe('displayPath.cloud:group.ungrouped:')
    expect(workspaceDisplayPath(null, { ...session, placement: 'local_node' }, t)).toBe('displayPath.computer:group.ungrouped:')
  })
})
