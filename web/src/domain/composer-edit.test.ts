import { describe, expect, it } from 'vitest'
import {
  ComposerEditHistory,
  composerTriggerAt,
  replaceComposerRange,
  type ComposerEditSnapshot,
} from './composer-edit'

describe('composer edit transactions', () => {
  it('recognizes runtime commands only at the leading dot, without claiming paths or decimals', () => {
    expect(composerTriggerAt('.ag', { start: 3, end: 3 })).toMatchObject({ kind: 'tool', query: 'ag' })
    expect(composerTriggerAt('.skill rel', { start: 10, end: 10 })).toMatchObject({ kind: 'skill', query: 'rel' })
    for (const input of ['./', '../', '.5', '1.2', 'text .agents', '.env.local']) {
      expect(composerTriggerAt(input, { start: input.length, end: input.length }), input).toBeNull()
    }
  })

  it('finds the trigger at the caret and replaces only its exact range', () => {
    const draft = '先看 @gu 再继续'
    const caret = draft.indexOf(' 再继续')
    const trigger = composerTriggerAt(draft, { start: caret, end: caret })

    expect(trigger).toEqual({
      kind: 'reference',
      query: 'gu',
      range: { start: 3, end: 6 },
    })
    expect(replaceComposerRange(draft, trigger!.range, '')).toEqual({
      draft: '先看  再继续',
      selection: { start: 3, end: 3 },
    })
  })

  it('keeps a suffix for command and skill matches but closes on a range selection', () => {
    expect(composerTriggerAt('/go 后缀', { start: 3, end: 3 })).toMatchObject({
      kind: 'command', query: 'go', range: { start: 0, end: 3 },
    })
    expect(composerTriggerAt('/skill rel 后缀', { start: 10, end: 10 })).toMatchObject({
      kind: 'skill', query: 'rel', range: { start: 0, end: 10 },
    })
    expect(composerTriggerAt('@guide', { start: 0, end: 6 })).toBeNull()
  })

  it('coalesces continuous text input and undoes or redoes a reference transaction atomically', () => {
    type Reference = { id: string }
    const history = new ComposerEditHistory<Reference>()
    const snapshot = (draft: string, references: Reference[] = []): ComposerEditSnapshot<Reference> => ({
      draft,
      references,
      selection: { start: draft.length, end: draft.length },
    })

    history.record(snapshot(''), 'insertText', 100)
    history.record(snapshot('@'), 'insertText', 200)
    history.record(snapshot('@g'), 'insertText', 300)
    history.record(snapshot('@gu'))

    const selected = snapshot('', [{ id: 'guide' }])
    expect(history.undo(selected)).toEqual(snapshot('@gu'))
    expect(history.redo(snapshot('@gu'))).toEqual(selected)
    expect(history.undo(selected)).toEqual(snapshot('@gu'))
    expect(history.undo(snapshot('@gu'))).toEqual(snapshot(''))
  })
})
