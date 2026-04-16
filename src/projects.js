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
