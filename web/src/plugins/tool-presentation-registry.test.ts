import { Wrench } from 'lucide-react'
import { describe, expect, it, vi } from 'vitest'
import type { ToolTrace } from '@/domain/events'
import { registerBuiltinToolPresentations } from './builtin-tool-presentations'
import { ToolPresentationRegistry, type ToolPresentationContribution } from './tool-presentation-registry'

function trace(): ToolTrace {
  return {
    id: 'call-1', name: 'fixture', arguments: {}, children: [], kind: 'tool',
    started: { seq: 1, occurred_at_ms: 10, run_id: 'run-1', type: 'tool_call_started' },
    output: { content: 'raw output', is_error: false },
  }
}

function contribution(
  id: string,
  priority: number,
  parse: ToolPresentationContribution<string>['parse'] = value => value.output?.content ?? null,
): ToolPresentationContribution<string> {
  return {
    id,
    priority,
    matches: () => true,
    parse,
    icon: () => Wrench,
    title: () => id,
    target: () => '',
    render: value => value,
  }
}

describe('ToolPresentationRegistry', () => {
  it('orders unique contributions by priority and stable ID', () => {
    const registry = new ToolPresentationRegistry()
    registry.register(contribution('z-low', 1))
    registry.register(contribution('z-high', 10))
    registry.register(contribution('a-high', 10))

    expect(registry.getSnapshot().map(entry => entry.id)).toEqual(['a-high', 'z-high', 'z-low'])
    expect(() => registry.register(contribution('z-high', 999))).toThrow(
      'contribution "z-high" is already registered',
    )
    expect(registry.getSnapshot().map(entry => entry.id)).toEqual(['a-high', 'z-high', 'z-low'])
  })

  it('unloads immediately and keeps its disposer idempotent', () => {
    const registry = new ToolPresentationRegistry()
    const listener = vi.fn()
    registry.subscribe(listener)
    registry.register(contribution('fallback', -1))
    const dispose = registry.register(contribution('plugin', 10, () => 'plugin view'))

    expect(registry.resolve(trace())?.result).toMatchObject({
      contribution: { id: 'plugin' },
      view: 'plugin view',
    })
    dispose()
    expect(registry.resolve(trace())?.result).toMatchObject({
      contribution: { id: 'fallback' },
      view: 'raw output',
    })
    expect(listener).toHaveBeenCalledTimes(3)
    dispose()
    expect(listener).toHaveBeenCalledTimes(3)
  })

  it('keeps specialized metadata while an unparsed result falls through honestly', () => {
    const registry = new ToolPresentationRegistry()
    registry.register(contribution('exact-tool', 100, () => null))
    registry.register(contribution('generic-result', -1))

    const resolved = registry.resolve(trace())
    expect(resolved?.contribution.id).toBe('exact-tool')
    expect(resolved?.result).toMatchObject({
      contribution: { id: 'generic-result' },
      view: 'raw output',
    })
  })

  it('disposes the declarative renderer with the rest of its registry bundle', () => {
    const registry = new ToolPresentationRegistry()
    const dispose = registerBuiltinToolPresentations(registry)
    expect(registry.getSnapshot().map(entry => entry.id)).toContain('builtin.declarative')
    dispose()
    expect(registry.getSnapshot()).toEqual([])
    dispose()
    expect(registry.getSnapshot()).toEqual([])
  })
})
