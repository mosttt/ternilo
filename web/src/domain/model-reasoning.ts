import type { ProviderModelReasoning, ReasoningEffort } from '@/types'

export const reasoningEfforts: ReasoningEffort[] = [
  'none', 'minimal', 'low', 'medium', 'high', 'xhigh', 'max',
]

export function supportedReasoningEfforts(reasoning: ProviderModelReasoning | null | undefined) {
  return reasoningEfforts.filter(effort => Object.hasOwn(reasoning?.efforts ?? {}, effort))
}
