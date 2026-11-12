/** Enter that confirms an IME conversion. Callers must not preventDefault or submit. */
export function isImeEnter(event: { nativeEvent: KeyboardEvent; keyCode: number }): boolean {
  return event.nativeEvent.isComposing || event.keyCode === 229;
}
