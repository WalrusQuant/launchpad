const { invoke } = window.__TAURI__.core;
import { loadProjects, addProject, removeProject, renameProject } from "./projects.js";

const pickerRoot = document.getElementById("picker-root");
let homeDir = null;

async function ensureHomeDir() {
  if (homeDir) return homeDir;
  try {
    homeDir = await invoke("get_home_dir");
  } catch {
    homeDir = null;
  }
  return homeDir;
}

function prettyPath(path) {
  if (homeDir && path.startsWith(homeDir)) {
    return "~" + path.slice(homeDir.length);
  }
  return path;
}

function formatRelativeTime(iso) {
  const then = new Date(iso).getTime();
  const now = Date.now();
  const diff = now - then;
  if (Number.isNaN(diff) || diff < 0) return "—";
  const s = Math.floor(diff / 1000);
  if (s < 45) return "just now";
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m ago`;
  const h = Math.floor(m / 60);
  if (h < 24) return `${h}h ago`;
  const d = Math.floor(h / 24);
  if (d < 30) return `${d}d ago`;
  const mo = Math.floor(d / 30);
  if (mo < 12) return `${mo}mo ago`;
  return `${Math.floor(mo / 12)}y ago`;
}

function escapeHtml(s) {
  const d = document.createElement("div");
  d.textContent = s;
  return d.innerHTML;
}

async function pickAndAdd() {
  const picked = await invoke("pick_directory");
  if (!picked) return null;
  return await addProject(picked);
}

function closeRowContextMenu() {
  const existing = document.getElementById("picker-ctx-menu");
  if (existing) existing.remove();
}

function startInlineRename(row, project, onChange) {
  const nameEl = row.querySelector(".picker-row-name");
  if (!nameEl) return;
  const original = project.name;
  const input = document.createElement("input");
  input.type = "text";
  input.className = "picker-row-name-input";
  input.value = original;
  nameEl.replaceWith(input);
  input.focus();
  input.select();

  let done = false;
  const commit = async () => {
    if (done) return;
    done = true;
    const next = input.value.trim();
    if (next && next !== original) {
      await renameProject(project.path, next);
    }
    onChange();
  };
  const cancel = () => {
    if (done) return;
    done = true;
    onChange();
  };

  input.addEventListener("keydown", (e) => {
    if (e.key === "Enter") {
      e.preventDefault();
      commit();
    } else if (e.key === "Escape") {
      e.preventDefault();
      cancel();
    }
  });
  input.addEventListener("blur", commit);
  // Swallow clicks inside the input so the row-click handler doesn't open the project.
  input.addEventListener("click", (e) => e.stopPropagation());
}

function showRowContextMenu(x, y, project, row, onChange) {
  closeRowContextMenu();
  const menu = document.createElement("div");
  menu.id = "picker-ctx-menu";
  menu.className = "picker-ctx-menu";
  menu.innerHTML = `
    <div class="picker-ctx-item" data-action="rename">Rename</div>
    <div class="picker-ctx-item picker-ctx-item-danger" data-action="remove">Remove from list</div>
  `;
  menu.style.left = x + "px";
  menu.style.top = y + "px";
  document.body.appendChild(menu);

  // Flip horizontally if the menu would run off the right edge.
  const rect = menu.getBoundingClientRect();
  if (rect.right > window.innerWidth - 8) {
    menu.style.left = Math.max(8, x - rect.width) + "px";
  }

  menu.querySelector('[data-action="rename"]').addEventListener("click", (e) => {
    e.stopPropagation();
    closeRowContextMenu();
    startInlineRename(row, project, onChange);
  });
  menu.querySelector('[data-action="remove"]').addEventListener("click", async (e) => {
    e.stopPropagation();
    await removeProject(project.path);
    closeRowContextMenu();
    onChange();
  });

  setTimeout(() => {
    document.addEventListener("click", closeRowContextMenu, { once: true });
  }, 0);
}

async function annotateBranches(projects, list) {
  projects.forEach(async (p, i) => {
    try {
      const status = await invoke("get_git_status", { path: p.path });
      if (!status?.branch) return;
      const row = list.children[i];
      const meta = row?.querySelector(".picker-row-meta");
      if (meta) {
        meta.textContent = `${prettyPath(p.path)} · ${status.branch} · ${formatRelativeTime(p.lastOpened)}`;
      }
    } catch {
      // Not a git repo — leave row as-is.
    }
  });
}

export async function showPicker(onOpen) {
  await ensureHomeDir();
  pickerRoot.hidden = false;

  async function rerender() {
    const projects = await loadProjects();
    pickerRoot.innerHTML = "";

    const shell = document.createElement("div");
    shell.className = "picker-shell";

    if (projects.length === 0) {
      shell.innerHTML = `
        <div class="picker-welcome">
          <div class="picker-logo">Launchpad</div>
          <div class="picker-tagline">Open a project to get started.</div>
          <button id="picker-open-btn" class="picker-open-btn">+ Open Project</button>
        </div>
      `;
    } else {
      const header = document.createElement("div");
      header.className = "picker-header";
      header.textContent = "Projects";
      shell.appendChild(header);

      const list = document.createElement("div");
      list.className = "picker-list";
      projects.forEach((p) => {
        const row = document.createElement("div");
        row.className = "picker-row";
        row.tabIndex = 0;
        row.innerHTML = `
          <div class="picker-row-body">
            <div class="picker-row-name">${escapeHtml(p.name)}</div>
            <div class="picker-row-meta">${escapeHtml(prettyPath(p.path))} · ${formatRelativeTime(p.lastOpened)}</div>
          </div>
          <button class="picker-row-menu-btn" title="Project options" aria-label="Project options">⋯</button>
        `;
        row.addEventListener("click", (e) => {
          // Ignore clicks originating from the menu button or inline-rename input.
          if (e.target.closest(".picker-row-menu-btn") || e.target.matches(".picker-row-name-input")) return;
          onOpen(p);
        });
        row.addEventListener("keydown", (e) => {
          if (e.target.matches(".picker-row-name-input")) return;
          if (e.key === "Enter" || e.key === " ") {
            e.preventDefault();
            onOpen(p);
          }
        });
        row.addEventListener("contextmenu", (e) => {
          e.preventDefault();
          showRowContextMenu(e.clientX, e.clientY, p, row, rerender);
        });
        const menuBtn = row.querySelector(".picker-row-menu-btn");
        menuBtn.addEventListener("click", (e) => {
          e.stopPropagation();
          const rect = menuBtn.getBoundingClientRect();
          showRowContextMenu(rect.right, rect.bottom + 4, p, row, rerender);
        });
        list.appendChild(row);
      });
      shell.appendChild(list);

      annotateBranches(projects, list);

      const openBtn = document.createElement("button");
      openBtn.id = "picker-open-btn";
      openBtn.className = "picker-open-btn picker-open-btn-secondary";
      openBtn.textContent = "+ Open Project";
      shell.appendChild(openBtn);
    }

    pickerRoot.appendChild(shell);

    const openBtn = document.getElementById("picker-open-btn");
    if (openBtn) {
      openBtn.addEventListener("click", async () => {
        const project = await pickAndAdd();
        if (project) onOpen(project);
      });
    }
  }

  await rerender();
}

export function hidePicker() {
  pickerRoot.hidden = true;
  pickerRoot.innerHTML = "";
}
