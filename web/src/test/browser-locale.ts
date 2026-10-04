// Chinese UI assertions use an explicit browser preference. Locale detection
// tests supply their own language lists or browser contexts.
Object.defineProperties(navigator, {
  language: { configurable: true, value: 'zh-CN' },
  languages: { configurable: true, value: ['zh-CN'] },
})
