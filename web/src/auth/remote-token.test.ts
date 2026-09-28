import { describe, expect, it } from 'vitest'
import {
  clearRemoteToken,
  readRemoteToken,
  REMOTE_TOKEN_STORAGE_KEY,
  storeRemoteToken,
} from './remote-token'

function memoryStorage(): Storage {
  const values = new Map<string, string>()
  return {
    get length() { return values.size },
    clear: () => values.clear(),
    getItem: key => values.get(key) ?? null,
    key: index => [...values.keys()][index] ?? null,
    removeItem: key => { values.delete(key) },
    setItem: (key, value) => { values.set(key, value) },
  }
}

describe('remote access token storage', () => {
  it('uses tab storage by default and persistent storage only when remembered', () => {
    const tab = memoryStorage()
    const persistent = memoryStorage()

    storeRemoteToken(' tab-token ', false, tab, persistent)
    expect(readRemoteToken(tab, persistent)).toEqual({ token: 'tab-token', remembered: false })
    expect(tab.getItem(REMOTE_TOKEN_STORAGE_KEY)).toBe('tab-token')
    expect(persistent.getItem(REMOTE_TOKEN_STORAGE_KEY)).toBeNull()

    storeRemoteToken('persistent-token', true, tab, persistent)
    expect(readRemoteToken(tab, persistent)).toEqual({ token: 'persistent-token', remembered: true })
    expect(tab.getItem(REMOTE_TOKEN_STORAGE_KEY)).toBeNull()
    expect(persistent.getItem(REMOTE_TOKEN_STORAGE_KEY)).toBe('persistent-token')
  })

  it('clears both lifetimes on logout', () => {
    const tab = memoryStorage()
    const persistent = memoryStorage()
    tab.setItem(REMOTE_TOKEN_STORAGE_KEY, 'tab-token')
    persistent.setItem(REMOTE_TOKEN_STORAGE_KEY, 'persistent-token')

    clearRemoteToken(tab, persistent)

    expect(readRemoteToken(tab, persistent)).toBeNull()
    expect(tab.getItem(REMOTE_TOKEN_STORAGE_KEY)).toBeNull()
    expect(persistent.getItem(REMOTE_TOKEN_STORAGE_KEY)).toBeNull()
  })
})
