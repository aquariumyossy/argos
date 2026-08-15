import { useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import DestPicker, { type DestItem } from "../chat/DestPicker";

type NoteDest = DestItem;

const LAST_DEST_KEY = "argos.notes.lastDestNoteId";

export function writeLastNoteDest(id: string) {
  try {
    localStorage.setItem(LAST_DEST_KEY, id);
  } catch {
    /* ignore */
  }
}

export type KeepToNoteResult = {
  created: boolean;
  createdNote: boolean;
  note: { id: string; title: string };
};

export async function keepToNote(
  payload: Record<string, unknown>,
  noteId: "new" | string,
): Promise<KeepToNoteResult> {
  const result = await invoke<KeepToNoteResult>("keep_to_note", {
    payload: {
      ...payload,
      noteId: noteId === "new" ? "new" : noteId,
    },
  });
  if (result.note?.id) writeLastNoteDest(result.note.id);
  return result;
}

type Props = {
  className?: string;
  buttonClassName?: string;
  title: string;
  ariaLabel?: string;
  disabled?: boolean;
  children: React.ReactNode;
  onPick: (noteId: "new" | string) => void;
};

export default function NoteDestPicker({
  className,
  buttonClassName,
  title,
  ariaLabel,
  disabled,
  children,
  onPick,
}: Props) {
  const loadItems = useCallback(() => invoke<NoteDest[]>("list_notes"), []);
  const loadActiveId = useCallback(async () => {
    const active = await invoke<NoteDest | null>("get_active_note");
    return active?.id ?? "";
  }, []);

  return (
    <DestPicker
      className={className}
      buttonClassName={buttonClassName}
      title={title}
      ariaLabel={ariaLabel}
      disabled={disabled}
      newLabel="新しいノート"
      emptyLabel="既存のノートはありません"
      fallbackTitle="無題のノート"
      lastKey={LAST_DEST_KEY}
      loadItems={loadItems}
      loadActiveId={loadActiveId}
      onPick={onPick}
    >
      {children}
    </DestPicker>
  );
}
