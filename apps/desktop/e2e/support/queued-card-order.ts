export interface DesktopUiQueuedCardObservation {
  readonly id: string;
  readonly text: string;
  readonly y: number;
  readonly displayed: boolean;
}

/**
 * The ids of the queued cards on screen, top to bottom, once they match `expectedPrompts`;
 * null while they do not. A card node that is not displayed does not count: the timeline's
 * virtualizer can keep a removed card's node off screen for a while after a cancel.
 */
export function queuedCardIdsInVisualOrder(
  allObservations: readonly DesktopUiQueuedCardObservation[],
  expectedPrompts: readonly string[],
): string[] | null {
  const observations = allObservations.filter((card) => card.displayed);
  if (observations.length !== expectedPrompts.length) return null;
  if (
    observations.some((card) => !card.id || !Number.isFinite(card.y)) ||
    new Set(observations.map((card) => card.id)).size !== observations.length
  ) {
    return null;
  }
  const ordered = [...observations].sort((left, right) => left.y - right.y);
  if (
    ordered.some(
      (card, index) =>
        !card.text.includes(expectedPrompts[index]!) ||
        (index > 0 && card.y <= ordered[index - 1]!.y),
    )
  ) {
    return null;
  }
  return ordered.map((card) => card.id);
}
