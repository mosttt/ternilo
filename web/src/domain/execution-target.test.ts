import { describe, expect, it } from 'vitest'
import { executionTargetKey, executionTargetPath } from './execution-target'

describe('execution target query', () => {
  it('keeps computer configuration independent of workspaces and separates its cache by space', () => {
    expect(executionTargetPath('/providers', { executorId: 'laptop', tenantId: 'personal' }))
      .toBe('/providers?executor_id=laptop')
    expect(executionTargetKey({ executorId: 'laptop', tenantId: 'personal' }))
      .not.toBe(executionTargetKey({ executorId: 'laptop', tenantId: 'team' }))
  })
  it('targets a Session before its Workspace and preserves other parameters', () => {
    expect(executionTargetKey({ sessionId: 'session/1', workspaceId: 'workspace/1' }))
      .toBe('session:session/1')
    expect(executionTargetPath('/authorizations', {
      sessionId: 'session/1',
      workspaceId: 'workspace/1',
    }, { surface_id: 'web 1' }))
      .toBe('/authorizations?surface_id=web+1&session_id=session%2F1')
  })

  it('uses a Workspace before a Session exists and leaves Cloud paths untouched', () => {
    expect(executionTargetKey({ workspaceId: 'workspace/1' })).toBe('workspace:workspace/1')
    expect(executionTargetPath('/providers', { workspaceId: 'workspace/1' }))
      .toBe('/providers?workspace_id=workspace%2F1')
    expect(executionTargetPath('/providers')).toBe('/providers')
  })
})
