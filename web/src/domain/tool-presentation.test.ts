import { describe, expect, it } from 'vitest'
import '@/plugins/builtin-tool-presentations'
import { toolPresentationRegistry } from '@/plugins/tool-presentation-registry'
import type { ToolTrace } from './events'
import { safeWebUrl, type BuiltinToolPresentation } from './tool-presentation'

function trace(name: string, args: unknown, content: string, isError = false): ToolTrace {
  return {
    id: 'call-1', name, arguments: args, kind: 'tool', children: [],
    started: { seq: 1, occurred_at_ms: 10, run_id: 'run-1', type: 'tool_call_started' },
    finished: { seq: 2, occurred_at_ms: 20, run_id: 'run-1', type: 'tool_call_finished' },
    output: { content, is_error: isError },
  }
}

function presentation(value: ToolTrace) {
  return toolPresentationRegistry.resolve(value)?.result?.view as BuiltinToolPresentation | undefined
}

describe('tool presentation', () => {
  it('projects file reads and literal replace diffs without parsing output as markup', () => {
    expect(presentation(trace('read_file', { path: 'src/a.rs' }, JSON.stringify({
      path: 'src/a.rs', content: '<script>plain</script>\nline 2', start_line: 4, end_line: 5, total_lines: 9,
    })))).toMatchObject({ kind: 'read', path: 'src/a.rs', startLine: 4, content: '<script>plain</script>\nline 2' })
    expect(presentation(trace('replace_in_file', {
      path: 'src/a.rs', old: 'before', new: 'after',
    }, '{"path":"src/a.rs","replacements":1}'))).toEqual({
      kind: 'diff', operation: 'replace', path: 'src/a.rs', before: 'before', after: 'after',
    })
  })

  it('projects search, terminal, and web results from their canonical tool contracts', () => {
    expect(presentation(trace('search_files', { pattern: 'needle' }, JSON.stringify([
      { path: 'a.rs', line: 7, column: 3, preview: 'needle()' },
    ])))).toMatchObject({ kind: 'search', query: 'needle', entries: [{ path: 'a.rs', line: 7 }] })
    expect(presentation(trace('shell', { command: 'cargo test' }, JSON.stringify({
      exit_code: 0, stdout: '\u001b[32mok\u001b[0m', stderr: '', timed_out: false,
    })))).toMatchObject({ kind: 'terminal', command: 'cargo test', exitCode: 0 })
    expect(presentation(trace('web_search', { query: 'Ternilo' }, JSON.stringify([
      { title: 'Docs', url: 'https://example.test/docs', snippet: 'Reference', engine: 'fixture' },
    ])))).toMatchObject({ kind: 'web-search', query: 'Ternilo', sources: [{ title: 'Docs' }] })
  })

  it('keeps errors generic and only exposes HTTP(S) links', () => {
    expect(presentation(trace('read_file', { path: 'x' }, 'permission denied', true))).toEqual({
      kind: 'generic', content: 'permission denied', error: true,
    })
    expect(safeWebUrl('https://example.test')).toBe('https://example.test')
    expect(safeWebUrl('javascript:alert(1)')).toBeUndefined()
  })

  it.each([
    { name: 'glob_files', field: 'files', values: ['src/a.rs'], mode: 'glob', entry: { path: 'src/a.rs' } },
    { name: 'search_files', field: 'matches', values: [{ path: 'src/a.rs', line: 7, column: 3, preview: 'needle()' }], mode: 'search', entry: { path: 'src/a.rs', line: 7, column: 3, preview: 'needle()' } },
  ])('keeps structured $name partial results and search diagnostics', ({ name, field, values, mode, entry }) => {
    expect(presentation(trace(name, { pattern: 'needle' }, JSON.stringify({
      [field]: values, warnings: ['private/: permission denied'], truncated: true,
    })))).toMatchObject({
      kind: 'search', mode, query: 'needle', entries: [entry],
      warnings: ['private/: permission denied'], truncated: true,
    })
  })

  it('preserves legacy glob arrays and distinguishes complete empty search reports', () => {
    expect(presentation(trace('glob_files', { pattern: '*.rs' }, '["a.rs"]'))).toMatchObject({
      kind: 'search', mode: 'glob', entries: [{ path: 'a.rs' }], warnings: [], truncated: false,
    })
    expect(presentation(trace('search_files', { pattern: 'missing' }, '{"matches":[],"warnings":[],"truncated":false}'))).toMatchObject({
      kind: 'search', entries: [], warnings: [], truncated: false,
    })
  })

  it('keeps skill calls in their dedicated replay-stable presentation', () => {
    const skill = trace('skill', { name: 'release-check' }, '<skill_content name="release-check">\nShip it.\n</skill_content>')
    expect(presentation(skill)).toEqual({
      kind: 'skill',
      name: 'release-check',
      content: '<skill_content name="release-check">\nShip it.\n</skill_content>',
      error: false,
    })
  })
})
