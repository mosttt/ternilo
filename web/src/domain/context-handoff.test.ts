import { describe, expect, it } from 'vitest'
import { parseContextHandoff } from './context-handoff'

describe('structured continuation checkpoints', () => {
  it('retains multilingual facts and distinguishes pending checks from completed work', () => {
    const handoff = parseContextHandoff(JSON.stringify({ objective: '修复 저장', constraints: ['Keep user data'], completed: ['Changed the handler'], pending: ['Run browser checks'], files: ['src/handler.rs'], verification: ['Not yet verified'] }))
    expect(handoff).toMatchObject({ objective: '修复 저장', constraints: ['Keep user data'], pending: ['Run browser checks'], verification: ['Not yet verified'], files: ['src/handler.rs'] })
    expect(handoff?.risks).toEqual([])
  })
  it('leaves legacy text and unknown or malformed summary formats for the existing renderer', () => {
    for (const summary of ['Earlier work: keep user data', '{"objective":"task","pending":true}', '{"objective":"task","extra":"retain this information"}', '{"objective":""}', '[]']) expect(parseContextHandoff(summary)).toBeNull()
  })
})
