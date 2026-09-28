import type { LucideIcon } from 'lucide-react'
import type { ReactNode } from 'react'
import type { DetailsSelection } from '@/components/workbench/details-panel'
import type { Translate } from '@/i18n/runtime'
import type { PendingSubmissionEcho, SessionEvent, SessionProjection } from '@/types'
import { ContributionRegistry } from './contribution-registry'

export interface ConversationViewContext {
  sessionId: string
  events: SessionEvent[]
  pendingSubmissions: PendingSubmissionEcho[]
  projection: SessionProjection | null
  reloadMetadata(): Promise<void>
  selection: DetailsSelection
  onSelect(selection: DetailsSelection): void
  onReaderNavigate(): void
  onRegenerate?(event: SessionEvent): Promise<void>
  onEdit?(event: SessionEvent, input: string): Promise<void>
  regenerateDisabled?: boolean
  history?: { hasOlder: boolean; loading: boolean; error: string; loadOlder(): Promise<void> }
}

export interface ConversationViewContribution {
  id: string
  order: number
  icon: LucideIcon
  label(t: Translate<'conversation'>): string
  openLabel?(t: Translate<'conversation'>): string
  render(context: ConversationViewContext): ReactNode
  contentClassName?: string
  primary?: boolean
  followsTail?: boolean
}

export const conversationViewRegistry = new ContributionRegistry<ConversationViewContribution>(
  contribution => contribution.id,
  (left, right) => left.order - right.order,
)

export const registerConversationView = conversationViewRegistry.register

export function primaryConversationView(
  entries: readonly ConversationViewContribution[] = conversationViewRegistry.getSnapshot(),
): ConversationViewContribution | undefined {
  return entries.find(entry => entry.primary) ?? entries[0]
}
