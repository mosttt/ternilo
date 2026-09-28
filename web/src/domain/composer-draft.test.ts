import { beforeEach, describe, expect, it } from 'vitest'
import {
  committedComposerDraft, composerDraftKey, readComposerDraft, restoredFailedDrafts, writeComposerDraft,
} from './composer-draft'

class MemoryStorage implements Storage {
  private readonly values = new Map<string, string>()

  get length() { return this.values.size }
  clear() { this.values.clear() }
  getItem(key: string) { return this.values.get(key) ?? null }
  key(index: number) { return [...this.values.keys()][index] ?? null }
  removeItem(key: string) { this.values.delete(key) }
  setItem(key: string, value: string) { this.values.set(key, value) }
}

describe('composer draft persistence', () => {
  const storage = new MemoryStorage()
  beforeEach(() => storage.clear())

  it('keeps private account drafts separate for the same shared session and preserves local drafts', () => {
    writeComposerDraft(storage, 'shared-session', 'local draft')
    writeComposerDraft(storage, 'shared-session', 'Alice private draft', '["alice","space"]')
    writeComposerDraft(storage, 'shared-session', 'Bob private draft', '["bob","space"]')
    expect(readComposerDraft(storage, 'shared-session')).toBe('local draft')
    expect(readComposerDraft(storage, 'shared-session', '["alice","space"]')).toBe('Alice private draft')
    expect(readComposerDraft(storage, 'shared-session', '["bob","space"]')).toBe('Bob private draft')
    writeComposerDraft(storage, 'shared-session', '', '["bob","space"]')
    expect(readComposerDraft(storage, 'shared-session', '["alice","space"]')).toBe('Alice private draft')
    expect(readComposerDraft(storage, 'shared-session', '["bob","space"]')).toBe('')
  })

  it('isolates drafts by session and rehydrates exact whitespace', () => {
    writeComposerDraft(storage, 'session/one', '  first draft\n')
    writeComposerDraft(storage, 'session/two', 'second draft')

    expect(readComposerDraft(storage, 'session/one')).toBe('  first draft\n')
    expect(readComposerDraft(storage, 'session/two')).toBe('second draft')
    expect(readComposerDraft(storage, 'session/three')).toBe('')
    expect(composerDraftKey('session/one')).not.toBe(composerDraftKey('session/two'))
  })

  it('removes the persisted item when a send clears the draft', () => {
    writeComposerDraft(storage, 'session', 'ready')
    expect(storage.getItem(composerDraftKey('session'))).toBe('ready')

    writeComposerDraft(storage, 'session', '')
    expect(storage.getItem(composerDraftKey('session'))).toBeNull()
  })

  it('keeps only a pure suffix typed while the submitted snapshot is in flight', () => {
    expect(committedComposerDraft('sent text', 'sent text')).toBe('')
    expect(committedComposerDraft('sent textnext draft', 'sent text')).toBe('next draft')
    expect(committedComposerDraft('edited sent text', 'sent text')).toBe('')
  })

  it('restores concurrent failures in submission order rather than settlement order', () => {
    expect(restoredFailedDrafts(new Map([[2, 'second'], [1, 'first']]))).toBe('first\n\nsecond')
  })
})
