import type { DetailsSelection } from '@/components/workbench/details-panel'
import type { SessionEvent, ToolPresentationDescriptor } from '@/types'
import { asRecord } from '@/lib/utils'
import { buildToolTraces } from './events'

/** Resolve an approval call to its canonical tool trace once execution starts,
 * or to the durable question event while it is still waiting for the user. */
export function approvalDetailsSelection(
  events: readonly SessionEvent[],
  callId: string,
): DetailsSelection {
  const trace = buildToolTraces([...events]).get(callId)
  if (trace) return { kind: 'tool', trace }
  const event = [...events].reverse().find(candidate => {
    if (candidate.type !== 'user_question_asked') return false
    const question = asRecord(candidate.question)
    return asRecord(question.tool_approval).call_id === callId
  })
  if (!event) return null
  const question = asRecord(event.question)
  const approval = asRecord(question.tool_approval)
  return {
    kind: 'approval',
    event,
    approval: {
      tool_name: String(approval.tool_name ?? ''),
      call_id: String(approval.call_id ?? ''),
      reason: String(approval.reason ?? ''),
      arguments: approval.arguments,
      presentation: (approval.presentation as ToolPresentationDescriptor | null | undefined) ?? undefined,
    },
  }
}
