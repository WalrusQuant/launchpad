const { invoke } = window.__TAURI__.core;

// Map of absolute path → { tabSize?, wordWrap?, lineEndings? }
let store = {};
let loaded = false;

export async function loadFileSettings() {
  if (loaded) return store;
  try {
    const raw = await invoke("load_file_settings");
    store = JSON.parse(raw) || {};
  } catch (err) {
    console.error("Failed to load file settings:", err);
    store = {};
  }
  loaded = true;
  return store;
}

export function getOverrides(path) {
  return store[path] || null;
}

export async function setOverride(path, key, value) {
  if (!store[path]) store[path] = {};
  store[path][key] = value;
  await persist();
}

export async function clearOverride(path, key) {
  if (!store[path]) return;
  delete store[path][key];
  if (Object.keys(store[path]).length === 0) delete store[path];
  await persist();
}

async function persist() {
  try {
    await invoke("save_file_settings", { data: JSON.stringify(store) });
  } catch (err) {
    console.error("Failed to save file settings:", err);
  }
}
