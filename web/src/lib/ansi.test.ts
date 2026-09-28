import { describe, expect, it } from 'vitest'
import { parseAnsiLines } from './ansi'

describe('ANSI terminal projection', () => {
  it('preserves visible lines and converts SGR state to inert React styles', () => {
    const lines = parseAnsiLines('\u001b[31merror\u001b[0m\nplain')
    expect(lines.map(line => line.map(span => span.text).join(''))).toEqual(['error', 'plain'])
    expect(lines[0]?.[0]?.style?.color).toBeTruthy()
    expect(lines[1]?.[0]?.style).toBeUndefined()
  })

  it('removes OSC payloads and replays carriage-return progress as visible text', () => {
    const lines = parseAnsiLines('\u001b]0;secret title\u0007old\rnew')
    expect(lines.flatMap(line => line.map(span => span.text)).join('')).toBe('new')
  })
})
