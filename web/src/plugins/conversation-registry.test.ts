import { MessageSquare } from 'lucide-react'
import { describe, expect, it } from 'vitest'
import '@/components/workbench/builtin-conversation-views'
import {
  conversationViewRegistry,
  primaryConversationView,
  registerConversationView,
} from './conversation-registry'

describe('conversation view contributions', () => {
  it('registers the shipped targets and unloads an added target through its disposer', () => {
    expect(conversationViewRegistry.getSnapshot().map(item => item.id)).toEqual(['chat', 'trajectory'])
    expect(primaryConversationView()?.id).toBe('chat')

    const dispose = registerConversationView({
      id: 'files', order: 15, icon: MessageSquare,
      label: () => 'Files',
      render: () => null,
    })
    expect(conversationViewRegistry.getSnapshot().map(item => item.id)).toEqual(['chat', 'files', 'trajectory'])
    dispose()
    dispose()
    expect(conversationViewRegistry.getSnapshot().map(item => item.id)).toEqual(['chat', 'trajectory'])
  })
})
