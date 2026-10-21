import { getCurrentWindow } from "@tauri-apps/api/window";
import Popup from "./features/popup/Popup";
import Notes from "./features/notes/Notes";
import Chat from "./features/chat/Chat";
import PreviewWindow from "./features/preview/PreviewWindow";
import Settings from "./features/settings/Settings";

function App() {
  if (import.meta.env.DEV && typeof window !== "undefined") {
    const shot = new URLSearchParams(window.location.search).get("popup-shot");
    if (shot === "enter" || shot === "layout") {
      return <Popup />;
    }
  }
  const label = getCurrentWindow().label;
  if (label === "popup") {
    return <Popup />;
  }
  if (label === "notes") {
    return <Notes />;
  }
  if (label === "chat") {
    return <Chat />;
  }
  if (label === "preview") {
    return <PreviewWindow />;
  }
  return <Settings />;
}

export default App;
