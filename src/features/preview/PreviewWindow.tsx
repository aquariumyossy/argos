import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import PreviewView from "./PreviewView";
import type { PreviewTarget } from "./types";

export default function PreviewWindow() {
  const [target, setTarget] = useState<PreviewTarget | null>(null);

  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    void invoke<PreviewTarget | null>("get_preview_target")
      .then((t) => {
        if (!cancelled && t) setTarget(t);
      })
      .catch(console.error);
    void listen<PreviewTarget>("preview-target", (event) => {
      if (!cancelled) setTarget(event.payload);
    }).then((fn) => {
      if (cancelled) fn();
      else unlisten = fn;
    });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  return <PreviewView target={target} />;
}
