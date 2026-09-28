// @vitest-environment jsdom

import { afterEach, describe, expect, it, vi } from 'vitest'
import { api } from '@/api/client'
import type { AgentTeamTask, AgentTeamTaskDraft } from '@/types'
import { agentTeamApi } from './agent-team-api'

const draft: AgentTeamTaskDraft = {
  subject: 'Ship Team UI',
  description: 'Finish the shared surface.',
  status: 'in_progress',
  dependencies: ['task-base'],
  owner: 'member-worker',
}

const task: AgentTeamTask = {
  id: 'task/edit with spaces',
  ...draft,
  revision: 7,
  created_at_ms: 10,
  updated_at_ms: 20,
}

afterEach(() => vi.restoreAllMocks())

describe('Agent Team REST client', () => {
  it('uses full replacement and the current revision for CAS update/delete', async () => {
    const request = vi.spyOn(api, 'request').mockResolvedValue(undefined as never)
    await agentTeamApi.replaceTask('session/a', task, { ...draft, status: 'completed' })
    expect(request).toHaveBeenCalledWith(
      '/sessions/session%2Fa/team/tasks/task%2Fedit%20with%20spaces',
      {
        method: 'PUT',
        body: { expected_revision: 7, ...draft, status: 'completed' },
      },
    )

    await agentTeamApi.deleteTask('session/a', task)
    expect(request).toHaveBeenCalledWith(
      '/sessions/session%2Fa/team/tasks/task%2Fedit%20with%20spaces?expected_revision=7',
      { method: 'DELETE' },
    )
  })

  it('uses the canonical snapshot, create, directed-message, and read routes', async () => {
    const request = vi.spyOn(api, 'request').mockResolvedValue(undefined as never)
    await agentTeamApi.snapshot('root')
    await agentTeamApi.events('child/session')
    await agentTeamApi.createTask('root', draft)
    await agentTeamApi.sendMessage('root', 'member-worker', 'Please review')
    await agentTeamApi.markMessageRead('root', 'message/1')

    expect(request).toHaveBeenNthCalledWith(1, '/sessions/root/team', { signal: undefined })
    expect(request).toHaveBeenNthCalledWith(2, '/sessions/child%2Fsession/events', { signal: undefined })
    expect(request).toHaveBeenNthCalledWith(3, '/sessions/root/team/tasks', { method: 'POST', body: draft })
    expect(request).toHaveBeenNthCalledWith(4, '/sessions/root/team/messages', {
      method: 'POST', body: { to: 'member-worker', content: 'Please review' },
    })
    expect(request).toHaveBeenNthCalledWith(5, '/sessions/root/team/messages/message%2F1/read', { method: 'PUT' })
  })
})
