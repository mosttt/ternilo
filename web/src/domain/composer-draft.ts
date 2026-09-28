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
