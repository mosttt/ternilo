import { expect, it } from 'vitest'
import type { SessionStats } from '@/types'
import type { Translate } from '@/i18n/runtime'
import { zh } from '@/i18n/resources/conversation'
import { hasHorizontalOverflow, sessionStatsGroups } from './session-stats-line'

const t: Translate<'conversation'> = (key, params) => zh[key].replace(/\{(\w+)\}/g, (match, name: string) => (
  params && Object.hasOwn(params, name) ? String(params[name]) : match
))

it('enables horizontal scrolling only for measurable content overflow', () => {
  expect(hasHorizontalOverflow(800, 800)).toBe(false)
  expect(hasHorizontalOverflow(801, 800)).toBe(false)
  expect(hasHorizontalOverflow(802, 800)).toBe(true)
})

it('formats the durable session summary', () => {
  const stats: SessionStats = {
    events: 20, turns: 2, completed_turns: 2, failed_turns: 0, cancelled_turns: 0,
    steps: 2, tool_calls: 1, user_messages: 2, assistant_messages: 2,
    estimated_logged_tokens: 0, exact_input_tokens: 15_700, exact_output_tokens: 1_500,
    exact_reasoning_tokens: 620,
    cached_input_tokens: 9_263, model_attempts: 2, measured_model_responses: 2,
    model_duration_ms: 18_200, tool_duration_ms: 0, first_token_duration_ms: 5_400,
    measured_first_tokens: 2, generation_duration_ms: 13_157,
    generation_output_tokens: 1_500,
  }
  expect(sessionStatsGroups(stats, t)).toEqual([
    '2 轮 · 2 步',
    'LLM 18.2s',
    '首 token 平均 2.7s · 114 tok/s',
    '缓存命中 59%',
    '输入 15.7K tok · 输出 1.5K tok · 推理 620 tok',
  ])
})

it('keeps provider token accounting when the count projection has no visible steps', () => {
  const stats: SessionStats = {
    events: 0, turns: 0, completed_turns: 0, failed_turns: 0, cancelled_turns: 0,
    steps: 0, tool_calls: 0, user_messages: 0, assistant_messages: 0,
    estimated_logged_tokens: 0, exact_input_tokens: 200, exact_output_tokens: 20,
    exact_reasoning_tokens: 0,
    cached_input_tokens: 50, model_attempts: 0, measured_model_responses: 1,
    model_duration_ms: 0, tool_duration_ms: 0, first_token_duration_ms: 0,
    measured_first_tokens: 0, generation_duration_ms: 0, generation_output_tokens: 0,
  }
  expect(sessionStatsGroups(stats, t)).toEqual([
    '缓存命中 25%',
    '输入 200 tok · 输出 20 tok',
  ])
})
