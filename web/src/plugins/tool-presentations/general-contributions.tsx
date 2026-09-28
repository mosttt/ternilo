import { Braces, Code2, Sparkles, Wrench } from 'lucide-react'
import { GenericToolResult, SkillToolResult } from '@/components/workbench/tool-result-views'
import { toolString } from '@/domain/tool-presentation'
import {
  argumentsOf, defineToolPresentation, exactTitle, genericTarget, genericView,
  type BuiltinView,
} from './shared'

export const skillTool = defineToolPresentation<BuiltinView<'skill'>>({
  id: 'builtin.skill', priority: 110,
  matches: trace => trace.name === 'skill',
  parse: trace => trace.output ? {
    kind: 'skill',
    name: toolString(argumentsOf(trace).name) || trace.id,
    content: trace.output.content,
    error: trace.output.is_error,
  } : null,
  icon: () => Sparkles,
  title: exactTitle,
  target: trace => toolString(argumentsOf(trace).name),
  kind: () => 'skill',
  render: (view, { t }) => <SkillToolResult view={view} t={t} />,
})

export const codeTool = defineToolPresentation<BuiltinView<'generic'>>({
  id: 'builtin.code', priority: 20,
  matches: trace => trace.kind === 'code' || trace.name.includes('code'),
  parse: genericView,
  icon: () => Code2,
  title: (trace, t) => trace.name === 'run_code' ? t('tool.code') : trace.name || t('tool.call'),
  target: genericTarget,
  badge: (_trace, t) => t('tool.codeBadge'),
  render: (view, { t }) => <GenericToolResult view={view} t={t} />,
})

export const orchestrationTool = defineToolPresentation<BuiltinView<'generic'>>({
  id: 'builtin.orchestration', priority: 10,
  matches: trace => trace.name.includes('agent') || trace.name.includes('workflow'),
  parse: genericView,
  icon: () => Braces,
  title: (trace, t) => trace.name || t('tool.call'),
  target: genericTarget,
  render: (view, { t }) => <GenericToolResult view={view} t={t} />,
})

export const genericTool = defineToolPresentation<BuiltinView<'generic'>>({
  id: 'builtin.generic', priority: -1_000,
  matches: () => true,
  parse: genericView,
  icon: () => Wrench,
  title: (trace, t) => trace.name || t('tool.call'),
  target: genericTarget,
  render: (view, { t }) => <GenericToolResult view={view} t={t} />,
})

export const generalToolPresentations = { skillTool, codeTool, orchestrationTool, genericTool }
