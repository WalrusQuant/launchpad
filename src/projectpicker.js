const { invoke } = window.__TAURI__.core;
import { loadProjects, addProject, removeProject, renameProject, focusProjectWindow, openProjectWindow } from "./projects.js";

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

// Stable per-project identity color so you learn to recognise a project by
// the colour of its tile, not by reading the name. Hash the path (stable even
// if renamed) onto the app's accent palette.
const TILE_ACCENTS = [
  "--accent-blue",
  "--accent-green",
  "--accent-cyan",
  "--accent-teal",
  "--accent-magenta",
  "--accent-warm",
  "--accent-yellow",
];
function accentFor(key) {
  let h = 0;
  for (let i = 0; i < key.length; i++) h = (h * 31 + key.charCodeAt(i)) >>> 0;
  return TILE_ACCENTS[h % TILE_ACCENTS.length];
}
function initialsFor(name) {
  const cleaned = (name || "").replace(/[^A-Za-z0-9]+/g, " ").trim();
  if (!cleaned) return "?";
  const parts = cleaned.split(/\s+/);
  if (parts.length >= 2) return (parts[0][0] + parts[1][0]).toUpperCase();
  return cleaned.slice(0, 2).toUpperCase();
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
    e.stopPropagation();
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
    <div class="picker-ctx-item" data-action="new-window">Open in New Window</div>
    <div class="picker-ctx-item" data-action="rename">Rename</div>
    <div class="picker-ctx-separator"></div>
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

  menu.querySelector('[data-action="new-window"]').addEventListener("click", async (e) => {
    e.stopPropagation();
    closeRowContextMenu();
    // If it's already open elsewhere, just focus that window — don't spawn a duplicate.
    const focused = await focusProjectWindow(project.path);
    if (!focused) await openProjectWindow(project.path);
  });
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

// Substring match on name + pretty path. Returns the surviving projects in
// their original (recency) order.
function filterProjects(projects, query) {
  const q = query.trim().toLowerCase();
  if (!q) return projects;
  return projects.filter((p) => {
    const hay = (p.name + " " + prettyPath(p.path)).toLowerCase();
    return q.split(/\s+/).every((tok) => hay.includes(tok));
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
          <div class="picker-subtle">Terminal-first workspace for developers</div>
          <div class="picker-blurb">
            A project is a directory. Open one and every terminal, the file tree, git panel, and Cmd+P all lock to it — so CLI agents never get a surprise <code>cd</code>.
          </div>
          <div class="picker-cta-wrap">
            <button id="picker-open-btn" class="picker-open-btn">+ Open Project</button>
            <div class="picker-footer-hint">⌘⇧N opens another window anytime</div>
          </div>
        </div>
      `;
      pickerRoot.appendChild(shell);
      const openBtn = document.getElementById("picker-open-btn");
      openBtn?.addEventListener("click", async () => {
        const project = await pickAndAdd();
        if (project) onOpen(project);
      });
      return;
    }

    // --- Header: wordmark + live search + open button ---------------------
    const head = document.createElement("div");
    head.className = "picker-topbar";
    head.innerHTML = `
      <div class="picker-brand">Launchpad</div>
      <div class="picker-search-wrap">
        <span class="picker-search-icon">⌕</span>
        <input class="picker-search" type="text" spellcheck="false"
               placeholder="Search ${projects.length} project${projects.length === 1 ? "" : "s"}…" />
      </div>
      <button id="picker-open-btn" class="picker-open-btn picker-open-btn-secondary">+ Open</button>
    `;
    shell.appendChild(head);

    const list = document.createElement("div");
    list.className = "picker-list";
    shell.appendChild(list);

    pickerRoot.appendChild(shell);

    const searchInput = head.querySelector(".picker-search");

    // --- Filtered render + keyboard-navigable selection -------------------
    let filtered = projects;
    let selected = 0;
    const cardCache = new Map(); // path -> get_project_card result

    function setSelected(i) {
      const rows = list.querySelectorAll(".picker-row");
      if (!rows.length) return;
      selected = Math.max(0, Math.min(i, rows.length - 1));
      rows.forEach((r, idx) => r.classList.toggle("is-selected", idx === selected));
      rows[selected].scrollIntoView({ block: "nearest" });
    }

    function renderList() {
      filtered = filterProjects(projects, searchInput.value);
      selected = 0;
      list.replaceChildren();

      if (filtered.length === 0) {
        const empty = document.createElement("div");
        empty.className = "picker-empty";
        empty.textContent = "No projects match your search.";
        list.appendChild(empty);
        return;
      }

      filtered.forEach((p, i) => {
        const row = document.createElement("div");
        row.className = "picker-row" + (i === 0 ? " is-selected" : "");
        row.tabIndex = -1;
        row.style.setProperty("--tile-accent", `var(${accentFor(p.path)})`);
        row.style.animationDelay = `${Math.min(i, 12) * 22}ms`;
        row.dataset.path = p.path;
        row.innerHTML = `
          <div class="picker-tile">${escapeHtml(initialsFor(p.name))}</div>
          <div class="picker-row-body">
            <div class="picker-row-head">
              <span class="picker-row-name">${escapeHtml(p.name)}</span>
              <span class="picker-row-branch" hidden></span>
            </div>
            <div class="picker-row-meta">${escapeHtml(prettyPath(p.path))}</div>
            <div class="picker-row-commit picker-row-commit-loading">loading…</div>
          </div>
          <div class="picker-row-time">${escapeHtml(formatRelativeTime(p.lastOpened))}</div>
          <button class="picker-row-menu-btn" title="Project options" aria-label="Project options">⋯</button>
        `;
        row.addEventListener("click", (e) => {
          if (e.target.closest(".picker-row-menu-btn") || e.target.matches(".picker-row-name-input")) return;
          onOpen(p);
        });
        row.addEventListener("mousemove", () => setSelected(i));
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

        // Fill folder-exists + branch + last-commit. Cached so re-filtering
        // (which rebuilds rows) doesn't re-hit the backend. The result is
        // applied to whatever row currently represents this path — rows are
        // transient across keystrokes, so we re-query by path, never trust the
        // captured `row` after an await.
        const cached = cardCache.get(p.path);
        if (cached) {
          applyCard(cached, row);
        } else {
          invoke("get_project_card", { path: p.path })
            .then((info) => {
              if (!info) return;
              cardCache.set(p.path, info);
              const live = list.querySelector(`.picker-row[data-path="${CSS.escape(p.path)}"]`);
              if (live) applyCard(info, live);
            })
            .catch(() => {});
        }
      });
    }

    function applyCard(info, row) {
      const commitEl = row.querySelector(".picker-row-commit");
      const branchEl = row.querySelector(".picker-row-branch");
      if (!commitEl) return;
      commitEl.classList.remove("picker-row-commit-loading");

      if (!info.exists) {
        row.classList.add("is-missing");
        commitEl.textContent = "⚠ folder no longer on disk";
        commitEl.classList.add("is-warning");
        return;
      }
      if (info.branch && branchEl) {
        branchEl.textContent = info.branch;
        branchEl.hidden = false;
      }
      if (info.last_commit) {
        const lc = info.last_commit;
        const when = formatRelativeTime(new Date(lc.timestamp * 1000).toISOString());
        // Pin the author (never truncated) and let only the message ellipsize,
        // so a long subject can't hide who made it. No relative time here — the
        // commit date lives in the title; the row already shows a date.
        const author = document.createElement("span");
        author.className = "picker-commit-author";
        author.textContent = lc.author;
        const msg = document.createElement("span");
        msg.className = "picker-commit-msg";
        msg.textContent = lc.message;
        commitEl.replaceChildren(author, msg);
        commitEl.title = `${lc.oid} · ${lc.message} · ${when}`;
      } else if (info.is_repo) {
        commitEl.textContent = "no commits yet";
        commitEl.classList.add("is-dim");
      } else {
        commitEl.textContent = "not a git repository";
        commitEl.classList.add("is-dim");
      }
    }

    searchInput.addEventListener("input", renderList);
    searchInput.addEventListener("keydown", (e) => {
      if (e.key === "ArrowDown") {
        e.preventDefault();
        setSelected(selected + 1);
      } else if (e.key === "ArrowUp") {
        e.preventDefault();
        setSelected(selected - 1);
      } else if (e.key === "Enter") {
        e.preventDefault();
        if (filtered[selected]) onOpen(filtered[selected]);
      } else if (e.key === "Escape") {
        if (searchInput.value) {
          e.preventDefault();
          searchInput.value = "";
          renderList();
        }
      }
    });

    head.querySelector("#picker-open-btn").addEventListener("click", async () => {
      const project = await pickAndAdd();
      if (project) onOpen(project);
    });

    renderList();
    // Auto-focus so you can just start typing to find a project.
    searchInput.focus();
  }

  await rerender();
}

export function hidePicker() {
  pickerRoot.hidden = true;
  pickerRoot.innerHTML = "";
}
