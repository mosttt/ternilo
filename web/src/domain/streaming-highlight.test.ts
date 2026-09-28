import { describe, expect, it, vi } from 'vitest'
import {
  HIGHLIGHT_MAX_CODE_LENGTH,
  HIGHLIGHT_MAX_LINE_LENGTH,
  highlightToHtml,
  StreamingHighlightSession,
} from './streaming-highlight'

const text = (line: readonly { text: string }[]) => line.map(span => span.text).join('')

function expectPlainFallback(code: string) {
  expect(new StreamingHighlightSession().update(code, 'ts')).toBeUndefined()

  const template = document.createElement('template')
  const html = highlightToHtml(code, 'ts')!
  template.innerHTML = html
  const pre = template.content.querySelector('pre')!
  const codeElement = pre.querySelector('code')!
  expect(pre.classList.contains('shiki')).toBe(false)
  expect(pre.tabIndex).toBe(0)
  expect(codeElement.className).toBe('language-typescript')
  expect(codeElement.textContent).toBe(code)
  expect(codeElement.querySelector('span')).toBeNull()
  return html
}

describe('StreamingHighlightSession', () => {
  it('continues grammar state and retains completed line identities', () => {
    const live = new StreamingHighlightSession()
    const first = live.update('const value = `first\nsecond', 'ts')!
    const completed = first[0]
    const grown = live.update('const value = `first\nsecond ${name}\n`\nconsole.log(value)', 'ts')!
    const fresh = new StreamingHighlightSession().update('const value = `first\nsecond ${name}\n`\nconsole.log(value)', 'ts')!

    expect(grown[0]).toBe(completed)
    expect(grown.map(text)).toEqual(fresh.map(text))
    expect(grown).toEqual(fresh)
  })

  it('resets on non-append edits and unknown grammars stay plain', () => {
    const session = new StreamingHighlightSession()
    expect(session.update('const before = 1\n', 'ts')).toBeDefined()
    expect(session.update('const replacement = 2', 'ts')?.map(text)).toEqual(['const replacement = 2'])
    expect(session.update('IDENTIFICATION DIVISION.', 'cobol')).toBeUndefined()
  })

  it('does not retain a partial tokenization after the default tokenizer budget expires', () => {
    const live = new StreamingHighlightSession()
    const realNow = Date.now
    let calls = 0
    const clock = vi.spyOn(Date, 'now').mockImplementation(() => {
      calls += 1
      if (calls === 1) return 0
      if (calls === 2) return 1_000
      return realNow()
    })
    live.update('const value = `first\nsecond', 'ts')
    clock.mockRestore()

    const code = 'const value = `first\nsecond ${name}\n`\nconsole.log(value)'
    const grown = live.update(code, 'ts')!
    const fresh = new StreamingHighlightSession().update(code, 'ts')!

    expect(grown).toEqual(fresh)
  })

  it('does not settle timeout-degraded tokens after the default tokenizer budget expires', () => {
    const realNow = Date.now
    let calls = 0
    const clock = vi.spyOn(Date, 'now').mockImplementation(() => {
      calls += 1
      if (calls === 1) return 0
      if (calls === 2) return 1_000
      return realNow()
    })
    const html = highlightToHtml('const value = 1', 'ts')!
    clock.mockRestore()

    expect(html).toContain('color:var(--shiki-token-keyword)')
  })

  it('falls back the whole streaming and settled block when one line is too long', () => {
    const code = `<script>alert('escaped')</script>${'x'.repeat(HIGHLIGHT_MAX_LINE_LENGTH)}`
    const html = expectPlainFallback(code)

    expect(code.length).toBeLessThan(HIGHLIGHT_MAX_CODE_LENGTH)
    expect(html).toContain('&lt;script&gt;alert(&#39;escaped&#39;)&lt;/script&gt;')
    expect(html).not.toContain('<script>')
  })

  it('falls back the whole streaming and settled block when its total size is too large', () => {
    const line = 'const value = 1\n'
    const code = line.repeat(Math.ceil((HIGHLIGHT_MAX_CODE_LENGTH + 1) / line.length))

    expect(code.split('\n', 1)[0]!.length).toBeLessThan(HIGHLIGHT_MAX_LINE_LENGTH)
    expect(code.length).toBeGreaterThan(HIGHLIGHT_MAX_CODE_LENGTH)
    expectPlainFallback(code)
  })

  it('still highlights ordinary multiline code in streaming and settled rendering', () => {
    const code = 'const value: number = 1\nconsole.log(value)'
    const streaming = new StreamingHighlightSession().update(code, 'ts')!
    const settled = highlightToHtml(code, 'ts')!

    expect(streaming.map(text)).toEqual(['const value: number = 1', 'console.log(value)'])
    expect(streaming.flat().some(span => span.style.color === 'var(--shiki-token-keyword)')).toBe(true)
    expect(settled).toContain('class="shiki css-variables"')
    expect(settled).toContain('class="language-typescript"')
    expect(settled).toContain('color:var(--shiki-token-keyword)')
  })
})
