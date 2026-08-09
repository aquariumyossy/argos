import { getCurrentWindow } from "@tauri-apps/api/window";
import Popup from "./features/popup/Popup";
import Notes from "./features/notes/Notes";
import Settings from "./features/settings/Settings";

function App() {
  const label = getCurrentWindow().label;
  if (label === "popup") {
    return <Popup />;
  }
  if (label === "notes") {
    return <Notes />;
  }
  return <Settings />;
}

export default App;
