// Agent chat composer — multiline textarea with send / cancel button.
// During an active turn the [Send] button switches to [Cancel].
// Typing `/` at the start of a line opens a slash-command picker backed
// by the agent's skill catalog (skills/list).
// Typing `@` at a word boundary opens a file-mention picker backed by
// search_files (the same index Cmd+P uses), scoped to the active project.

import { invoke } from "@tauri-apps/api/core";
import { getActiveProject } from "./projects.js";

// Cache key includes the project path so per-workspace skills don't leak
// between projects in the same window.
let cachedSkills = null;
let cachedSkillsKey = null;

async function fetchSkills() {
  const projectPath = getActiveProject()?.path || null;
  const key = projectPath || "__no_project__";
  if (cachedSkills && cachedSkillsKey === key) return cachedSkills;
  try {
    const resp = await invoke("agent_skills_list", { projectPath });
    const skills = resp?.result?.skills || resp?.result || [];
    cachedSkills = Array.isArray(skills) ? skills : [];
  } catch (_) {
    cachedSkills = [];
  }
  cachedSkillsKey = key;
  return cachedSkills;
}

// Public so settings can refresh after a runtime reload.
export function clearSkillCache() {
  cachedSkills = null;
  cachedSkillsKey = null;
}

export function createComposer({ onSend, onCancel }) {
  const wrap = document.createElement("div");
  wrap.className = "agent-composer";

  const textarea = document.createElement("textarea");
  textarea.className = "agent-composer-input";
  textarea.placeholder = "Send a message…  (Enter to send, Shift+Enter for newline)";
  textarea.rows = 1;

  const button = document.createElement("button");
  button.className = "agent-composer-btn agent-composer-send";
  button.textContent = "Send";

  wrap.appendChild(textarea);
  wrap.appendChild(button);

  let turnActive = false;

  const autosize = () => {
    textarea.style.height = "auto";
    const max = 12 * 18; // ~12 lines worth
    textarea.style.height = Math.min(textarea.scrollHeight, max) + "px";
  };
  textarea.addEventListener("input", autosize);

  // Slash-command popup state. Owned by this composer instance so multiple
  // agent tabs don't fight over a single global popup.
  let slashPopup = null;
  let slashItems = [];
  let slashIndex = 0;
  let slashStart = -1; // textarea index of the leading "/"

  // @-mention popup state. Tracks file-search results. Mentions selected via
  // the picker accumulate in `mentionedPaths` (absolute), and on submit are
  // sent alongside the text as structured InputItem::Mention items so the
  // agent resolves them deterministically (the literal "@<rel-path>" stays
  // in the visible text for the user's own reading).
  let atPopup = null;
  let atItems = []; // each: { rel: string, abs: string }
  let atIndex = 0;
  let atStart = -1; // textarea index of the leading "@"
  let atFetchToken = 0;
  let atDebounceTimer = null;
  const mentionedPaths = new Set(); // absolute paths
  // Skill ids the user picked from the slash menu. Sent as structured
  // InputItem::Skill items so the runtime resolves SKILL.md content into
  // the turn — without this the literal "/foo" string is just user text.
  const insertedSkills = new Set();

  function isSlashContext() {
    // `/` is a slash command only when it's at the very start of input or
    // immediately after a newline. Mid-word slashes (e.g. paths) are left
    // alone.
    const pos = textarea.selectionStart;
    const prefix = textarea.value.slice(0, pos);
    const idx = prefix.lastIndexOf("\n");
    const lineStart = idx + 1;
    if (lineStart > prefix.length) return -1;
    const lineSlice = prefix.slice(lineStart);
    return lineSlice.startsWith("/") ? lineStart : -1;
  }

  function closeSlashPopup() {
    slashPopup?.remove();
    slashPopup = null;
    slashItems = [];
    slashStart = -1;
  }

  async function maybeOpenSlashPopup() {
    const start = isSlashContext();
    if (start < 0) {
      closeSlashPopup();
      return;
    }
    const pos = textarea.selectionStart;
    const filterRaw = textarea.value.slice(start + 1, pos).toLowerCase();

    const skills = await fetchSkills();
    const matches = skills.filter((s) => {
      const name = (s.name || s.id || "").toLowerCase();
      const desc = (s.description || s.summary || "").toLowerCase();
      return !filterRaw || name.includes(filterRaw) || desc.includes(filterRaw);
    });

    slashItems = matches;
    slashIndex = matches.length ? Math.min(Math.max(slashIndex, 0), matches.length - 1) : 0;
    slashStart = start;
    renderSlashPopup({ totalSkills: skills.length, filterRaw });
  }

  function renderSlashPopup(meta = {}) {
    if (!slashPopup) {
      slashPopup = document.createElement("div");
      slashPopup.className = "agent-slash-popup";
      wrap.appendChild(slashPopup);
    }
    slashPopup.replaceChildren();

    if (!slashItems.length) {
      // Empty-state hint: distinct row, non-selectable, copy depends on
      // whether the catalog is empty altogether vs. just no-match-for-filter.
      const empty = document.createElement("div");
      empty.className = "agent-slash-empty";
      if (meta.totalSkills === 0) {
        empty.innerHTML = `
          <div class="agent-slash-empty-title">No skills installed</div>
          <div class="agent-slash-empty-body">
            Drop a <code>SKILL.md</code> at
            <code>~/.lpagent/skills/&lt;name&gt;/SKILL.md</code> (user-wide) or
            <code>&lt;project&gt;/skills/&lt;name&gt;/SKILL.md</code> (per-project),
            then press <kbd>/</kbd> again.
          </div>
        `;
      } else {
        empty.innerHTML = `
          <div class="agent-slash-empty-title">No matches for “${escapeHtml(meta.filterRaw || "")}”</div>
          <div class="agent-slash-empty-body">${meta.totalSkills} skill${meta.totalSkills === 1 ? "" : "s"} installed.</div>
        `;
      }
      slashPopup.appendChild(empty);
      return;
    }

    slashItems.forEach((skill, i) => {
      const row = document.createElement("div");
      row.className = "agent-slash-item" + (i === slashIndex ? " agent-slash-item-active" : "");
      const name = document.createElement("div");
      name.className = "agent-slash-name";
      name.textContent = "/" + (skill.name || skill.id || "?");
      const desc = document.createElement("div");
      desc.className = "agent-slash-desc";
      desc.textContent = skill.description || skill.summary || "";
      row.appendChild(name);
      if (desc.textContent) row.appendChild(desc);
      row.addEventListener("mousedown", (e) => {
        e.preventDefault();
        slashIndex = i;
        applySlashSelection();
      });
      slashPopup.appendChild(row);
    });
  }

  function escapeHtml(s) {
    return String(s).replace(/[&<>"']/g, (c) =>
      ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c])
    );
  }

  function applySlashSelection() {
    if (slashStart < 0 || !slashItems[slashIndex]) {
      closeSlashPopup();
      return;
    }
    const skill = slashItems[slashIndex];
    const pos = textarea.selectionStart;
    const before = textarea.value.slice(0, slashStart);
    const after = textarea.value.slice(pos);
    const skillName = skill.name || skill.id || "";
    const insert = "/" + skillName;
    textarea.value = before + insert + " " + after;
    textarea.selectionStart = textarea.selectionEnd = (before + insert + " ").length;
    autosize();
    // Skill id (preferred) for the structured InputItem::Skill on submit.
    // The runtime's catalog matches SkillId to record id, which equals the
    // frontmatter `name` field — both fields work.
    if (skill.id || skill.name) insertedSkills.add(skill.id || skill.name);
    closeSlashPopup();
    textarea.focus();
  }

  // ─── @-mention popup ─────────────────────────────────────────────────────

  function isAtContext() {
    // `@` triggers a file-mention popup when it follows whitespace, a newline,
    // or sits at the very start of the textarea. Mid-word `@` (email-like)
    // is left alone. Returns the textarea index of the `@`, or -1.
    const pos = textarea.selectionStart;
    const prefix = textarea.value.slice(0, pos);
    const idx = prefix.lastIndexOf("@");
    if (idx < 0) return -1;
    if (idx > 0) {
      const before = prefix[idx - 1];
      if (before !== " " && before !== "\n" && before !== "\t") return -1;
    }
    // No whitespace allowed inside the @-token (so wrapping back to a previous
    // @ doesn't accidentally fire when the cursor sits later in the line).
    const between = prefix.slice(idx + 1);
    if (/\s/.test(between)) return -1;
    return idx;
  }

  function closeAtPopup() {
    if (atDebounceTimer) {
      clearTimeout(atDebounceTimer);
      atDebounceTimer = null;
    }
    atPopup?.remove();
    atPopup = null;
    atItems = [];
    atStart = -1;
  }

  async function maybeOpenAtPopup() {
    const start = isAtContext();
    if (start < 0) {
      closeAtPopup();
      return;
    }
    const project = getActiveProject();
    if (!project?.path) {
      // No project = no file index. Render a hint row.
      atStart = start;
      atItems = [];
      renderAtPopup({ noProject: true });
      return;
    }
    const pos = textarea.selectionStart;
    const query = textarea.value.slice(start + 1, pos);

    // Debounce search_files — typing fast shouldn't fire dozens of RPCs.
    if (atDebounceTimer) clearTimeout(atDebounceTimer);
    atDebounceTimer = setTimeout(async () => {
      atDebounceTimer = null;
      const token = ++atFetchToken;
      let hits = [];
      try {
        // search_files returns relative paths from the root, sorted by length.
        hits = await invoke("search_files", {
          root: project.path,
          query,
          maxResults: 30,
        });
      } catch (_) {
        hits = [];
      }
      // A newer keystroke fired while we were awaiting — discard stale results.
      if (token !== atFetchToken) return;
      // Project switched mid-flight — results are for the wrong tree.
      if (getActiveProject()?.path !== project.path) {
        closeAtPopup();
        return;
      }
      // Re-validate the trigger context — user might have deleted the `@`.
      if (isAtContext() < 0) {
        closeAtPopup();
        return;
      }
      atItems = (hits || []).map((rel) => ({
        rel,
        abs: joinPath(project.path, rel),
      }));
      atIndex = atItems.length ? Math.min(Math.max(atIndex, 0), atItems.length - 1) : 0;
      atStart = start;
      renderAtPopup({ query });
    }, 120);
  }

  function renderAtPopup(meta = {}) {
    if (!atPopup) {
      atPopup = document.createElement("div");
      atPopup.className = "agent-slash-popup agent-at-popup";
      wrap.appendChild(atPopup);
    }
    atPopup.replaceChildren();

    if (meta.noProject) {
      const empty = document.createElement("div");
      empty.className = "agent-slash-empty";
      empty.innerHTML = `
        <div class="agent-slash-empty-title">No project open</div>
        <div class="agent-slash-empty-body">Open a project to mention files with <kbd>@</kbd>.</div>
      `;
      atPopup.appendChild(empty);
      return;
    }
    if (!atItems.length) {
      const empty = document.createElement("div");
      empty.className = "agent-slash-empty";
      empty.innerHTML = `
        <div class="agent-slash-empty-title">No files match “${escapeHtml(meta.query || "")}”</div>
        <div class="agent-slash-empty-body">Keep typing or press <kbd>Esc</kbd> to dismiss.</div>
      `;
      atPopup.appendChild(empty);
      return;
    }

    atItems.forEach((hit, i) => {
      const row = document.createElement("div");
      row.className = "agent-slash-item" + (i === atIndex ? " agent-slash-item-active" : "");
      const name = document.createElement("div");
      name.className = "agent-slash-name";
      name.textContent = "@" + hit.rel;
      row.appendChild(name);
      row.addEventListener("mousedown", (e) => {
        e.preventDefault();
        atIndex = i;
        applyAtSelection();
      });
      atPopup.appendChild(row);
    });
  }

  function applyAtSelection() {
    if (atStart < 0 || !atItems[atIndex]) {
      closeAtPopup();
      return;
    }
    const hit = atItems[atIndex];
    const pos = textarea.selectionStart;
    const before = textarea.value.slice(0, atStart);
    const after = textarea.value.slice(pos);
    const insert = "@" + hit.rel;
    textarea.value = before + insert + " " + after;
    textarea.selectionStart = textarea.selectionEnd = (before + insert + " ").length;
    autosize();
    mentionedPaths.add(hit.abs);
    closeAtPopup();
    textarea.focus();
  }

  function joinPath(root, rel) {
    if (!root) return rel;
    if (root.endsWith("/")) return root + rel;
    return root + "/" + rel;
  }

  textarea.addEventListener("keydown", (e) => {
    // Escape always dismisses any open popup, including empty-state rows.
    if (e.key === "Escape" && (slashPopup || atPopup)) {
      e.preventDefault();
      closeSlashPopup();
      closeAtPopup();
      return;
    }
    // @-mention popup nav has priority over slash (only one is open at a
    // time in practice, but defensive ordering). Empty-state popup ignores
    // arrows / Enter / Tab so the user can still type freely.
    if (atPopup && atItems.length) {
      if (e.key === "ArrowDown") {
        e.preventDefault();
        atIndex = (atIndex + 1) % atItems.length;
        renderAtPopup();
        return;
      }
      if (e.key === "ArrowUp") {
        e.preventDefault();
        atIndex = (atIndex - 1 + atItems.length) % atItems.length;
        renderAtPopup();
        return;
      }
      if ((e.key === "Enter" && !e.shiftKey) || e.key === "Tab") {
        e.preventDefault();
        applyAtSelection();
        return;
      }
    }
    if (slashPopup && slashItems.length) {
      if (e.key === "ArrowDown") {
        e.preventDefault();
        slashIndex = (slashIndex + 1) % slashItems.length;
        renderSlashPopup();
        return;
      }
      if (e.key === "ArrowUp") {
        e.preventDefault();
        slashIndex = (slashIndex - 1 + slashItems.length) % slashItems.length;
        renderSlashPopup();
        return;
      }
      if (e.key === "Enter" && !e.shiftKey) {
        e.preventDefault();
        applySlashSelection();
        return;
      }
      if (e.key === "Tab") {
        e.preventDefault();
        applySlashSelection();
        return;
      }
    }
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      submit();
    }
  });
  // After every input event, re-evaluate which popup (if any) should be
  // open. Runs after keydown so the textarea value reflects the just-typed
  // character. The two popups are mutually exclusive in practice — the
  // context detectors only return non-(-1) for their own trigger char.
  textarea.addEventListener("input", () => {
    maybeOpenSlashPopup();
    maybeOpenAtPopup();
  });
  textarea.addEventListener("blur", () => {
    // Defer so a click on a popup row still fires its mousedown handler.
    setTimeout(() => { closeSlashPopup(); closeAtPopup(); }, 100);
  });

  button.addEventListener("click", () => {
    if (turnActive) {
      onCancel?.();
    } else {
      submit();
    }
  });

  function submit() {
    const text = textarea.value;
    if (!text.trim()) return;
    // Only forward mentions / skills whose tokens still appear in the final
    // text. A user can pick a file or skill, then delete the inserted token —
    // in that case we shouldn't ship the structured item either.
    const project = getActiveProject();
    const projectPath = project?.path || "";
    const mentions = [];
    for (const abs of mentionedPaths) {
      const rel = projectPath && abs.startsWith(projectPath + "/")
        ? abs.slice(projectPath.length + 1)
        : abs;
      if (text.includes("@" + rel)) mentions.push(abs);
    }
    const skills = [];
    for (const id of insertedSkills) {
      if (text.includes("/" + id)) skills.push(id);
    }
    textarea.value = "";
    mentionedPaths.clear();
    insertedSkills.clear();
    autosize();
    onSend?.(text, mentions, skills);
  }

  return {
    el: wrap,
    focus() {
      textarea.focus();
    },
    setTurnActive(active) {
      turnActive = !!active;
      button.textContent = active ? "Cancel" : "Send";
      button.classList.toggle("agent-composer-cancel", active);
      button.classList.toggle("agent-composer-send", !active);
    },
    setDisabled(disabled, placeholder) {
      textarea.disabled = !!disabled;
      button.disabled = !!disabled;
      wrap.classList.toggle("agent-composer-disabled", !!disabled);
      if (placeholder !== undefined) textarea.placeholder = placeholder;
    },
  };
}
