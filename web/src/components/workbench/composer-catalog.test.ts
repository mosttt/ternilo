import { describe, expect, it } from 'vitest'
import type { Translate } from '@/i18n/runtime'
import {
  composerCommand, composerCommandInput, composerCommandLabel, composerMenuCommands, composerReference, feedbackCommandText, mergeComposerCommands,
  modelIndependentComposerCommand,
} from './composer-catalog'

const t = ((key: string) => key) as Translate<'conversation'>

describe('composer catalog', () => {
  it('separates the two menus without changing the runtime command protocol or ordinary dot paths', () => {
    const commands = mergeComposerCommands({ session_id: 'session-1', commands: [
      { name: 'agents', description: 'List agents' },
      { name: 'goal', description: 'Update goal' },
    ] }, t)
    expect(composerMenuCommands(commands, 'command', '').map(command => command.value)).toEqual(['/export', '/feedback', '/goal', '/permission', '/plan', '/model'])
    expect(composerMenuCommands(commands, 'tool', '').map(composerCommandLabel)).toEqual(['.agents'])
    const command = composerCommand('.agents', commands)
    expect(command?.execution).toBe('direct')
    expect(composerCommandInput('.agents\nargument', command)).toBe('/agents\nargument')
    for (const input of ['.env', './agents', '../agents', '1.2', '.model', '.unknown']) {
      expect(composerCommand(input, commands), input).toBeNull()
      expect(composerCommandInput(input, null)).toBe(input)
    }
    expect(composerCommand('/agents', commands)).toEqual(command)
  })

  it('merges the Session runtime directory with only true Web contributions', () => {
    const commands = mergeComposerCommands({
      session_id: 'session-1',
      commands: [
        { name: 'read', description: 'Read a workspace file', input: { hint: '<path>', images: false } },
        { name: 'skills', description: 'List available Skills' },
        { name: 'vision', description: 'Inspect attached images', input: { hint: '<question>', images: true } },
      ],
    }, t)
    expect(commands.map(command => command.value)).toEqual(['/export', '/feedback', '/model', '/permission', '/plan', '/read', '/skill', '/skills', '/vision'])
    expect(commands.find(command => command.value === '/vision')?.images).toBe(true)
    expect(commands.find(command => command.value === '/read')?.description).toBe('Read a workspace file')
  })

  it('omits the Skill contribution when the composed runtime has no Skill service', () => {
    const commands = mergeComposerCommands({
      session_id: 'minimal-session',
      commands: [{ name: 'read', description: 'Read a workspace file' }],
    }, t)
    expect(commands.map(command => command.value)).toEqual(['/export', '/feedback', '/model', '/permission', '/plan', '/read'])
  })

  it('separates model-independent commands from Skill and unknown prompts', () => {
    const commands = mergeComposerCommands({
      session_id: 'session-1',
      commands: [{ name: 'goal', description: 'Update the goal', input: { hint: '<objective>', images: false } }],
    }, t)
    expect(composerCommand('/goal ship it', commands)?.execution).toBe('direct')
    expect(modelIndependentComposerCommand('/goal ship it', commands)).toBeNull()
    expect(modelIndependentComposerCommand('/goal resume ship it', commands)).toBeNull()
    for (const action of ['edit', 'complete', 'blocked']) {
      expect(modelIndependentComposerCommand(`/goal ${action} ship it`, commands)?.value).toBe('/goal')
    }
    expect(modelIndependentComposerCommand('/feedback useful', commands)).not.toBeNull()
    expect(modelIndependentComposerCommand('/plan off', commands)).not.toBeNull()
    expect(modelIndependentComposerCommand('/skill release-check', commands)).toBeNull()
    expect(modelIndependentComposerCommand('/unknown', commands)).toBeNull()
    expect(composerCommand('explain /goal', commands)).toBeNull()
  })

  it('converts protocol candidates to typed submission references without prompt syntax', () => {
    expect(composerReference({ kind: 'file', path: 'src/main.rs', file_kind: 'file', label: 'main.rs' })).toEqual({
      id: 'file:src/main.rs', kind: 'file', label: 'main.rs', detail: 'src/main.rs', fileKind: 'file',
      reference: { kind: 'file', path: 'src/main.rs', file_kind: 'file' },
    })
    expect(composerReference({
      kind: 'session', session_id: 's1', label: 'Fix build', workspace: 'workspace-2',
      same_workspace: false, updated_at_ms: 2,
    })).toEqual({
      id: 'session:s1', kind: 'session', label: 'Fix build', detail: 'workspace-2',
      reference: { kind: 'session', session_id: 's1', label: 'Fix build' },
    })
  })

  it('recognizes only the exact feedback command and preserves its unparsed text', () => {
    expect(feedbackCommandText('/feedback the diff is unreadable')).toBe('the diff is unreadable')
    expect(feedbackCommandText('/feedback plan felt SLOW\n twice')).toBe('plan felt SLOW\n twice')
    expect(feedbackCommandText('/feedback')).toBe('')
    expect(feedbackCommandText('/feedback-later')).toBeNull()
    expect(feedbackCommandText('prefix /feedback no')).toBeNull()
  })
})
