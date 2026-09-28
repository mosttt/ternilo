import * as React from 'react'
import type { InputAuthor } from '@/types'

export interface InputViewer {
  local: boolean
  user: { user_id: string; username: string } | null
}

const InputViewerContext = React.createContext<InputViewer>({ local: false, user: null })

export function InputViewerProvider({ children, local, user }: InputViewer & { children: React.ReactNode }) {
  const value = React.useMemo(() => ({ local, user }), [local, user])
  return <InputViewerContext.Provider value={value}>{children}</InputViewerContext.Provider>
}

export function useInputViewer() {
  return React.useContext(InputViewerContext)
}

/** This snapshot is only for the current browser's optimistic message. */
export function optimisticInputAuthor(viewer: InputViewer): InputAuthor | undefined {
  if (viewer.user) return { kind: 'account', ...viewer.user }
  if (viewer.local) return { kind: 'local' }
  return undefined
}
