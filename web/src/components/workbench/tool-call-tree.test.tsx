import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { Sparkles } from 'lucide-react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { ToolTrace } from '@/domain/events'
import { registerToolPresentation } from '@/plugins/tool-presentation-registry'
import { ToolCallTree } from './tool-call-tree'

let host: HTMLDivElement
let root: Root
let disposeRuntimeContribution: (() => void) | undefined

function trace(values: Partial<ToolTrace> & Pick<ToolTrace, 'id' | 'name'>): ToolTrace {
  const { id, name, ...rest } = values
  return {
    id,
    name,
    arguments: {},
    children: [],
    kind: 'tool',
    started: { seq: 1, occurred_at_ms: 10, run_id: 'run-1', type: 'tool_call_started' },
    ...rest,
  }
}

function render(value: ToolTrace, selectedCallId?: string, onSelect = vi.fn()) {
  act(() => root.render(<ToolCallTree trace={value} selectedCallId={selectedCallId} onSelect={onSelect} />))
  return onSelect
}

beforeEach(() => {
  ;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true
  host = document.createElement('div')
  document.body.append(host)
  root = createRoot(host)
  disposeRuntimeContribution = undefined
})

afterEach(() => {
  act(() => disposeRuntimeContribution?.())
  act(() => root.unmount())
  host.remove()
  vi.restoreAllMocks()
  delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT
})

