export type ChatShortcutPlan =
  | { action: "focus" }
  | { action: "select"; threadId: string }
  | { action: "reload" }
  | { action: "create" };

/** What Ctrl+Alt+C should do with the conversation already on screen. */
export function planChatShortcut(args: {
  threadId: string;
  activeId: string | null;
  knownThreadIds: readonly string[];
}): ChatShortcutPlan {
  const threadId = args.threadId.trim();
  if (!threadId) {
    if (!args.activeId && args.knownThreadIds.length === 0) {
      return { action: "create" };
    }
    return { action: "focus" };
  }
  if (args.activeId === threadId) return { action: "focus" };
  if (args.knownThreadIds.includes(threadId)) {
    return { action: "select", threadId };
  }
  return { action: "reload" };
}
