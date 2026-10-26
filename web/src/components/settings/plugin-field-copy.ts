import type { Translate } from '@/i18n/runtime'
import { fieldLabel, type PluginConfigField } from './plugin-config'

export function pluginFieldCopy(kind: string, field: PluginConfigField, t: Translate<'settings'>) {
  if (kind === 'ternilo.tool.shell' && field.key === 'checks') {
    return { label: t('checks.title'), description: t('checks.description') }
  }
  const subagentFields = {
    max_children: ['coordination.children', 'coordination.childrenHint'],
    max_steps: ['coordination.steps', 'coordination.stepsHint'],
    max_tool_calls: ['coordination.toolCalls', 'coordination.toolCallsHint'],
    foreground_timeout_ms: ['coordination.timeout', 'coordination.timeoutHint'],
  } as const
  if (kind === 'ternilo.subagents.in_process' && field.key in subagentFields) {
    const keys = subagentFields[field.key as keyof typeof subagentFields]
    return { label: t(keys[0]), description: t(keys[1]) }
  }
  if (kind === 'ternilo.session.log' && field.key === 'tool_result_limits') {
    return { label: t('tools.perToolLimit'), description: t('tools.perToolLimitHint') }
  }
  if (kind === 'ternilo.session.log' && field.key === 'max_tool_result_chars') {
    return { label: t('tools.defaultResultLimit'), description: t('tools.defaultResultLimitHint') }
  }
  if (kind === 'ternilo.agent.react' && field.key === 'automatic_reasoning') {
    return { label: t('optimization.automaticReasoning'), description: t('optimization.automaticReasoningHint') }
  }
  const structuredSummary = kind === 'ternilo.context.compaction' && field.key === 'structured_summary'
  const budgetField = kind === 'ternilo.session.log' && field.key === 'max_tool_history_chars'
  const cacheField = kind === 'ternilo.files.local' && field.key === 'read_cache_bytes'
  const workflowFields: Record<string, [string, string]> = {
    'ternilo.tools.project_memory:max_entries': [t('workflow.memoryEntries'), t('workflow.memoryHint')],
    'ternilo.tools.project_memory:max_note_chars': [t('workflow.memoryChars'), t('workflow.memoryHint')],
    'ternilo.tools.github:base_url': [t('workflow.githubUrl'), t('workflow.githubUrlHint')],
    'ternilo.tools.github:credential_ref': [t('workflow.githubCredential'), t('workflow.githubCredentialHint')],
    'ternilo.tools.document_review:browser_command': [t('workflow.browser'), t('workflow.browserHint')],
  }
  const workflowField = workflowFields[`${kind}:${field.key}`]
  const optimizationField = kind === 'ternilo.agent.react' ? field.key : ''
  const codeMapField = kind === 'ternilo.tools.code_map' ? field.key : ''
  const label = workflowField ? workflowField[0] : optimizationField === 'prompt_cache' ? t('optimization.promptCache') : optimizationField === 'observe_reasoning' ? t('optimization.reasoning') : codeMapField === 'max_files' ? t('codeMap.maxFiles') : codeMapField === 'max_file_bytes' ? t('codeMap.maxFileBytes') : budgetField ? t('tools.historyBudget') : cacheField ? t('tools.readCache') : structuredSummary ? t('context.structuredSummary') : fieldLabel(field.key, field.schema)
  const description = workflowField ? workflowField[1] : optimizationField === 'prompt_cache' ? t('optimization.promptCacheHint') : optimizationField === 'observe_reasoning' ? t('optimization.reasoningHint') : codeMapField ? t('codeMap.limitsHint') : budgetField ? t('tools.historyBudgetHint') : cacheField ? t('tools.readCacheHint') : structuredSummary ? t('context.structuredSummaryDescription') : field.schema.description || t('plugins.schemaFieldDescription', { key: field.key })
  return { label, description }
}
