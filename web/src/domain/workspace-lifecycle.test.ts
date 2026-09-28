import { describe, expect, it } from 'vitest'
import type { LocalSession, Workspace } from '@/types'
import {
  abbreviateHomePath, ungroupedSessions, workspaceRenameIssue,
} from './workspace-lifecycle'

const workspace = (id: string, title = id): Workspace => ({
  workspace_id: id,
  path: `/home/user/${id}`,
  title,
  created_at_ms: 1,
  updated_at_ms: 1,
})

const session = (id: string, workspaceId: string): LocalSession => ({
  identity: { tenant_id: 'local', user_id: 'user', agent_id: 'standard', session_id: id },
  workspace_id: workspaceId,
  workspace_path: `/home/user/${workspaceId}`,
  title: id,
  permissions: 'workspace_write',
  model: { provider: 'profile_default' },
  agent_preset: 'standard',
  preset_plugins: [],
  profile_plugins: [],
  mode: 'execute',
  created_at_ms: 1,
  updated_at_ms: 1,
})

describe('workspace lifecycle view contract', () => {
  it('classifies sessions whose registration was removed as ungrouped', () => {
    expect(ungroupedSessions([workspace('registered')], [
      session('inside', 'registered'),
      session('retained', 'removed'),
    ]).map(item => item.identity.session_id)).toEqual(['retained'])
  })

  it('validates the controlled rename before issuing a request', () => {
    const target = workspace('one', 'Alpha')
    const all = [target, workspace('two', 'Beta')]
    expect(workspaceRenameIssue(' ', target, all)).toBe('blank')
    expect(workspaceRenameIssue(' Alpha ', target, all)).toBe('unchanged')
    expect(workspaceRenameIssue('Beta', target, all)).toBe('duplicate')
    expect(workspaceRenameIssue('Gamma', target, all)).toBeNull()
  })

  it('abbreviates only paths rooted at the exact host home', () => {
    expect(abbreviateHomePath('/home/user', '/home/user/')).toBe('~')
    expect(abbreviateHomePath('/home/user/project', '/home/user')).toBe('~/project')
    expect(abbreviateHomePath('/home/username/project', '/home/user')).toBe('/home/username/project')
    expect(abbreviateHomePath('C:\\Users\\Ada\\code', 'C:\\Users\\Ada')).toBe('~\\code')
  })
})
