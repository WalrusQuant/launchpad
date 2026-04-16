const { invoke } = window.__TAURI__.core;

let activeProject = null;

function nowIso() {
  return new Date().toISOString();
}

export async function loadProjects() {
  try {
    return await invoke("load_projects");
  } catch (err) {
    console.error("Failed to load projects:", err);
    return [];
  }
}

export async function addProject(path, name = null) {
  return await invoke("add_project", {
    path,
    name,
    lastOpened: nowIso(),
  });
}

export async function removeProject(path) {
  return await invoke("remove_project", { path });
}

export async function renameProject(path, newName) {
  return await invoke("rename_project", { path, newName });
}

export async function focusProjectWindow(path) {
  try {
    return await invoke("focus_project_window", { path });
  } catch (err) {
    console.error("focus_project_window failed:", err);
    return false;
  }
}

export async function openProjectWindow(path) {
  return await invoke("open_new_window", { path });
}

export async function registerProjectWindow(path) {
  const { getCurrentWindow } = window.__TAURI__.window;
  const label = getCurrentWindow().label;
  return await invoke("register_project_window", { path, label });
}

export async function unregisterProjectWindow(path) {
  return await invoke("unregister_project_window", { path });
}

export async function unregisterCurrentWindow() {
  const { getCurrentWindow } = window.__TAURI__.window;
  const label = getCurrentWindow().label;
  return await invoke("unregister_window_label", { label });
}

export async function touchProject(path) {
  return await invoke("touch_project", {
    path,
    lastOpened: nowIso(),
  });
}

export function getActiveProject() {
  return activeProject;
}

export function setActiveProject(project) {
  activeProject = project;
}
