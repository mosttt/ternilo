import type { AgentPresetSummary, PluginCatalogEntry } from '@/types'
import type { Translate } from './runtime'

const pluginDescriptionKeys = {
  'ternilo.session.log': 'plugin.sessionLog',
  'ternilo.session_title.llm': 'plugin.sessionTitle',
  'ternilo.tool.session_query': 'plugin.sessionQuery',
  'ternilo.prompt.registry': 'plugin.promptRegistry',
  'ternilo.tools.registry': 'plugin.toolRegistry',
  'ternilo.tools.agent_team': 'plugin.agentTeam',
  'ternilo.hooks.registry': 'plugin.hookRegistry',
  'ternilo.hooks.claude_code': 'plugin.claudeCodeHooks',
  'ternilo.hooks.codex': 'plugin.codexHooks',
  'ternilo.prompt.system': 'plugin.systemPrompt',
  'ternilo.prompt.identity': 'plugin.identityPrompt',
  'ternilo.prompt.section': 'plugin.promptSection',
  'ternilo.prompt.workspace_instructions': 'plugin.workspaceInstructions',
  'ternilo.tools.runtime_extensions': 'plugin.runtimeExtensions',
  'ternilo.tool.schedule': 'plugin.schedule',
  'ternilo.context.compaction': 'plugin.contextCompaction',
  'ternilo.tools.files': 'plugin.workspaceFiles',
  'ternilo.tool.shell': 'plugin.shellTool',
  'ternilo.tool.ask_user': 'plugin.askUser',
  'ternilo.tool.plan': 'plugin.plan',
  'ternilo.skills.registry': 'plugin.skillRegistry',
  'ternilo.skills.filesystem': 'plugin.filesystemSkills',
  'ternilo.tools.skills': 'plugin.skillTools',
  'ternilo.subagents.in_process': 'plugin.subagent',
  'ternilo.subagents.acp': 'plugin.acpSubagent',
  'ternilo.tools.terminal': 'plugin.terminalTools',
  'ternilo.telemetry.otlp': 'plugin.telemetry',
  'ternilo.mcp.stdio': 'plugin.mcp',
  'ternilo.tools.jobs': 'plugin.jobsTool',
  'ternilo.lsp.stdio': 'plugin.lsp',
  'ternilo.tool.web_fetch': 'plugin.webFetch',
  'ternilo.web.search.searxng': 'plugin.webSearch',
  'ternilo.web.search.brave': 'plugin.braveSearch',
  'ternilo.web.search.tavily': 'plugin.tavilySearch',
  'ternilo.workflow.rhai': 'plugin.workflowRuntime',
  'ternilo.tools.workflow': 'plugin.workflowTools',
  'ternilo.model.rule': 'plugin.ruleModel',
  'ternilo.model.openai_compatible': 'plugin.openAiModel',
  'ternilo.model.replay': 'plugin.replayModel',
  'ternilo.agent.react': 'plugin.agentLoop',
  'ternilo.code_runtime.rhai': 'plugin.codeRuntime',
  'ternilo.tools.code_mode': 'plugin.codeMode',
  'ternilo.extension.package': 'plugin.extensionPackage',
  'ternilo.sandbox.local': 'plugin.localSandbox',
  'ternilo.files.local': 'plugin.localFiles',
  'ternilo.shell.local': 'plugin.localShell',
  'ternilo.terminals.local': 'plugin.localTerminals',
  'ternilo.jobs.local': 'plugin.localJobs',
  'ternilo.sandbox.cloud_outer': 'plugin.cloudSandbox',
  'ternilo.model.host_gateway': 'plugin.hostGatewayModel',
} as const

const presetKeys = {
  standard: {
    name: 'preset.standard.name',
    description: 'preset.standard.description',
  },
  ptc: { name: 'preset.ptc.name', description: 'preset.ptc.description' },
  minimal: {
    name: 'preset.minimal.name',
    description: 'preset.minimal.description',
  },
  creative: {
    name: 'preset.creative.name',
    description: 'preset.creative.description',
  },
} as const

export function localizePluginMetadata(plugin: PluginCatalogEntry, t: Translate<'builtins'>): PluginCatalogEntry {
  const key = pluginDescriptionKeys[plugin.kind as keyof typeof pluginDescriptionKeys]
  const localized = key ? { ...plugin, description: t(key) } : plugin
  if (plugin.kind !== 'ternilo.agent.react') return localized
  const properties = plugin.config_schema.properties
  if (!properties || typeof properties !== 'object' || Array.isArray(properties)) return localized
  const fields = { ...properties } as Record<string, unknown>
  if (fields.max_tool_calls && typeof fields.max_tool_calls === 'object') {
    fields.max_tool_calls = {
      ...fields.max_tool_calls,
      title: t('field.agentToolCalls.title'),
      description: t('field.agentToolCalls.description'),
    }
  }
  if (fields.max_steps && typeof fields.max_steps === 'object') {
    fields.max_steps = {
      ...fields.max_steps,
      title: t('field.agentSteps.title'),
      description: t('field.agentSteps.description'),
    }
  }
  return { ...localized, config_schema: { ...plugin.config_schema, properties: fields } }
}

export function localizeAgentPreset<T extends AgentPresetSummary>(preset: T, t: Translate<'builtins'>): T {
  if (preset.trust !== 'system') return preset
  const keys = presetKeys[preset.id as keyof typeof presetKeys]
  return keys
    ? {
        ...preset,
        display_name: t(keys.name),
        description: t(keys.description),
      }
    : preset
}
