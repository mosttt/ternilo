import { describe, expect, it } from 'vitest'
import { ConversationScrollMemory } from './conversation-scroll-memory'

describe('conversation scroll memory', () => {
  it('preserves a paused reading choice even four pixels from the bottom', () => {
    const memory = new ConversationScrollMemory()
    memory.write('first', 'chat', 996, false)
    memory.write('first', 'trajectory', 180, false)
    memory.write('second', 'chat', 1200, true)
    expect(memory.read('first', 'chat')).toBe(996)
    expect(memory.following('first', 'chat')).toBe(false)
    expect(memory.following('second', 'chat')).toBe(true)
  })
  it('keeps independent chat and trajectory positions for every session', () => {
    const memory = new ConversationScrollMemory()

    expect(memory.read('first', 'chat')).toBeUndefined()
    memory.write('first', 'chat', 420)
    memory.write('first', 'trajectory', 180)
    memory.write('second', 'chat', 75)
    memory.write('second', 'trajectory', 0)

    expect(memory.read('first', 'chat')).toBe(420)
    expect(memory.read('first', 'trajectory')).toBe(180)
    expect(memory.read('second', 'chat')).toBe(75)
    expect(memory.read('second', 'trajectory')).toBe(0)
  })
})
