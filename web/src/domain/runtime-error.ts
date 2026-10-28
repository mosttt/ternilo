import type { Translate } from '@/i18n/runtime'

export interface RuntimeErrorPresentation {
  title: string
  message: string
  raw: string
  requestId?: string
}

type ErrorKey = Parameters<Translate<'chat'>>[0]
const deepseekErrors: Record<string, { title: ErrorKey; message: ErrorKey }> = {
  400: { title: 'runtimeError.deepseek.request.title', message: 'runtimeError.deepseek.request.message' },
  401: { title: 'runtimeError.deepseek.auth.title', message: 'runtimeError.deepseek.auth.message' },
  402: { title: 'runtimeError.deepseek.balance.title', message: 'runtimeError.deepseek.balance.message' },
  422: { title: 'runtimeError.deepseek.parameters.title', message: 'runtimeError.deepseek.parameters.message' },
  429: { title: 'runtimeError.deepseek.rateLimit.title', message: 'runtimeError.deepseek.rateLimit.message' },
  500: { title: 'runtimeError.deepseek.service.title', message: 'runtimeError.deepseek.service.message' },
  503: { title: 'runtimeError.deepseek.overloaded.title', message: 'runtimeError.deepseek.overloaded.message' },
}

const apiErrors: Record<string, { title: ErrorKey; message: ErrorKey }> = {
  invalid_request: { title: 'runtimeError.api.request.title', message: 'runtimeError.api.request.message' },
  service_tier: { title: 'runtimeError.api.serviceTier.title', message: 'runtimeError.api.serviceTier.message' },
  authentication_failed: { title: 'runtimeError.api.auth.title', message: 'runtimeError.api.auth.message' },
  access_denied: { title: 'runtimeError.api.access.title', message: 'runtimeError.api.access.message' },
  not_found: { title: 'runtimeError.api.notFound.title', message: 'runtimeError.api.notFound.message' },
  credit_balance_exhausted: { title: 'runtimeError.api.balance.title', message: 'runtimeError.api.balance.message' },
  organization_spend_limit_exceeded: { title: 'runtimeError.api.orgSpend.title', message: 'runtimeError.api.orgSpend.message' },
  project_spend_limit_exceeded: { title: 'runtimeError.api.projectSpend.title', message: 'runtimeError.api.projectSpend.message' },
  organization_usage_limit_exceeded: { title: 'runtimeError.api.orgUsage.title', message: 'runtimeError.api.orgUsage.message' },
  insufficient_quota: { title: 'runtimeError.api.quota.title', message: 'runtimeError.api.quota.message' },
  rate_limit_exceeded: { title: 'runtimeError.api.rateLimit.title', message: 'runtimeError.api.rateLimit.message' },
  slow_down: { title: 'runtimeError.api.slowDown.title', message: 'runtimeError.api.slowDown.message' },
  request_limited: { title: 'runtimeError.api.limited.title', message: 'runtimeError.api.limited.message' },
  server_error: { title: 'runtimeError.api.service.title', message: 'runtimeError.api.service.message' },
  server_is_overloaded: { title: 'runtimeError.api.overloaded.title', message: 'runtimeError.api.overloaded.message' },
  context_length_exceeded: { title: 'runtimeError.context.title', message: 'runtimeError.context.message' },
}

export function presentRuntimeError(value: unknown, t: Translate<'chat'>): RuntimeErrorPresentation {
  const raw = value instanceof Error ? value.message : String(value ?? '')
  const requestId = raw.match(/; Server request ID: (mrq_[A-Za-z0-9_-]{1,124})$/)?.[1]
  const diagnostic = requestId ? raw.slice(0, raw.lastIndexOf('; Server request ID: ')) : raw
  const presentation = presentDiagnostic(diagnostic, t)
  return requestId ? { ...presentation, raw, requestId } : presentation
}

function presentDiagnostic(raw: string, t: Translate<'chat'>): RuntimeErrorPresentation {
  const normalized = raw.toLocaleLowerCase()
  if (/^(?:execution:\s*)?(?:Server|Worker) model connection ended without completion;/.test(raw)) {
    return { title: t('runtimeError.gateway.title'), message: t('runtimeError.gateway.message'), raw }
  }
  if (/^(?:execution:\s*)?model request failed; check the Server model request log$/.test(raw)) {
    return { title: t('runtimeError.gatewayFailure.title'), message: t('runtimeError.gatewayFailure.message'), raw }
  }
  const toolLimit = normalized.match(/turn exceeded max_tool_calls \((\d+)\)/)

  const webStatus = raw.match(/^(?:execution:\s*)?DeepSeek web tools error \(HTTP (\d{3})\):/)?.[1]
  if (webStatus) {
    const messages: Record<string, ErrorKey> = {
      400: 'runtimeError.deepseekWeb.request', 413: 'runtimeError.deepseekWeb.request',
      401: 'runtimeError.deepseekWeb.auth', 402: 'runtimeError.deepseekWeb.balance',
      403: 'runtimeError.deepseekWeb.blocked', 404: 'runtimeError.deepseekWeb.missing',
      408: 'runtimeError.deepseekWeb.timeout', 504: 'runtimeError.deepseekWeb.timeout',
      429: 'runtimeError.deepseekWeb.limit',
    }
    return { title: t('runtimeError.deepseekWeb.title'), message: t(messages[webStatus] ?? 'runtimeError.deepseekWeb.service'), raw }
  }

  const deepseekStatus = raw.match(/^(?:execution:\s*)?DeepSeek API error \(HTTP (400|401|402|422|429|500|503)\):/i)?.[1]
  const deepseekError = deepseekErrors[deepseekStatus ?? '']
  if (deepseekError) {
    return {
      title: t(deepseekError.title),
      message: t(deepseekError.message),
      raw,
    }
  }

  const apiCode = raw.match(/^(?:execution:\s*)?Model API error \(HTTP \d{3}; ([a-z_]+)\):/)?.[1]
  const apiError = apiCode && Object.hasOwn(apiErrors, apiCode) ? apiErrors[apiCode] : null
  if (apiError) return { title: t(apiError.title), message: t(apiError.message), raw }

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
