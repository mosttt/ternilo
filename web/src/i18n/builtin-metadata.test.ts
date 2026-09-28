import { describe, expect, it } from 'vitest'
import type { AgentPresetSummary, PluginCatalogEntry } from '@/types'
import { en } from './resources/builtins'
import { localizeAgentPreset, localizePluginMetadata } from './builtin-metadata'

const t = (key: keyof typeof en) => en[key]

describe('built-in display metadata localization', () => {
  it('localizes Agent limit fields while retaining their numeric constraints and defaults', () => {
    const plugin: PluginCatalogEntry = {
      kind: 'ternilo.agent.react', description: 'Agent', requires: [], provides: [],
      config_schema: { type: 'object', properties: {
        max_tool_calls: { type: 'integer', minimum: 0, default: 512 },
        max_steps: { type: 'integer', minimum: 0, default: 0 },
      } },
    }
    expect(localizePluginMetadata(plugin, t).config_schema).toMatchObject({ properties: {
      max_tool_calls: { title: 'Tool calls per turn', type: 'integer', minimum: 0, default: 512 },
      max_steps: { title: 'Execution steps per turn', type: 'integer', minimum: 0, default: 0 },
    } })
    expect(plugin.config_schema).toEqual({ type: 'object', properties: {
      max_tool_calls: { type: 'integer', minimum: 0, default: 512 },
      max_steps: { type: 'integer', minimum: 0, default: 0 },
    } })
  })

  it('uses stable plugin kind while preserving external author metadata', () => {
    const builtIn: PluginCatalogEntry = {
      kind: 'ternilo.web.search.searxng',
      description: '通过配置的 SearXNG endpoint 搜索网页。',
      requires: [],
      provides: [],
      config_schema: {},
    }
    const external: PluginCatalogEntry = { ...builtIn, kind: 'acme.search', description: '作者原文' }

    expect(localizePluginMetadata(builtIn, t).description).toBe(
      'Searches the web through the configured SearXNG endpoint.',
    )
    expect(localizePluginMetadata(external, t)).toBe(external)
  })

  it('uses stable system preset id while preserving user-authored metadata', () => {
    const system: AgentPresetSummary = {
      id: 'standard', display_name: 'Standard', description: 'Rust 固定中文', trust: 'system',
    }
    const user: AgentPresetSummary = {
      id: 'standard', display_name: '我的 Agent', description: '作者原文', trust: 'user',
    }

    expect(localizeAgentPreset(system, t)).toMatchObject({
      display_name: 'Standard Mode',
      description: 'A full software Agent with both native tools and the Rhai Code Mode SDK.',
    })
    expect(localizeAgentPreset(user, t)).toBe(user)
  })

  it('localizes the complete system preset catalog by stable id', () => {
    const presets: AgentPresetSummary[] = [
      { id: 'standard', display_name: '', description: '', trust: 'system' },
      { id: 'ptc', display_name: '', description: '', trust: 'system' },
      { id: 'minimal', display_name: '', description: '', trust: 'system' },
      { id: 'creative', display_name: '', description: '', trust: 'system' },
    ]

    expect(presets.map(preset => localizeAgentPreset(preset, t).display_name)).toEqual([
      'Standard Mode', 'PTC Mode', 'Minimal Mode', 'Creative Mode',
    ])
  })
})
