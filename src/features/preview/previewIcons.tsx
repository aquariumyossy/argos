import type { ReactNode } from "react";

function HitActionIcon({ children }: { children: ReactNode }) {
  return (
    <svg
      className="hit-action-icon"
      viewBox="0 0 16 16"
      width="14"
      height="14"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.5"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
    >
      {children}
    </svg>
  );
}

export function IconOpenFile() {
  return (
    <HitActionIcon>
      <path d="M9 2.5h4.5V7" />
      <path d="M13.5 2.5 7 9" />
      <path d="M7.5 3.5H3.75A1.25 1.25 0 0 0 2.5 4.75v7.5A1.25 1.25 0 0 0 3.75 13.5h7.5a1.25 1.25 0 0 0 1.25-1.25V8.5" />
    </HitActionIcon>
  );
}

export function IconPreview() {
  return (
    <HitActionIcon>
      <path d="M1.75 8s2.25-4 6.25-4 6.25 4 6.25 4-2.25 4-6.25 4-6.25-4-6.25-4Z" />
      <circle cx="8" cy="8" r="1.75" />
    </HitActionIcon>
  );
}

export function IconRescope() {
  return (
    <HitActionIcon>
      <circle cx="7" cy="7" r="4" />
      <path d="m13 13-2.5-2.5" />
    </HitActionIcon>
  );
}

export function IconFolder() {
  return (
    <HitActionIcon>
      <path d="M2.5 5.25A1.25 1.25 0 0 1 3.75 4h2.3l1.2 1.5h5A1.25 1.25 0 0 1 13.5 6.75v4.5A1.25 1.25 0 0 1 12.25 12.5H3.75A1.25 1.25 0 0 1 2.5 11.25v-6Z" />
    </HitActionIcon>
  );
}

export function IconKeep() {
  return (
    <HitActionIcon>
      <path d="M5 2.5h6v6.5l-3 2-3-2V2.5Z" />
      <path d="M8 11v2.5" />
    </HitActionIcon>
  );
}

export function IconChat() {
  return (
    <HitActionIcon>
      <path d="M3.25 3.5h9.5A1.25 1.25 0 0 1 14 4.75v5.25A1.25 1.25 0 0 1 12.75 11.25H8.1L4.75 13.5v-2.25H3.25A1.25 1.25 0 0 1 2 10V4.75A1.25 1.25 0 0 1 3.25 3.5Z" />
    </HitActionIcon>
  );
}

export function IconClose() {
  return (
    <HitActionIcon>
      <path d="M4 4l8 8M12 4l-8 8" />
    </HitActionIcon>
  );
}
