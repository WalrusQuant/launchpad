const { invoke } = window.__TAURI__.core;

let settings = {
  sidebarWidth: 260,
  appTheme: "auto", // "auto" | "dark" | "light"

  // Terminal
  termFontSize: 13,
  termFontFamily: '"SF Mono", "Menlo", "Monaco", "Courier New", monospace',
  termScrollback: 10000,
  termCursorStyle: "bar",
  termCursorBlink: true,
  // Editor
  editorFontSize: 12,
  editorTabSize: 2,
  editorWordWrap: false,
  editorVimMode: false,
  // Git
  gitPollInterval: 3,
  gitDefaultPrefix: "",
};

export async function loadSettings() {
  try {
    const data = await invoke("load_settings");
    const saved = JSON.parse(data);
    settings = { ...settings, ...saved };
  } catch (err) {
    console.error("Failed to load settings:", err);
  }
  return settings;
}

export async function saveSetting(key, value) {
  settings[key] = value;
  try {
    await invoke("save_settings", { data: JSON.stringify(settings) });
  } catch (err) {
    console.error("Failed to save settings:", err);
  }
}

export function getSettings() {
  return settings;
}
