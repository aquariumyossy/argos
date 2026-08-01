import { getCurrentWindow } from "@tauri-apps/api/window";
import Popup from "./features/popup/Popup";
import Settings from "./features/settings/Settings";

function App() {
  const label = getCurrentWindow().label;
  if (label === "popup") {
    return <Popup />;
  }
  return <Settings />;
}

export default App;
