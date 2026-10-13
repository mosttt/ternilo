import * as React from 'react'

export interface PluginDraft {
  draft: Record<string, unknown>
  texts: Record<string, string>
  rawText: string
  rawInvalid: boolean
  touched: Set<string>
}

type StoredDraft = { baseline: string; value: PluginDraft }
const PluginDraftContext = React.createContext<{
  locks: Set<string>
  locksRef: React.RefObject<Set<string>>
  setLocks: React.Dispatch<React.SetStateAction<Set<string>>>
  drafts: Record<string, StoredDraft>
  setDrafts: React.Dispatch<React.SetStateAction<Record<string, StoredDraft>>>
} | null>(null)

// Keep drafts across settings views and dialog lifetimes, scoped by account and session.
export function PluginDraftProvider({ children }: { children: React.ReactNode }) {
  const locksRef = React.useRef(new Set<string>())
  const [locks, setLocks] = React.useState(new Set<string>())
  const [drafts, setDrafts] = React.useState<Record<string, StoredDraft>>({})
  return <PluginDraftContext.Provider value={{ drafts, setDrafts, locks, locksRef, setLocks }}>{children}</PluginDraftContext.Provider>
}

export function usePluginDraft(key: string | undefined, baseline: string, initial: PluginDraft) {
  const shared = React.useContext(PluginDraftContext)
  const [local, setLocal] = React.useState<StoredDraft>(() => ({ baseline, value: initial }))
  const stored = key && shared ? shared.drafts[key] : local
  const value = stored?.baseline === baseline ? stored.value : initial
  const update = (action: React.SetStateAction<PluginDraft>) => {
    const apply = (previous: StoredDraft | undefined): StoredDraft => {
      const current = previous?.baseline === baseline ? previous.value : initial
      return { baseline, value: typeof action === 'function' ? action(current) : action }
    }
    if (key && shared) shared.setDrafts(current => ({ ...current, [key]: apply(current[key]) }))
    else setLocal(apply)
  }
  React.useEffect(() => {
    if (stored && stored.baseline !== baseline) update(initial)
  }, [baseline, key, stored?.baseline])
  return [value, update] as const
}

// A save may outlive a dialog. Share its lock with the next configuration entry point.
export function usePluginWriteLock(key: string) {
  const shared = React.useContext(PluginDraftContext)
  const localRef = React.useRef(new Set<string>())
  const [local, setLocal] = React.useState(new Set<string>())
  const ref = shared?.locksRef ?? localRef
  const setLocks = shared?.setLocks ?? setLocal
  const start = () => {
    if (ref.current.has(key)) return false
    ref.current.add(key)
    setLocks(new Set(ref.current))
    return true
  }
  const finish = () => {
    ref.current.delete(key)
    setLocks(new Set(ref.current))
  }
  return [(shared?.locks ?? local).has(key), start, finish] as const
}
