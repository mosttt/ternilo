import type { ApplicationCatalog, CredentialInventory, ModelSelection, Profile, ProviderProfile } from '@/types'

export function profileModelIsUsable(
  profile: Profile | null,
  catalog: ApplicationCatalog | null,
  credentials: CredentialInventory | null,
) {
  const model = profile?.plugins.find(entry => entry.enabled && catalog?.plugins.some(plugin => (
    plugin.kind === entry.kind && plugin.provides.includes('ternilo/models@3')
  )))
  if (!model || model.kind === 'ternilo.model.rule') return false
  if (model.kind !== 'ternilo.model.openai_compatible') return true

  const config = model.config as Record<string, unknown>
  if (!config || typeof config !== 'object') return false
  if (typeof config.base_url !== 'string' || !config.base_url.trim()
    || typeof config.model !== 'string' || !config.model.trim()) return false
  return !config.api_key_env || credentials?.references.some(reference => (
    reference.reference === config.api_key_env && reference.configured
  )) === true
}

export function providerIsUsable(provider: ProviderProfile, credentials: CredentialInventory | null) {
  if (provider.models.length === 0) return false
  if (provider.source === 'operator' || !provider.api_key_ref) return true
  return credentials?.references.some(reference => (
    reference.reference === provider.api_key_ref && reference.configured
  )) ?? false
}

export function usableProviderModels(providers: ProviderProfile[], credentials: CredentialInventory | null) {
  return providers.reduce((count, provider) => (
    count + (providerIsUsable(provider, credentials) ? provider.models.length : 0)
  ), 0)
}

export function modelSelectionIsUsable(
  selection: ModelSelection,
  providers: ProviderProfile[],
  credentials: CredentialInventory | null,
  profileAvailable = false,
) {
  if (selection.provider === 'platform_model') return false
  if (selection.provider === 'account_provider') return profileAvailable
  if (selection.provider === 'open_ai_compatible') {
    return Boolean(selection.base_url.trim() && selection.model.trim())
      && (!selection.api_key_env || credentials?.references.some(reference => (
        reference.reference === selection.api_key_env && reference.configured
      )) === true)
  }
  if (selection.provider === 'profile_default') {
    return profileAvailable
  }
  const provider = providers.find(item => item.id === selection.provider_id)
  return Boolean(
    provider
    && providerIsUsable(provider, credentials)
    && provider.models.some(model => model.id === selection.model),
  )
}
