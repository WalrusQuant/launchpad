const { invoke } = window.__TAURI__.core;

let settings = {
  sidebarWidth: 260,
  defaultDirectory: null, // null = auto-detect ~/Code
  fontSize: 13,
  lastDirectory: null,
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
