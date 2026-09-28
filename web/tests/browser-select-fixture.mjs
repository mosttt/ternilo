export async function selectChoice(control, value) {
  await control.click()
  const option = typeof value === 'string' ? control.page().locator(`[role="option"][data-choice-option=${JSON.stringify(value)}]`) : control.page().getByRole('option', { name: value.label, exact: true })
  await option.click()
}
