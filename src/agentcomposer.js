// Agent chat composer — multiline textarea with send / cancel button.
// During an active turn the [Send] button switches to [Cancel].
// Typing `/` at the start of a line opens a slash-command picker backed
// by the agent's skill catalog (skills/list).

import { invoke } from "@tauri-apps/api/core";

let cachedSkills = null;

async function fetchSkills() {
  if (cachedSkills) return cachedSkills;
  try {
    const resp = await invoke("agent_skills_list");
    const skills = resp?.result?.skills || resp?.result || [];
    cachedSkills = Array.isArray(skills) ? skills : [];
  } catch (_) {
    cachedSkills = [];
  }
  return cachedSkills;
}

// Public so settings can refresh after a runtime reload.
export function clearSkillCache() {
  cachedSkills = null;
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

    if (!matches.length) {
      closeSlashPopup();
      return;
    }

    slashItems = matches;
    slashIndex = Math.min(slashIndex, matches.length - 1);
    if (slashIndex < 0) slashIndex = 0;
    slashStart = start;
    renderSlashPopup();
  }

  function renderSlashPopup() {
    if (!slashPopup) {
      slashPopup = document.createElement("div");
      slashPopup.className = "agent-slash-popup";
      wrap.appendChild(slashPopup);
    }
    slashPopup.replaceChildren();
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

  function applySlashSelection() {
    if (slashStart < 0 || !slashItems[slashIndex]) {
      closeSlashPopup();
      return;
    }
    const skill = slashItems[slashIndex];
    const pos = textarea.selectionStart;
    const before = textarea.value.slice(0, slashStart);
    const after = textarea.value.slice(pos);
    const insert = "/" + (skill.name || skill.id || "");
    textarea.value = before + insert + " " + after;
    textarea.selectionStart = textarea.selectionEnd = (before + insert + " ").length;
    autosize();
    closeSlashPopup();
    textarea.focus();
  }

  textarea.addEventListener("keydown", (e) => {
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
      if (e.key === "Escape") {
        e.preventDefault();
        closeSlashPopup();
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
  // After every input event, re-evaluate whether the popup should be
  // open and what items it should show. Runs after keydown so the
  // textarea value reflects the just-typed character.
  textarea.addEventListener("input", () => { maybeOpenSlashPopup(); });
  textarea.addEventListener("blur", () => {
    // Defer so a click on a popup row still fires its mousedown handler.
    setTimeout(closeSlashPopup, 100);
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
    textarea.value = "";
    autosize();
    onSend?.(text);
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
  };
}
