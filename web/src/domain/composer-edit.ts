export type ComposerSelection = {
  start: number
  end: number
}

export type ComposerTrigger = {
  kind: 'command' | 'tool' | 'skill' | 'reference'
  query: string
  range: ComposerSelection
}

export type ComposerEditSnapshot<Reference> = {
  draft: string
  references: readonly Reference[]
  selection: ComposerSelection
}

function selectionAt(value: string, start: number | null, end: number | null): ComposerSelection {
  const maximum = value.length
  const normalizedStart = Math.max(0, Math.min(start ?? maximum, maximum))
  const normalizedEnd = Math.max(normalizedStart, Math.min(end ?? normalizedStart, maximum))
  return { start: normalizedStart, end: normalizedEnd }
}

export function textareaSelection(element: Pick<HTMLTextAreaElement, 'value' | 'selectionStart' | 'selectionEnd'>): ComposerSelection {
  return selectionAt(element.value, element.selectionStart, element.selectionEnd)
}

export function composerTriggerAt(value: string, selection: ComposerSelection): ComposerTrigger | null {
  if (selection.start !== selection.end) return null
  const before = value.slice(0, selection.start)
  const skill = /^[/.]skill(?:\s+([^\s]*))?$/.exec(before)
  if (skill) return { kind: 'skill', query: skill[1] ?? '', range: { start: 0, end: selection.start } }

  const reference = /(^|[\s(])@([^\s@]*)$/.exec(before)
  if (reference) {
    const start = reference.index + reference[1]!.length
    return { kind: 'reference', query: reference[2] ?? '', range: { start, end: selection.start } }
  }

  const command = /^\/[\w!-]*$/.exec(before)
  if (command) return { kind: 'command', query: before.slice(1), range: { start: 0, end: selection.start } }
  const tool = /^\.(?:[a-zA-Z][\w!-]*)?$/.exec(before)
  if (tool) return { kind: 'tool', query: before.slice(1), range: { start: 0, end: selection.start } }
  return null
}

export function replaceComposerRange(value: string, range: ComposerSelection, replacement: string): {
  draft: string
  selection: ComposerSelection
} {
  const draft = `${value.slice(0, range.start)}${replacement}${value.slice(range.end)}`
  const caret = range.start + replacement.length
  return { draft, selection: { start: caret, end: caret } }
}

function cloneSnapshot<Reference>(snapshot: ComposerEditSnapshot<Reference>): ComposerEditSnapshot<Reference> {
  return {
    draft: snapshot.draft,
    references: [...snapshot.references],
    selection: { ...snapshot.selection },
  }
}

export class ComposerEditHistory<Reference> {
  private readonly past: ComposerEditSnapshot<Reference>[] = []
  private readonly future: ComposerEditSnapshot<Reference>[] = []
  private group: { key: string; at: number } | null = null

  record(snapshot: ComposerEditSnapshot<Reference>, groupKey?: string, at = Date.now()): void {
    const coalesced = groupKey !== undefined
      && this.group?.key === groupKey
      && at - this.group.at <= 750
    if (!coalesced) this.past.push(cloneSnapshot(snapshot))
    this.future.length = 0
    this.group = groupKey === undefined ? null : { key: groupKey, at }
  }

  undo(current: ComposerEditSnapshot<Reference>): ComposerEditSnapshot<Reference> | null {
    const previous = this.past.pop()
    if (!previous) return null
    this.future.push(cloneSnapshot(current))
    this.group = null
    return cloneSnapshot(previous)
  }

  redo(current: ComposerEditSnapshot<Reference>): ComposerEditSnapshot<Reference> | null {
    const next = this.future.pop()
    if (!next) return null
    this.past.push(cloneSnapshot(current))
    this.group = null
    return cloneSnapshot(next)
  }

  reset(): void {
    this.past.length = 0
    this.future.length = 0
    this.group = null
  }
}
