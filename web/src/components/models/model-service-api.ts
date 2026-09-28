import { api } from '@/api/client'
import type { ProviderModelDefaults, ProviderProfile, ProviderProtocol } from '@/types'

export const modelAdminPath = '/admin/models'
export const modelAccessPath = '/model-access'

export interface ModelProvider {
  profile: ProviderProfile
  enabled: boolean
  has_api_key: boolean
  created_at_ms: number
  updated_at_ms: number
}

export interface PublicModel {
  model_id: string
  display_name: string
  protocol: ProviderProtocol
  defaults: ProviderModelDefaults
}

export interface ModelPublication extends PublicModel {
  provider_id: string
  upstream_model: string
  enabled: boolean
  provider_enabled: boolean
  created_at_ms: number
  updated_at_ms: number
}

export interface ModelGroup {
  group_id: string
  name: string
  description: string | null
  member_count: number
  created_at_ms: number
  updated_at_ms: number
}

export interface ModelUser {
  user_id: string
  username: string
}

export interface ModelQuota {
  month: string
  limit_tokens: number
  used_tokens: number
  reserved_tokens: number
  active_requests: number
  max_concurrent_requests: number
}

export interface ModelGrant {
  grant_id: string
  name: string
  subject: { kind: 'user' | 'group'; id: string }
  subject_name: string | null
  allow_resource_sharing: boolean
  model_ids: string[]
  quota: ModelQuota
  expires_at_ms: number | null
  revoked_at_ms: number | null
  created_at_ms: number
  updated_at_ms: number
}

export interface ModelEntitlement {
  grant: ModelGrant
  models: PublicModel[]
}

export interface ModelKey {
  key_id: string
  user_id: string
  name: string
  token_prefix: string
  grant_id: string
  grant_name: string
  model_ids: string[]
  monthly_tokens: number | null
  max_concurrent_requests: number | null
  expires_at_ms: number | null
  revoked_at_ms: number | null
  created_at_ms: number
  last_used_at_ms: number | null
}

export interface ModelKeyInput {
  name: string
  grant_id: string
  model_ids: string[]
  monthly_tokens: number | null
  max_concurrent_requests: number | null
  expires_at_ms: number | null
}

export interface ModelUsage {
  input_tokens: number | null
  output_tokens: number | null
  cached_input_tokens: number | null
  cache_write_tokens: number | null
  reasoning_tokens: number | null
}

export interface ModelServiceAttempt {
  attempt: number
  state: 'pending' | 'completed' | 'failed' | 'cancelled'
  attempted: boolean
  reserved_tokens: number
  accounted_tokens: number | null
  usage: ModelUsage | null
  upstream_request_id?: string | null
  error_code?: string | null
  created_at_ms: number
  settled_at_ms: number | null
}

export interface ModelServiceRequest {
  request_id: string
  origin: 'api_key' | 'client_device' | 'workload'
  source: 'platform_grant' | 'user_provider'
  key_id: string | null
  actor_user_id: string
  resource_owner_user_id: string | null
  model_beneficiary_user_id: string
  workload: { session_id: string; run_id: string } | null
  grant_id: string | null
  grant_name: string | null
  attempts: ModelServiceAttempt[]
  model_id: string
  provider_id?: string
  upstream_model?: string
  protocol: ProviderProtocol
  state: 'pending' | 'completed' | 'failed' | 'cancelled'
  attempted: boolean
  reserved_tokens: number
  accounted_tokens: number | null
  usage: ModelUsage | null
  upstream_request_id?: string | null
  error_code?: string | null
  month: string
  created_at_ms: number
  expires_at_ms: number
  settled_at_ms: number | null
}

export interface ModelUsageReport {
  month: string
  request_count: number
  active_requests: number
  unknown_requests: number
  used_tokens: number
  reserved_tokens: number
  input_tokens: number
  output_tokens: number
  cached_input_tokens: number
  cache_write_tokens: number
  reasoning_tokens: number
}

export function modelResource(path: string, id: string) {
  return `${path}/${encodeURIComponent(id)}`
}

export function createModelKey(input: ModelKeyInput) {
  return api.request<{ key: ModelKey; token: string }>(`${modelAccessPath}/keys`, { method: 'POST', body: input })
}

export function revokeModelKey(id: string) {
  return api.request<void>(modelResource(`${modelAccessPath}/keys`, id), { method: 'DELETE' })
}