describe('ToolCallTree', () => {
  it('renders recursive code dispatches once and selects the exact leaf', () => {
    const leaf = trace({
      id: 'leaf', name: 'read_file', kind: 'code', arguments: { path: 'src/a.rs' },
      output: { content: JSON.stringify({ path: 'src/a.rs', content: 'fn main() {}', start_line: 1, end_line: 1, total_lines: 1 }), is_error: false },
    })
    const child = trace({ id: 'child', name: 'run_code', kind: 'code', children: [leaf] })
    const parent = trace({ id: 'parent', name: 'run_code', children: [child] })
    const onSelect = render(parent, 'leaf')

    expect(host.querySelectorAll('[data-tool-call-id]')).toHaveLength(3)
    expect(host.querySelectorAll('[data-subcalls]')).toHaveLength(2)
    expect(host.querySelector('[data-tool-call-id="leaf"]')?.getAttribute('data-selected')).toBe('true')
    expect(host.querySelector('[data-tool-call-id="parent"]')?.hasAttribute('data-selected')).toBe(false)
    expect(host.querySelector('[data-tool-call-id="parent"] [data-tool-badge]')?.textContent).toBe('代码')

    const expand = host.querySelector<HTMLButtonElement>('[data-tool-call-id="leaf"] [data-tool-call-toggle]')!
    act(() => expand.click())
    expect(expand.getAttribute('aria-expanded')).toBe('true')
    expect(host.querySelector('[data-tool-view="read"]')?.textContent).toContain('fn main() {}')
    const readList = host.querySelector<HTMLOListElement>('[data-tool-view="read"] ol[data-tool-scroll]')
    expect(readList?.tabIndex).toBe(0)
    expect(readList?.getAttribute('aria-label')).toBe('读取文件')
    expect(readList?.getAttribute('role')).toBeNull()

    const inspect = host.querySelector<HTMLButtonElement>('[data-tool-call-id="leaf"] [data-tool-call-inspect]')!
    act(() => inspect.click())
    expect(onSelect).toHaveBeenCalledWith(leaf)
  })

  it('renders ANSI terminal output as styled text without escape bytes', () => {
    const shell = trace({
      id: 'shell', name: 'shell', arguments: { command: 'cargo test' },
      output: { content: JSON.stringify({ exit_code: 0, stdout: '\u001b[32mok\u001b[0m\n', stderr: '', timed_out: false }), is_error: false },
    })
    render(shell)
    const expand = host.querySelector<HTMLButtonElement>('[data-tool-call-toggle]')!
    act(() => expand.click())
    const card = host.querySelector('[data-tool-view="terminal"]')
    expect(card?.textContent).toContain('$ cargo test')
    expect(card?.textContent).toContain('ok')
    expect(card?.textContent).not.toContain('\u001b')
    expect(card?.querySelector<HTMLElement>('[data-tool-terminal-output] span')?.style.color).not.toBe('')
  })

  it('copies the original multiline shell command without prompt or execution metadata', async () => {
    const command = "printf '%s\\n' \"quoted argument\" 'single quotes'\nprintf '%s' 'second line' >&2"
    const writeText = vi.fn().mockResolvedValue(undefined)
    vi.stubGlobal('navigator', { language: 'zh-CN', languages: ['zh-CN'], clipboard: { writeText } })
    try {
      render(trace({
        id: 'shell-copy', name: 'shell', arguments: { command },
        output: { content: JSON.stringify({ exit_code: 0, stdout: 'output', stderr: 'diagnostics', timed_out: false }), is_error: false },
      }))
      expect(host.querySelector('[data-tool-terminal-command]')).toBeNull()
      const toggle = host.querySelector<HTMLButtonElement>('[data-tool-call-toggle]')!
      act(() => toggle.click())
      expect(host.querySelector('[data-tool-terminal-command]')?.textContent).toBe(`$ ${command}`)
      const copy = host.querySelector<HTMLButtonElement>('button[aria-label="复制命令"]')!
      await act(async () => copy.click())
      expect(writeText).toHaveBeenCalledWith(command)
      expect(copy.getAttribute('aria-label')).toBe('已复制')
      expect(host.querySelector('[aria-label="stdout"]')?.textContent).toBe('output')
      expect(host.querySelector('[aria-label="stderr"]')?.textContent).toBe('diagnostics')
      act(() => toggle.click())
      expect(host.querySelector('[data-tool-view="terminal"]')).toBeNull()
    } finally {
      vi.unstubAllGlobals()
    }
  })

  it('keeps the full command available when clipboard access fails', async () => {
    vi.stubGlobal('navigator', { language: 'zh-CN', languages: ['zh-CN'], clipboard: { writeText: vi.fn().mockRejectedValue(new Error('Clipboard denied')) } })
    try {
      render(trace({
        id: 'shell-copy-failed', name: 'shell', arguments: { command: 'printf hello' },
        output: { content: JSON.stringify({ exit_code: 0, stdout: '', stderr: '', timed_out: false }), is_error: false },
      }))
      act(() => host.querySelector<HTMLButtonElement>('[data-tool-call-toggle]')!.click())
      await act(async () => host.querySelector<HTMLButtonElement>('button[aria-label="复制命令"]')!.click())
      expect(host.querySelector('[role="alert"]')?.textContent).toContain('复制失败')
      expect(host.querySelector('[data-tool-terminal-command]')?.textContent).toBe('$ printf hello')
    } finally {
      vi.unstubAllGlobals()
    }
  })

  it('keeps unsafe web result URLs inert', () => {
    const web = trace({
      id: 'web', name: 'web_search', arguments: { query: 'links' },
      output: { content: JSON.stringify([
        { title: 'Safe', url: 'https://example.test', snippet: 'ok' },
        { title: 'Unsafe', url: 'javascript:alert(1)', snippet: 'blocked' },
      ]), is_error: false },
    })
    render(web)
    act(() => host.querySelector<HTMLButtonElement>('[data-tool-call-toggle]')!.click())
    expect(host.querySelector('a[href="https://example.test"]')).not.toBeNull()
    expect(host.querySelector('a[href^="javascript:"]')).toBeNull()
    expect(host.querySelector('[data-tool-view="web-search"]')?.textContent).toContain('Unsafe')
    const results = host.querySelector<HTMLOListElement>('[data-tool-view="web-search"] ol[data-tool-scroll]')
    expect(results?.tabIndex).toBe(0)
    expect(results?.getAttribute('aria-label')).toBe('搜索网页')
    expect(results?.getAttribute('role')).toBeNull()
  })

  it('keeps file-search results as a named keyboard-scrollable list', () => {
    render(trace({
      id: 'search', name: 'search_files', arguments: { pattern: 'needle' },
      output: { content: JSON.stringify([{ path: 'src/main.rs', line: 7, preview: 'needle' }]), is_error: false },
    }))
    act(() => host.querySelector<HTMLButtonElement>('[data-tool-call-toggle]')!.click())
    const results = host.querySelector<HTMLOListElement>('[data-tool-view="search"] ol[data-tool-scroll]')
    expect(results?.tabIndex).toBe(0)
    expect(results?.getAttribute('aria-label')).toBe('搜索结果')
    expect(results?.getAttribute('role')).toBeNull()
  })

  it.each([
    { name: 'glob_files', output: { files: ['src/main.rs'], warnings: ['private/<script>skip()</script>: permission denied'], truncated: false } },
    { name: 'search_files', output: { matches: [{ path: 'src/main.rs', line: 7, preview: 'needle' }], warnings: ['private/<script>skip()</script>: permission denied'], truncated: true } },
  ])('shows successful $name matches alongside partial-search notices', ({ name, output }) => {
    render(trace({
      id: 'partial-search', name, arguments: { pattern: 'needle' },
      output: { content: JSON.stringify(output), is_error: false },
    }))
    act(() => host.querySelector<HTMLButtonElement>('[data-tool-call-toggle]')!.click())
    const result = host.querySelector('[data-tool-view="search"]')
    expect(result?.querySelector('ol[data-tool-scroll]')?.textContent).toContain('src/main.rs')
    expect(result?.getAttribute('data-partial')).toBe('true')
    expect(host.querySelector('[data-tool-call-id="partial-search"]')?.getAttribute('data-state')).toBe('complete')
    expect(host.querySelector('[data-error]')).toBeNull()
    expect(host.querySelector('[data-tool-view="generic"]')).toBeNull()
    const notice = result?.querySelector('[role="note"]')
    expect(notice?.getAttribute('aria-label')).toBe('部分搜索结果')
    expect(notice?.textContent).toContain('private/<script>skip()</script>: permission denied')
    expect(notice?.querySelector('script')).toBeNull()
    if (output.truncated) expect(notice?.textContent).toContain('结果已截断')
  })

  it('shows truncation without warnings and avoids claiming incomplete empty searches had no matches', () => {
    render(trace({
      id: 'truncated-search', name: 'glob_files', arguments: { pattern: '*.rs' },
      output: { content: JSON.stringify({ files: [], warnings: [], truncated: true }), is_error: false },
    }))
    act(() => host.querySelector<HTMLButtonElement>('[data-tool-call-toggle]')!.click())
    expect(host.querySelector('[data-tool-view="search"]')?.textContent).toContain('已检索部分未发现匹配结果')
    expect(host.querySelector('[data-tool-search-notice]')?.textContent).toContain('结果已截断')

    render(trace({
      id: 'empty-search', name: 'search_files', arguments: { pattern: 'missing' },
      output: { content: JSON.stringify({ matches: [], warnings: [], truncated: false }), is_error: false },
    }))
    expect(host.querySelector('[data-tool-view="search"]')?.textContent).toContain('没有结果')
    expect(host.querySelector('[data-tool-search-notice]')).toBeNull()
    expect(host.querySelector('[data-partial]')).toBeNull()
  })

  it('settles a canonically cancelled tool instead of leaving a running spinner', () => {
    render(trace({
      id: 'cancelled', name: 'shell', arguments: { command: 'sleep 30' },
      output: { content: 'tool_call_cancelled', is_error: true },
    }))
    const row = host.querySelector('[data-tool-call-id="cancelled"]')
    expect(row?.getAttribute('data-state')).toBe('cancelled')
    expect(row?.textContent).toContain('已中止')
    expect(row?.textContent).not.toContain('运行中')
    expect(row?.querySelector('.animate-spin')).toBeNull()
  })

  it('includes running, failed, cancelled, and completed state in the toggle name', () => {
    const cases: Array<[ToolTrace, string]> = [
      [trace({ id: 'running', name: 'shell' }), '运行中'],
      [trace({ id: 'failed', name: 'shell', output: { content: 'boom', is_error: true } }), '失败'],
      [trace({ id: 'aborted', name: 'shell', output: { content: 'tool_call_cancelled', is_error: true } }), '已中止'],
      [trace({
        id: 'complete', name: 'shell',
        output: { content: JSON.stringify({ exit_code: 0, stdout: '', stderr: '', timed_out: false }), is_error: false },
      }), '完成'],
    ]

    for (const [value, label] of cases) {
      render(value)
      expect(host.querySelector<HTMLButtonElement>('[data-tool-call-toggle]')?.getAttribute('aria-label')).toContain(`状态：${label}`)
    }
  })

  it('renders a dedicated replay-stable Skill row and exact instructions', () => {
    const skill = trace({
      id: 'skill-call', name: 'skill', arguments: { name: 'release-check' },
      output: {
        content: '<skill_content name="release-check">\nInspect the workspace.\n</skill_content>',
        is_error: false,
      },
    })
    const onSelect = render(skill)
    const row = host.querySelector('[data-tool="skill"]')
    expect(row?.getAttribute('data-state')).toBe('complete')
    expect(row?.textContent).toContain('Skill')
    expect(row?.textContent).toContain('release-check')
    expect(row?.textContent).not.toContain('Inspect the workspace.')

    const toggle = row?.querySelector<HTMLButtonElement>('[data-tool-call-toggle]')
    act(() => toggle?.click())
    const instructions = row?.querySelector('[data-tool-view="skill"]')
    expect(toggle?.getAttribute('aria-expanded')).toBe('true')
    expect(instructions?.getAttribute('aria-label')).toBe('Skill 说明')
    expect(instructions?.textContent).toContain('Inspect the workspace.')

    act(() => row?.querySelector<HTMLButtonElement>('[data-tool-call-inspect]')?.click())
    expect(onSelect).toHaveBeenCalledWith(skill)
  })

  it('renders a durable declarative table without loading plugin JavaScript', () => {
    render(trace({
      id: 'signed',
      name: 'signed_fixture',
      arguments: { subject: 'browser' },
      presentation: {
        title: 'Signed fixture report',
        icon_kind: 'sparkles',
        input_summary: [{ label: 'Subject', path: ['subject'] }],
        result: {
          kind: 'table',
          columns: [
            { label: 'Trust', path: ['status'] },
            { label: 'Message', path: ['message'] },
          ],
        },
      },
      output: {
        content: JSON.stringify([{ status: 'signed', message: 'hello from fixture' }]),
        is_error: false,
      },
    }))

    const row = host.querySelector('[data-tool-call-id="signed"]')
    expect(row?.getAttribute('data-tool-contribution')).toBe('builtin.declarative')
    expect(row?.textContent).toContain('Signed fixture report')
    expect(row?.textContent).toContain('Subject: browser')
    act(() => row?.querySelector<HTMLButtonElement>('[data-tool-call-toggle]')?.click())
    const table = row?.querySelector('[data-tool-view="declarative-table"]')
    expect(table?.textContent).toContain('Trust')
    expect(table?.textContent).toContain('signed')
    expect(table?.textContent).toContain('hello from fixture')
    const scroll = table?.querySelector<HTMLElement>('div[data-tool-scroll]')
    expect(scroll?.tabIndex).toBe(0)
    expect(scroll?.getAttribute('role')).toBe('region')
    expect(scroll?.getAttribute('aria-label')).toBe('输出')
  })

  it('makes declarative Markdown a named scroll region', () => {
    render(trace({
      id: 'markdown', name: 'signed_markdown',
      presentation: {
        title: 'Report', icon_kind: 'sparkles', input_summary: [], result: { kind: 'markdown' },
      },
      output: { content: '# Durable report', is_error: false },
    }))
    act(() => host.querySelector<HTMLButtonElement>('[data-tool-call-toggle]')!.click())
    const markdown = host.querySelector<HTMLElement>('[data-tool-view="declarative-markdown"] [data-tool-scroll]')
    expect(markdown?.tagName).toBe('DIV')
    expect(markdown?.tabIndex).toBe(0)
    expect(markdown?.getAttribute('role')).toBe('region')
    expect(markdown?.getAttribute('aria-label')).toBe('输出')

  })

  it('makes an invalid declarative table fallback a named scroll region', () => {
    render(trace({
      id: 'invalid-table', name: 'signed_table',
      presentation: {
        title: 'Rows', icon_kind: 'database', input_summary: [],
        result: { kind: 'table', columns: [{ label: 'Value', path: ['value'] }] },
      },
      output: { content: '{not a row array}', is_error: false },
    }))
    act(() => host.querySelector<HTMLButtonElement>('[data-tool-call-toggle]')!.click())
    const fallback = host.querySelector<HTMLElement>('[data-tool-view="declarative-table"] pre[data-tool-scroll]')
    expect(fallback?.tabIndex).toBe(0)
    expect(fallback?.getAttribute('role')).toBe('region')
    expect(fallback?.getAttribute('aria-label')).toBe('输出')
  })

  it('switches a mounted generic result to a runtime plugin and back on unload', () => {
    const pluginTrace = trace({
      id: 'runtime-call', name: 'third_party_tool', arguments: { name: 'live' },
      output: { content: 'exact raw output', is_error: false },
    })
    render(pluginTrace)
    act(() => host.querySelector<HTMLButtonElement>('[data-tool-call-toggle]')!.click())

    const row = () => host.querySelector('[data-tool-call-id="runtime-call"]')
    expect(row()?.getAttribute('data-tool-contribution')).toBe('builtin.generic')
    expect(row()?.querySelector('[data-tool-view="generic"]')?.textContent).toBe('exact raw output')

    act(() => {
      disposeRuntimeContribution = registerToolPresentation({
        id: 'test.runtime-tool',
        priority: 10_000,
        matches: value => value.name === 'third_party_tool',
        parse: value => value.output ? { message: `plugin: ${value.output.content}` } : null,
        icon: () => Sparkles,
        title: () => 'Runtime tool',
        target: () => 'live target',
        kind: () => 'runtime',
        render: view => <section data-tool-view="runtime">{view.message}</section>,
      })
    })

    expect(row()?.getAttribute('data-tool-contribution')).toBe('test.runtime-tool')
    expect(row()?.getAttribute('data-tool')).toBe('runtime')
    expect(row()?.textContent).toContain('Runtime tool')
    expect(row()?.textContent).toContain('live target')
    expect(row()?.querySelector('[data-tool-view="runtime"]')?.textContent).toBe('plugin: exact raw output')

    act(() => {
      disposeRuntimeContribution?.()
      disposeRuntimeContribution = undefined
    })
    expect(row()?.getAttribute('data-tool-contribution')).toBe('builtin.generic')
    expect(row()?.querySelector('[data-tool-view="generic"]')?.textContent).toBe('exact raw output')
  })

  it('lets a runtime contribution override and unload back to the built-in code badge', () => {
    const codeTrace = trace({
      id: 'runtime-code', name: 'run_code', kind: 'code',
      output: { content: 'code output', is_error: false },
    })
    render(codeTrace)

    const badge = () => host.querySelector('[data-tool-call-id="runtime-code"] [data-tool-badge]')
    expect(badge()?.textContent).toBe('代码')

    act(() => {
      disposeRuntimeContribution = registerToolPresentation({
        id: 'test.runtime-code',
        priority: 10_000,
        matches: value => value.name === 'run_code',
        parse: value => value.output?.content ?? null,
        icon: () => Sparkles,
        title: () => 'Runtime code',
        target: () => '',
        badge: () => '插件',
        render: view => <pre data-tool-view="runtime-code">{view}</pre>,
      })
    })
    expect(badge()?.textContent).toBe('插件')

    act(() => {
      disposeRuntimeContribution?.()
      disposeRuntimeContribution = undefined
    })
    expect(badge()?.textContent).toBe('代码')
  })
})
