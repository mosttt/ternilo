export const REMOTE_TOKEN_STORAGE_KEY = 'ternilo.remote-access-token'

export interface StoredRemoteToken {
  token: string
  remembered: boolean
}

export function readRemoteToken(
  tab: Storage = sessionStorage,
  persistent: Storage = localStorage,
): StoredRemoteToken | null {
  const remembered = persistent.getItem(REMOTE_TOKEN_STORAGE_KEY)?.trim()
  if (remembered) return { token: remembered, remembered: true }
  const token = tab.getItem(REMOTE_TOKEN_STORAGE_KEY)?.trim()
  return token ? { token, remembered: false } : null
}

export function storeRemoteToken(
  token: string,
  remembered: boolean,
  tab: Storage = sessionStorage,
  persistent: Storage = localStorage,
): void {
  clearRemoteToken(tab, persistent)
  const normalized = token.trim()
  if (!normalized) return
  ;(remembered ? persistent : tab).setItem(REMOTE_TOKEN_STORAGE_KEY, normalized)
}

export function clearRemoteToken(
  tab: Storage = sessionStorage,
  persistent: Storage = localStorage,
): void {
  tab.removeItem(REMOTE_TOKEN_STORAGE_KEY)
  persistent.removeItem(REMOTE_TOKEN_STORAGE_KEY)
}
