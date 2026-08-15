import { useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import DestPicker, { type DestItem } from "./DestPicker";

export type ChatDestThread = DestItem & {
  updatedAt?: number;
};

const LAST_DEST_KEY = "argos.chat.lastDestThreadId";

export function writeLastChatDest(id: string) {
  try {
    localStorage.setItem(LAST_DEST_KEY, id);
  } catch {
    /* ignore */
  }
}

export type AttachToChatResult = {
  added: number;
  skipped: number;
  createdThread: boolean;
  thread: { id: string; title: string };
};

export async function attachToChat(
  items: unknown[],
  title: string | null,
  threadId: "new" | string,
): Promise<AttachToChatResult> {
  const result = await invoke<AttachToChatResult>("llm_attach_sources", {
    items,
    title,
    threadId: threadId === "new" ? "new" : threadId,
  });
  if (result.thread?.id) writeLastChatDest(result.thread.id);
  return result;
}

type Props = {
  className?: string;
  buttonClassName?: string;
  title: string;
  ariaLabel?: string;
  disabled?: boolean;
  children: React.ReactNode;
  onPick: (threadId: "new" | string) => void;
};

export default function ChatDestPicker({
  className,
  buttonClassName,
  title,
  ariaLabel,
  disabled,
  children,
  onPick,
}: Props) {
  const loadItems = useCallback(
    () => invoke<ChatDestThread[]>("llm_list_threads"),
    [],
  );
  const loadActiveId = useCallback(async () => {
    const active = await invoke<ChatDestThread | null>("llm_get_active_thread");
    return active?.id ?? "";
  }, []);

  return (
    <DestPicker
      className={className}
      buttonClassName={buttonClassName}
      title={title}
      ariaLabel={ariaLabel}
      disabled={disabled}
      newLabel="新しいチャット"
      emptyLabel="既存の会話はありません"
      fallbackTitle="新しい会話"
      lastKey={LAST_DEST_KEY}
      loadItems={loadItems}
      loadActiveId={loadActiveId}
      onPick={onPick}
    >
      {children}
    </DestPicker>
  );
}
