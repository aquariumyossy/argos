export type PendingMemo = { id: string; memo: string };

export type SaveSettlement = {
  applyLocal: boolean;
  rewrite: PendingMemo | null;
};

/**
 * A memo save that started before a restore must not stay as the latest.
 * When the restored text is already known, the caller writes it back.
 */
export function settleMemoSave(
  startedEpoch: number,
  currentEpoch: number,
  saved: PendingMemo,
  restored: PendingMemo | null,
): SaveSettlement {
  if (currentEpoch === startedEpoch) {
    return { applyLocal: true, rewrite: null };
  }
  if (restored && restored.id === saved.id && restored.memo !== saved.memo) {
    return { applyLocal: false, rewrite: restored };
  }
  return { applyLocal: false, rewrite: null };
}

export async function finishMemoWrite(
  startedEpoch: number,
  currentEpoch: number,
  saved: PendingMemo,
  restored: PendingMemo | null,
  write: (memo: PendingMemo) => Promise<void>,
): Promise<"apply" | "dropped" | "rewritten"> {
  const decision = settleMemoSave(startedEpoch, currentEpoch, saved, restored);
  if (decision.rewrite) {
    await write(decision.rewrite);
    return "rewritten";
  }
  if (!decision.applyLocal) return "dropped";
  return "apply";
}
