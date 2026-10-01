import { describe, expect, it } from 'vitest'
import type { LocalSession, Workspace } from '@/types'
import {
  canonicalSessionTree, defaultSidebarView, expandActiveAccount, foldSessionWindow,
  moveItem, moveItemByStep, orderIds, readSidebarView,
  groupWorkspacesByComputer, replaceOrderedSubset,
} from './sidebar-view'

const items = [
  { id: 'one', updated: 3 },
  { id: 'two', updated: 7 },
  { id: 'three', updated: 5 },
]

describe('sidebar view', () => {
  it('groups by computer by default while keeping explicit grouping and collapse preferences', () => {
    expect(readSidebarView(null).groupBy).toBe('computer')
    expect(readSidebarView('{}').groupBy).toBe('computer')
    expect(readSidebarView(JSON.stringify({ groupBy: 'computer', collapsedComputers: ['scope:node-a', 7] })))
      .toMatchObject({ groupBy: 'computer', collapsedComputers: ['scope:node-a'] })
    expect(readSidebarView(JSON.stringify({ groupBy: 'workspace' })).groupBy).toBe('workspace')
  })

  it('keeps same-named workspaces on different computers separate, including offline computers', () => {
    const workspace = (id: string, node: string, status: 'online' | 'offline'): Workspace => ({
      workspace_id: id, node_id: node, status, placement: 'local_node', path: '', title: 'Pictures', created_at_ms: 1, updated_at_ms: 1,
    })
    const groups = groupWorkspacesByComputer([workspace('a-1','a','offline'),workspace('b-1','b','online'),workspace('a-2','a','offline')])
    expect(groups.map(group => [group.nodeId, group.workspaces.map(item => item.workspace_id)])).toEqual([
      ['a',['a-1','a-2']],['b',['b-1']],
    ])
    expect(replaceOrderedSubset(['a-1','b-1','a-2'],['a-2','a-1'])).toEqual(['a-2','b-1','a-1'])
  })
  it('keeps grouping and ordering as independent persisted choices', () => {
    const view = readSidebarView(JSON.stringify({
      groupBy: 'flat', orderBy: 'manual', workspaceOrder: ['device-only'],
      sessionOrderByAccount: { workspace: ['device-only'] }, collapsedSessions: ['parent'],
    }))
    expect(view).toMatchObject({
      groupBy: 'flat', orderBy: 'manual', collapsedSessions: ['parent'],
    })
    expect(view).not.toHaveProperty('workspaceOrder')
    expect(view).not.toHaveProperty('sessionOrderByAccount')
    expect(readSidebarView('{broken')).toEqual(defaultSidebarView)
  })

  it('nests only Sessions with explicit canonical Subagent metadata', () => {
    const session = (id: string, parent?: string, subagent = false): LocalSession => ({
      identity: { tenant_id: 'tenant', user_id: 'user', agent_id: 'agent', session_id: id },
      workspace_id: 'workspace', workspace_path: '/workspace', title: id,
      permissions: 'workspace_write', model: { provider: 'profile_default' },
      agent_preset: 'standard', preset_plugins: [], profile_plugins: [], mode: 'execute',
      parent_session_id: parent ?? null, created_at_ms: 1, updated_at_ms: 1,
      ...(subagent ? { subagent: { subagent_id: `agent-${id}`, provider: 'in-process', transcript_kind: 'conversation' as const } } : {}),
    })
    const tree = canonicalSessionTree([
      session('root'), session('child', 'root', true), session('grandchild', 'child', true),
      session('ordinary-fork', 'root'), session('orphan-child', 'missing', true),
    ])

    expect(tree.map(node => node.session.identity.session_id)).toEqual(['root', 'ordinary-fork', 'orphan-child'])
    expect(tree[0]?.children.map(node => node.session.identity.session_id)).toEqual(['child'])
    expect(tree[0]?.children[0]?.children.map(node => node.session.identity.session_id)).toEqual(['grandchild'])
  })

  it('sorts by activity or reconciles a durable manual order', () => {
    expect(orderIds(items, item => item.id, item => item.updated, [], 'updated')).toEqual(['two', 'three', 'one'])
    expect(orderIds(items, item => item.id, item => item.updated, ['one', 'two'], 'manual')).toEqual(['three', 'one', 'two'])
  })

  it('moves a dragged row before or after its target', () => {
    expect(moveItem(['one', 'two', 'three'], 'one', 'three', false)).toEqual(['two', 'one', 'three'])
    expect(moveItem(['one', 'two', 'three'], 'one', 'three', true)).toEqual(['two', 'three', 'one'])
  })

  it('moves one accessible menu step without wrapping at either edge', () => {
    const order = ['one', 'two', 'three']
    expect(moveItemByStep(order, 'two', -1)).toEqual(['two', 'one', 'three'])
    expect(moveItemByStep(order, 'two', 1)).toEqual(['one', 'three', 'two'])
    expect(moveItemByStep(order, 'one', -1)).toBe(order)
    expect(moveItemByStep(order, 'three', 1)).toBe(order)
  })

  it('does not charge provisional blank sessions against the five established rows', () => {
    const sessions = [
      { id: 'blank', blank: true },
      ...Array.from({ length: 7 }, (_, index) => ({ id: `saved-${index + 1}`, blank: false })),
    ]
    expect(foldSessionWindow(sessions, session => session.blank).map(session => session.id)).toEqual([
      'blank', 'saved-1', 'saved-2', 'saved-3', 'saved-4', 'saved-5',
    ])
  })

  it('keeps a pinned established session visible inside the five-row quota', () => {
    const sessions = Array.from({ length: 7 }, (_, index) => ({
      id: `saved-${index + 1}`,
      blank: false,
      pinned: index === 6,
    }))
    expect(foldSessionWindow(
      sessions,
      session => session.blank,
      session => session.pinned,
    ).map(session => session.id)).toEqual([
      'saved-1', 'saved-2', 'saved-3', 'saved-4', 'saved-7',
    ])
  })

  it('auto-expands only when entering an account and preserves a manual collapse', () => {
    expect(expandActiveAccount(null, '__ungrouped__', [])).toEqual(['__ungrouped__'])
    const manuallyCollapsed: string[] = []
    expect(expandActiveAccount('__ungrouped__', '__ungrouped__', manuallyCollapsed)).toBe(manuallyCollapsed)
    expect(expandActiveAccount('__ungrouped__', 'workspace-2', manuallyCollapsed)).toEqual(['workspace-2'])
  })
})
