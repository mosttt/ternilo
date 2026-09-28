import { api } from './client'
import { LiveClient } from './live-client'
import { usesLocalBootstrap } from '@/auth/local'

function liveUrl() {
  const url = new URL('/api/v1/live', window.location.href)
  url.protocol = url.protocol === 'https:' ? 'wss:' : 'ws:'
  return url.href
}

export const live = new LiveClient({
  url: liveUrl(),
  credentials: () => api.liveCredentials(),
})

api.onCredentialsChanged(() => live.credentialsChanged())

live.onStatus(status => {
  if (status === 'reconnecting' && usesLocalBootstrap()) {
    void api.request('/state').catch(() => undefined)
  }
})
