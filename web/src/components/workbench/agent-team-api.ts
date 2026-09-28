import { api } from '@/api/client'
import type {
  AgentTeamMessage, AgentTeamSnapshot, AgentTeamTask, AgentTeamTaskDraft, SessionEvent,
} from '@/types'

function teamPath(sessionId: string) {
  return `/sessions/${encodeURIComponent(sessionId)}/team`
}

export const agentTeamApi = {
  snapshot(sessionId: string, signal?: AbortSignal) {
    return api.request<AgentTeamSnapshot>(teamPath(sessionId), { signal })
  },

  events(sessionId: string, signal?: AbortSignal) {
    return api.request<SessionEvent[]>(`/sessions/${encodeURIComponent(sessionId)}/events`, { signal })
  },

  createTask(sessionId: string, draft: AgentTeamTaskDraft) {
    return api.request<AgentTeamTask>(`${teamPath(sessionId)}/tasks`, {
      method: 'POST',
      body: draft,
    })
  },

  replaceTask(sessionId: string, task: AgentTeamTask, draft: AgentTeamTaskDraft) {
    return api.request<AgentTeamTask>(`${teamPath(sessionId)}/tasks/${encodeURIComponent(task.id)}`, {
      method: 'PUT',
      body: { expected_revision: task.revision, ...draft },
    })
  },

  deleteTask(sessionId: string, task: AgentTeamTask) {
    const query = new URLSearchParams({ expected_revision: String(task.revision) })
    return api.request<void>(`${teamPath(sessionId)}/tasks/${encodeURIComponent(task.id)}?${query}`, {
      method: 'DELETE',
    })
  },

  sendMessage(sessionId: string, to: string, content: string) {
    return api.request<AgentTeamMessage>(`${teamPath(sessionId)}/messages`, {
      method: 'POST',
      body: { to, content },
    })
  },

  markMessageRead(sessionId: string, messageId: string) {
    return api.request<AgentTeamMessage>(
      `${teamPath(sessionId)}/messages/${encodeURIComponent(messageId)}/read`,
      { method: 'PUT' },
    )
  },
}
