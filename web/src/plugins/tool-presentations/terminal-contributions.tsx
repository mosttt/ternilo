import { TerminalSquare } from 'lucide-react'
import { TerminalToolResult } from '@/components/workbench/tool-result-views'
import { toolNumber, toolRecord, toolString } from '@/domain/tool-presentation'
import {
  argumentsOf, defineToolPresentation, exactTitle, outputJson,
  type BuiltinView,
} from './shared'

export const shellTool = defineToolPresentation<BuiltinView<'terminal'>>({
  id: 'builtin.shell', priority: 100,
  matches: trace => trace.name === 'shell',
  parse: trace => {
    if (!trace.output || trace.output.is_error) return null
    const decoded = toolRecord(outputJson(trace))
    if (!decoded) return null
    return {
      kind: 'terminal',
      command: toolString(argumentsOf(trace).command),
      stdout: toolString(decoded.stdout),
      stderr: toolString(decoded.stderr),
      exitCode: toolNumber(decoded.exit_code),
      timedOut: decoded.timed_out === true,
    }
  },
  icon: () => TerminalSquare,
  title: exactTitle,
  target: trace => toolString(argumentsOf(trace).command),
  render: (view, { t }) => <TerminalToolResult view={view} t={t} />,
})

export const terminalTool = defineToolPresentation<BuiltinView<'terminal'>>({
  id: 'builtin.persistent-terminal', priority: 90,
  matches: trace => trace.name.startsWith('terminal_'),
  parse: trace => {
    if (!trace.output || trace.output.is_error) return null
    const decoded = toolRecord(outputJson(trace))
    if (!decoded) return null
    const args = argumentsOf(trace)
    const terminal = toolRecord(decoded.terminal)
    return {
      kind: 'terminal',
      command: toolString(args.input) || `${trace.name} ${toolString(args.terminal_id)}`.trim(),
      stdout: toolString(decoded.output),
      stderr: '',
      exitCode: toolNumber(terminal?.exit_code ?? decoded.exit_code),
      timedOut: decoded.timed_out === true,
    }
  },
  icon: () => TerminalSquare,
  title: exactTitle,
  target: trace => toolString(argumentsOf(trace).input) || toolString(argumentsOf(trace).terminal_id),
  render: (view, { t }) => <TerminalToolResult view={view} t={t} />,
})

export const terminalToolPresentations = { shellTool, terminalTool }
