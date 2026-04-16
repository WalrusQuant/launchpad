const { invoke } = window.__TAURI__.core;
import { pushEscape, popEscape } from "./main.js";

let visible = false;
let currentRoot = "";
let onSelectCallback = null;
let debounceTimer = null;

export function initQuickOpen(getRoot, onSelect) {
  currentRoot = getRoot();
  onSelectCallback = onSelect;

  const overlay = document.getElementById("quick-open");
  const input = document.getElementById("quick-open-input");
  const results = document.getElementById("quick-open-results");

  input.addEventListener("input", () => {
    clearTimeout(debounceTimer);
    debounceTimer = setTimeout(() => search(input.value.trim()), 100);
  });

  input.addEventListener("keydown", (e) => {
    if (e.key === "Escape") {
      e.preventDefault();
      hide();
    } else if (e.key === "Enter") {
      e.preventDefault();
      const active = results.querySelector(".qo-result-active");
      if (active) selectResult(active.dataset.path);
    } else if (e.key === "ArrowDown") {
      e.preventDefault();
      moveSelection(1);
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      moveSelection(-1);
    }
    e.stopPropagation();
  });

  overlay.addEventListener("click", (e) => {
    if (e.target === overlay) hide();
  });
}

export function show(root) {
  if (root) currentRoot = root;
  visible = true;
  const overlay = document.getElementById("quick-open");
  const input = document.getElementById("quick-open-input");
  const results = document.getElementById("quick-open-results");

  overlay.classList.add("visible");
  input.value = "";
  results.innerHTML = "";
  input.focus();
  pushEscape(hide);
}

export function hide() {
  visible = false;
  document.getElementById("quick-open").classList.remove("visible");
  popEscape(hide);
}

export function updateRoot(root) {
  currentRoot = root;
}

async function search(query) {
  const results = document.getElementById("quick-open-results");
  if (!query) {
    results.innerHTML = '<div class="qo-hint">Type to search files...</div>';
    return;
  }

  results.innerHTML = '<div class="qo-hint">Searching\u2026</div>';

  try {
    const files = await invoke("search_files", {
      root: currentRoot,
      query,
      maxResults: 20,
    });

    if (files.length === 0) {
      results.innerHTML = '<div class="qo-hint">No files found</div>';
      return;
    }

    results.innerHTML = files
      .map((f, i) => {
        const name = f.split("/").pop();
        const dir = f.split("/").slice(0, -1).join("/");
        return `<div class="qo-result ${i === 0 ? "qo-result-active" : ""}" data-path="${escapeAttr(f)}">
          <span class="qo-result-name">${escapeHtml(name)}</span>
          <span class="qo-result-dir">${escapeHtml(dir)}</span>
        </div>`;
      })
      .join("");

    // Click handlers
    results.querySelectorAll(".qo-result").forEach((el) => {
      el.addEventListener("click", () => selectResult(el.dataset.path));
    });
  } catch (err) {
    results.innerHTML = `<div class="qo-hint">Search error</div>`;
  }
}

function selectResult(relativePath) {
  const fullPath = currentRoot + "/" + relativePath;
  if (onSelectCallback) onSelectCallback(fullPath, relativePath);
  hide();
}

function moveSelection(direction) {
  const results = document.getElementById("quick-open-results");
  const items = [...results.querySelectorAll(".qo-result")];
  if (items.length === 0) return;

  const activeIdx = items.findIndex((el) => el.classList.contains("qo-result-active"));
  const newIdx = Math.min(Math.max(activeIdx + direction, 0), items.length - 1);

  items.forEach((el) => el.classList.remove("qo-result-active"));
  items[newIdx].classList.add("qo-result-active");
  items[newIdx].scrollIntoView({ block: "nearest" });
}

function escapeHtml(str) {
  return str.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
}

function escapeAttr(str) {
  return str.replace(/"/g, "&quot;");
}
