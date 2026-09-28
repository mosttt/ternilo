import { describe, expect, it } from 'vitest'
import type { LocalSession, SessionSearchHit, Workspace } from '@/types'
import {
  deriveSessionSearchResults,
  MAX_SESSION_SEARCH_LENGTH,
  normalizeSessionSearchQuery,
} from './session-search'

const workspace = (id: string, title: string): Workspace => ({
  workspace_id: id,
  path: `/tmp/${id}`,
  title,
  created_at_ms: 1,
  updated_at_ms: 1,
})

const session = (id: string, workspaceId: string, title: string): LocalSession => ({
  identity: {
    tenant_id: 'local',
    user_id: 'local-user',
    agent_id: 'standard',
    session_id: id,
  },
  workspace_id: workspaceId,
  workspace_path: `/tmp/${workspaceId}`,
  parent_session_id: null,
  title,
  archived_at_ms: null,
  blank: false,
  permissions: 'workspace_write',
  model: { provider: 'profile_default' },
  agent_preset: 'standard',
  preset_plugins: [],
  profile_plugins: [],
  mode: 'execute',
  created_at_ms: 1,
  updated_at_ms: 1,
})

const hit = (sessionId: string, workspaceId: string, excerpt: string): SessionSearchHit => ({
  session_id: sessionId,
  workspace_id: workspaceId,
  title: excerpt,
  updated_at_ms: 1,
  event_seq: 3,
  occurred_at_ms: 1,
  run_id: 'run',
  category: 'assistant',
  excerpt,
})

describe('session search projection', () => {
  const sessions = [
    session('one', 'alpha', '发布检查'),
    session('two', 'beta', '文档整理'),
    session('orphan', 'removed', '遗留会话'),
  ]
  const workspaces = [workspace('alpha', '桌面应用'), workspace('beta', '云端平台')]

  it('merges indexed content hits with local session and workspace title matches', () => {
    expect(deriveSessionSearchResults(sessions, workspaces, [hit('orphan', 'removed', '云端命中')], '云端').items.map(item => item.identity.session_id))
      .toEqual(['two', 'orphan'])
    expect(deriveSessionSearchResults(sessions, workspaces, [], '发布').items.map(item => item.identity.session_id))
      .toEqual(['one'])
  })

  it('keeps local matches first, then backend rank, and deduplicates repeated event hits', () => {
    const results = deriveSessionSearchResults(sessions, workspaces, [
      hit('two', 'beta', '第一命中'), hit('one', 'alpha', '第二命中'), hit('two', 'beta', '重复命中'),
    ], '会话').items
    expect(results.map(item => item.identity.session_id)).toEqual(['orphan', 'two', 'one'])
  })

  it('does not invent a removed workspace title for an ungrouped session', () => {
    expect(deriveSessionSearchResults(sessions, workspaces, [], 'removed').items).toEqual([])
  })

  it('bounds merged results and carries an explicit refine-query hint', () => {
    const many = Array.from({ length: 4 }, (_, index) => session(`s${index}`, 'alpha', `会话 ${index}`))
    const result = deriveSessionSearchResults(many, workspaces, [], '会话', 2)
    expect(result.items).toHaveLength(2)
    expect(result.hasMore).toBe(true)
  })

  it('normalizes and bounds the Host query to 500 code units', () => {
    expect(normalizeSessionSearchQuery('  云端  ')).toBe('云端')
    expect(normalizeSessionSearchQuery('x'.repeat(MAX_SESSION_SEARCH_LENGTH + 20))).toHaveLength(MAX_SESSION_SEARCH_LENGTH)
  })
})
