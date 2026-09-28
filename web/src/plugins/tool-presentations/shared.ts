import type { ToolTrace } from '@/domain/events'
import {
  toolJson, toolRecord, toolString,
  type BuiltinToolPresentation,
} from '@/domain/tool-presentation'
import type { Translate } from '@/i18n/runtime'
import type { ToolPresentationContribution } from '../tool-presentation-registry'

export type BuiltinView<Kind extends BuiltinToolPresentation['kind']> =
  Extract<BuiltinToolPresentation, { kind: Kind }>

export function defineToolPresentation<View>(
  contribution: ToolPresentationContribution<View>,
) {
  return contribution
}

export function argumentsOf(trace: ToolTrace) {
  return toolRecord(trace.arguments) ?? {}
}

export function outputJson(trace: ToolTrace) {
  return trace.output ? toolJson(trace.output.content) : null
}

export function genericView(trace: ToolTrace): BuiltinView<'generic'> | null {
  return trace.output
    ? { kind: 'generic', content: trace.output.content, error: trace.output.is_error }
    : null
}

export function genericTarget(trace: ToolTrace) {
  const values = argumentsOf(trace)
  return toolString(values.path)
    || toolString(values.command)
    || toolString(values.pattern)
    || toolString(values.query)
    || toolString(values.url)
    || toolString(values.name)
    || toolString(values.terminal_id)
}

const exactTitles: Record<string, Parameters<Translate<'chat'>>[0]> = {
  read_file: 'tool.read',
  write_file: 'tool.write',
  replace_in_file: 'tool.edit',
  search_files: 'tool.search',
  glob_files: 'tool.glob',
  shell: 'tool.shell',
  web_search: 'tool.webSearch',
  web_fetch: 'tool.webFetch',
  terminal_open: 'tool.terminalOpen',
  terminal_send: 'tool.terminalSend',
  terminal_read: 'tool.terminalRead',
  terminal_signal: 'tool.terminalSignal',
  terminal_close: 'tool.terminalClose',
  terminal_list: 'tool.terminalList',
  run_code: 'tool.code',
  skill: 'tool.skill',
}

export function exactTitle(trace: ToolTrace, t: Translate<'chat'>) {
  const key = exactTitles[trace.name]
  return key ? t(key) : trace.name || t('tool.call')
}
