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

live.onFrame(frame => {
  if (window.__TERNILO_BOOT__?.platform && frame.type === 'error'
    && frame.subscription_id === undefined && frame.code === 'policy_denied') {
    // HTTP distinguishes an expired login from a valid account denied by policy.
    void api.request('/auth/session').catch(() => undefined)
  }
})

live.onStatus(status => {
  if (status === 'reconnecting' && usesLocalBootstrap()) {
    void api.request('/state').catch(() => undefined)
  }
})
