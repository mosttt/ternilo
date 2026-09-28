import type { Translate } from '@/i18n/runtime'

export interface RuntimeErrorPresentation {
  title: string
  message: string
  raw: string
}

export function presentRuntimeError(value: unknown, t: Translate<'chat'>): RuntimeErrorPresentation {
  const raw = value instanceof Error ? value.message : String(value ?? '')
  const normalized = raw.toLocaleLowerCase()
  const toolLimit = normalized.match(/turn exceeded max_tool_calls \((\d+)\)/)

  if (/\bcapacity_exhausted\s*:/.test(normalized)) {
    return {
      title: t('runtimeError.capacity.title'),
      message: t('runtimeError.capacity.message'),
      raw,
    }
  }

  if (toolLimit) {
    return {
      title: t('runtimeError.toolLimit.title'),
      message: t('runtimeError.toolLimit.message', { limit: toolLimit[1] }),
      raw,
    }
  }

  if ((normalized.includes('401') || normalized.includes('unauthorized'))
    && (normalized.includes('token') || normalized.includes('api key') || normalized.includes('credential'))) {
    return {
      title: t('runtimeError.auth.title'),
      message: t('runtimeError.auth.message'),
      raw,
    }
  }
  if (normalized.includes('429') || normalized.includes('rate limit')) {
    return {
      title: t('runtimeError.rateLimit.title'),
      message: t('runtimeError.rateLimit.message'),
      raw,
    }
  }
  if (normalized.includes('context_length_exceeded')
    || normalized.includes('maximum context length')
    || normalized.includes('context window')
    || normalized.includes('too many tokens')) {
    return {
      title: t('runtimeError.context.title'),
      message: t('runtimeError.context.message'),
      raw,
    }
  }
  if (normalized.includes('404')
    || normalized.includes('model_not_found')
    || normalized.includes('model not found')
    || normalized.includes('unknown model')) {
    return {
      title: t('runtimeError.model.title'),
      message: t('runtimeError.model.message'),
      raw,
    }
  }
  if ((normalized.includes('403') || normalized.includes('forbidden') || normalized.includes('permission denied'))
    && (normalized.includes('model endpoint')
      || normalized.includes('provider')
      || normalized.includes('api key')
      || normalized.includes('account'))) {
    return {
      title: t('runtimeError.access.title'),
      message: t('runtimeError.access.message'),
      raw,
    }
  }
  if (/\b5\d\d\b/.test(normalized) || normalized.includes('bad gateway') || normalized.includes('service unavailable')) {
    return {
      title: t('runtimeError.service.title'),
      message: t('runtimeError.service.message'),
      raw,
    }
  }
  if (normalized.includes('timed out') || normalized.includes('timeout')) {
    return {
      title: t('runtimeError.timeout.title'),
      message: t('runtimeError.timeout.message'),
      raw,
    }
  }
  if (normalized.includes('parse model stream event') || normalized.includes('expected a sequence')) {
    return {
      title: t('runtimeError.format.title'),
      message: t('runtimeError.format.message'),
      raw,
    }
  }
  if (normalized.includes('connection refused')
    || normalized.includes('error sending request')
    || normalized.includes('dns')
    || normalized.includes('certificate')
    || normalized.includes('tls')) {
    return {
      title: t('runtimeError.connection.title'),
      message: t('runtimeError.connection.message'),
      raw,
    }
  }
  if (normalized.includes('400') || normalized.includes('bad request') || normalized.includes('invalid_request_error')) {
    return {
      title: t('runtimeError.request.title'),
      message: t('runtimeError.request.message'),
      raw,
    }
  }

  return {
    title: t('runtimeError.generic.title'),
    message: raw.replace(/^execution:\s*/i, '') || t('runtimeError.generic.message'),
    raw,
  }
}
