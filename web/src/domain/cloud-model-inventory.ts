import { api } from '@/api/client'
import type { CredentialInventory, ModelSelection, ProviderModelReasoning, ProviderProfile } from '@/types'
import type { PublicModel } from '@/components/models/model-service-api'
import { executionTargetHeaders, executionTargetKey, executionTargetPath, type ExecutionTarget } from './execution-target'

export interface CloudModelOption {
  grant_id: string
  grant_name: string
  model: PublicModel
}

export interface CloudModelCurrent {
  selection: ModelSelection
  model: PublicModel | null
  selectable_reasoning: ProviderModelReasoning | null
  source_name: string | null
  available: boolean
  unavailable_reason?: string | null
}

export interface CloudModelOptions {
  current: CloudModelCurrent | null
  options: CloudModelOption[]
  next_cursor: string | null
  providers: ProviderProfile[]
  credentials: CredentialInventory
}

interface CloudInventoryStore {
  cached: CloudModelOptions | null
  inFlight: Promise<CloudModelOptions> | null
  generation: number
  listeners: Set<() => void>
}

const stores = new Map<string, CloudInventoryStore>()

function storeFor(target: ExecutionTarget) {
  const key = executionTargetKey(target)
  let store = stores.get(key)
  if (!store) {
    store = { cached: null, inFlight: null, generation: 0, listeners: new Set() }
    stores.set(key, store)
  }
  return store
}

export function modelSelectionKey(selection: ModelSelection) {
  if (selection.provider === 'computer_provider') return JSON.stringify([selection.provider, selection.executor_id, selection.provider_id, selection.model, selection.reasoning_effort ?? null])
  if (selection.provider === 'platform_model') return JSON.stringify([selection.provider, selection.grant_id, selection.model_id, selection.reasoning_effort ?? null])
  if (selection.provider === 'account_provider') return JSON.stringify([selection.provider, selection.owner_user_id, selection.provider_id, selection.model, selection.reasoning_effort ?? null])
  if (selection.provider === 'named_provider') return JSON.stringify([selection.provider, selection.provider_id, selection.model, selection.reasoning_effort ?? null])
  return JSON.stringify(selection)
}

export function currentCloudModel(inventory: CloudModelOptions | null, selection: ModelSelection) {
  return inventory?.current && modelSelectionKey(inventory.current.selection) === modelSelectionKey(selection) ? inventory.current : null
}

export function cloudModelOptionsPath(target: ExecutionTarget, parameters: Record<string, string> = {}) {
  return executionTargetPath('/model-options', target, { limit: '25', ...parameters })
}

export function peekCloudModelInventory(target: ExecutionTarget) {
  return storeFor(target).cached
}

export function subscribeCloudModelInventory(listener: () => void, target: ExecutionTarget) {
  const store = storeFor(target)
  store.listeners.add(listener)
  return () => { store.listeners.delete(listener) }
}

export function invalidateCloudModelInventory(target: ExecutionTarget) {
  invalidate(storeFor(target))
}

function invalidate(store: CloudInventoryStore) {
  store.generation += 1
  store.cached = null
  store.inFlight = null
  for (const listener of store.listeners) listener()
}

export function invalidateAllCloudModelInventories() {
  for (const store of stores.values()) invalidate(store)
}

export function loadCloudModelInventory(force: boolean, target: ExecutionTarget): Promise<CloudModelOptions> {
  const store = storeFor(target)
  if (!force && store.cached) return Promise.resolve(store.cached)
  if (store.inFlight) return store.inFlight
  const generation = store.generation
  let tracked: Promise<CloudModelOptions>
  tracked = api.request<CloudModelOptions>(cloudModelOptionsPath(target), { headers: executionTargetHeaders(target) }).then(snapshot => {
    if (generation !== store.generation) return store.inFlight && store.inFlight !== tracked ? store.inFlight : loadCloudModelInventory(true, target)
    store.cached = snapshot
    for (const listener of store.listeners) listener()
    return snapshot
  }).finally(() => { if (store.inFlight === tracked) store.inFlight = null })
  store.inFlight = tracked
  return tracked
}

export function resetCloudModelInventoryForTest() {
  stores.clear()
}
