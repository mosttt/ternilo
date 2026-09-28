import * as React from 'react'

export type TranscriptViewMode = 'normal' | 'compact'

export const transcriptViewStorageKey = 'ternilo.transcript-view'
const transcriptViewChanged = 'ternilo:transcript-view-changed'

function validMode(value: string | null): value is TranscriptViewMode {
  return value === 'normal' || value === 'compact'
}

export function readTranscriptView(storage: Pick<Storage, 'getItem'> | undefined): TranscriptViewMode {
  if (!storage) return 'compact'
  const value = storage.getItem(transcriptViewStorageKey)
  return validMode(value) ? value : 'compact'
}

export function writeTranscriptView(
  storage: Pick<Storage, 'setItem'> | undefined,
  mode: TranscriptViewMode,
  target: Pick<Window, 'dispatchEvent'> | undefined = typeof window === 'undefined' ? undefined : window,
) {
  storage?.setItem(transcriptViewStorageKey, mode)
  target?.dispatchEvent(new CustomEvent(transcriptViewChanged, { detail: mode }))
}

function subscribe(onChange: () => void) {
  if (typeof window === 'undefined') return () => undefined
  const onStorage = (event: StorageEvent) => {
    if (event.key === transcriptViewStorageKey) onChange()
  }
  window.addEventListener('storage', onStorage)
  window.addEventListener(transcriptViewChanged, onChange)
  return () => {
    window.removeEventListener('storage', onStorage)
    window.removeEventListener(transcriptViewChanged, onChange)
  }
}

function snapshot() {
  return readTranscriptView(typeof window === 'undefined' ? undefined : window.localStorage)
}

export function useTranscriptView() {
  return React.useSyncExternalStore(subscribe, snapshot, () => 'compact')
}
