import type { PublicModel } from './model-service-api'

export type ModelDeviceScope = { kind: 'account'; include_account_providers?: boolean } | { kind: 'selected'; grants: { grant_id: string; model_ids: string[] }[]; providers?: { provider_id: string; model_ids: string[] }[] }
export interface ModelDeviceProvider { provider_id: string; provider_name: string; models: PublicModel[] }
export interface ModelDeviceLimits {
  monthly_tokens: number | null
  max_concurrent_requests: number | null
  requests_per_minute?: number | null
  expires_at_ms: number | null
}
export interface ModelDeviceUsage {
  month: string
  used_tokens: number
  reserved_tokens: number
  active_requests: number
}
export interface ModelDeviceIdentity {
  device_id: string
  device_name: string
  user_id: string
  username: string
  scope: ModelDeviceScope
  limits?: ModelDeviceLimits
  revoked_at_ms: number | null
  created_at_ms: number
  last_used_at_ms: number | null
}
export interface ModelConnection {
  connection_id: string
  name: string
  server_url: string
  session: { identity: ModelDeviceIdentity; grants: { grant_id: string; grant_name: string; models: PublicModel[] }[]; providers?: ModelDeviceProvider[] }
}
export interface ConnectionAuthorization {
  attempt_id: string
  user_code: string
  verification_uri: string
  expires_at_ms: number
  interval: number
}
export type ConnectionPoll = { status: 'pending'; interval: number } | { status: 'denied' | 'expired' } | { status: 'connected'; connection: ModelConnection }
export function isAccountConnectionProvider(id: string) { return /^server_a_[a-f0-9]{54}$/.test(id) }
export function isConnectionProvider(id: string) { return /^server_[a-f0-9]{56}$/.test(id) || isAccountConnectionProvider(id) }
