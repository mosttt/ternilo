import type { Attachment, SubmissionReference } from '@/types'

const DRAFT_PREFIX = 'ternilo.composer.draft.'

export function composerDraftKey(sessionId: string, accountScope?: string) {
  return accountScope
    ? `${DRAFT_PREFIX}scoped.${encodeURIComponent(JSON.stringify([accountScope, sessionId]))}`
    : `${DRAFT_PREFIX}${encodeURIComponent(sessionId)}`
}

export function readComposerDraft(storage: Storage, sessionId: string, accountScope?: string) {
  return storage.getItem(composerDraftKey(sessionId, accountScope)) ?? ''
}

export function writeComposerDraft(storage: Storage, sessionId: string, draft: string, accountScope?: string) {
  const key = composerDraftKey(sessionId, accountScope)
  if (draft === '') storage.removeItem(key)
  else storage.setItem(key, draft)
}

export function committedComposerDraft(current: string, submitted: string) {
  return current !== submitted && current.startsWith(submitted)
    ? current.slice(submitted.length)
    : ''
}

export function restoredFailedDrafts(failed: ReadonlyMap<number, string>) {
  return [...failed.entries()]
    .sort(([left], [right]) => left - right)
    .map(([, draft]) => draft)
    .filter(Boolean)
    .join('\n\n')
}

export interface ComposerDraftReference {
  id: string
  kind: 'file' | 'session'
  label: string
  detail: string
  fileKind?: 'file' | 'directory'
  reference: SubmissionReference
}

interface ComposerExtras {
  attachments: Attachment[]
  references: ComposerDraftReference[]
}

// Keep unsent binary attachments in memory when switching sessions, scoped like text drafts.
const extras = new Map<string, ComposerExtras>()

export function readComposerExtras(sessionId: string, accountScope?: string): ComposerExtras {
  return extras.get(composerDraftKey(sessionId, accountScope)) ?? { attachments: [], references: [] }
}

export function writeComposerExtras(sessionId: string, value: ComposerExtras, accountScope?: string) {
  const key = composerDraftKey(sessionId, accountScope)
  if (value.attachments.length || value.references.length) extras.set(key, value)
  else extras.delete(key)
}

export function hasComposerDraft(storage: Storage, sessionId: string, accountScope?: string) {
  const value = readComposerExtras(sessionId, accountScope)
  return readComposerDraft(storage, sessionId, accountScope) !== '' || value.attachments.length > 0 || value.references.length > 0
}

export function clearComposerDraft(storage: Storage, sessionId: string, accountScope?: string) {
  writeComposerDraft(storage, sessionId, '', accountScope)
  extras.delete(composerDraftKey(sessionId, accountScope))
}
