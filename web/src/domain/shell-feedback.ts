import type { Translate } from '@/i18n/runtime'

export function shellFeedback(stderr: string, t: Translate<'chat'>): string | null {
  const messages: string[] = []
  if (stderr.includes('MethodInvocationNotSupportedInConstrainedLanguage')) messages.push(t('tool.constrainedLanguage'))
  if (/\b(EmptyPipeElement|TerminatorExpectedAtEndOfString|UnexpectedToken|MissingEndParenthesisInExpression)\b/.test(stderr)) messages.push(t('tool.powershellSyntax'))
  return messages.length ? messages.join(' ') : null
}
