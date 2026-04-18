const { invoke } = window.__TAURI__.core;

let getProjectRoot = () => null;
let openFile = null;
let searchTimer = null;
let caseSensitive = false;
let isRegex = false;
let searchToken = 0; // cancels stale in-flight searches

const DEBOUNCE_MS = 200;

export function initProjectSearch(projectRootFn, openFileCb) {
  getProjectRoot = projectRootFn;
  openFile = openFileCb;

  const overlay = document.getElementById("project-search");
  const input = document.getElementById("project-search-input");
  const caseBtn = document.getElementById("ps-case");
  const regexBtn = document.getElementById("ps-regex");

  overlay.addEventListener("click", (e) => {
    if (e.target === overlay) hide();
  });

  input.addEventListener("keydown", (e) => {
    if (e.key === "Escape") {
      hide();
    } else if (e.key === "Enter") {
      e.preventDefault();
      runSearch();
    }
  });

  input.addEventListener("input", () => schedule());

  caseBtn.addEventListener("click", () => {
    caseSensitive = !caseSensitive;
    caseBtn.setAttribute("aria-pressed", String(caseSensitive));
    caseBtn.classList.toggle("active", caseSensitive);
    runSearch();
  });

  regexBtn.addEventListener("click", () => {
    isRegex = !isRegex;
    regexBtn.setAttribute("aria-pressed", String(isRegex));
    regexBtn.classList.toggle("active", isRegex);
    runSearch();
  });
}

export function showProjectSearch() {
  const overlay = document.getElementById("project-search");
  const input = document.getElementById("project-search-input");
  overlay.classList.add("visible");
  input.focus();
  input.select();
}

function hide() {
  document.getElementById("project-search").classList.remove("visible");
}

function schedule() {
  clearTimeout(searchTimer);
  searchTimer = setTimeout(runSearch, DEBOUNCE_MS);
}

async function runSearch() {
  const input = document.getElementById("project-search-input");
  const statusEl = document.getElementById("project-search-status");
  const resultsEl = document.getElementById("project-search-results");
  const query = input.value;
  const root = getProjectRoot();

  if (!root) {
    statusEl.textContent = "No project open.";
    resultsEl.innerHTML = "";
    return;
  }
  if (!query) {
    statusEl.textContent = "Type to search…";
    resultsEl.innerHTML = "";
    return;
  }

  const myToken = ++searchToken;
  statusEl.textContent = "Searching…";

  try {
    const hits = await invoke("search_in_files", {
      root,
      query,
      caseSensitive,
      isRegex,
      maxResults: 500,
    });
    if (myToken !== searchToken) return; // stale
    render(hits, statusEl, resultsEl);
  } catch (err) {
    if (myToken !== searchToken) return;
    statusEl.textContent = String(err);
    resultsEl.innerHTML = "";
  }
}

function render(hits, statusEl, resultsEl) {
  if (!hits.length) {
    statusEl.textContent = "No matches.";
    resultsEl.innerHTML = "";
    return;
  }

  // Group by file
  const byFile = new Map();
  for (const h of hits) {
    if (!byFile.has(h.file)) byFile.set(h.file, []);
    byFile.get(h.file).push(h);
  }
  const total = hits.length;
  const files = byFile.size;
  statusEl.textContent = `${total} match${total === 1 ? "" : "es"} in ${files} file${files === 1 ? "" : "s"}`;

  const frag = document.createDocumentFragment();
  for (const [file, fileHits] of byFile) {
    const group = document.createElement("div");
    group.className = "ps-file-group";

    const header = document.createElement("div");
    header.className = "ps-file-header";
    header.textContent = `${file} · ${fileHits.length}`;
    group.appendChild(header);

    for (const hit of fileHits) {
      const row = document.createElement("div");
      row.className = "ps-hit";
      row.tabIndex = 0;

      const lineNo = document.createElement("span");
      lineNo.className = "ps-lineno";
      lineNo.textContent = String(hit.line);

      const line = document.createElement("span");
      line.className = "ps-line";

      const content = hit.line_content;
      const colStart = hit.column - 1;
      const colEnd = colStart + hit.match_length;
      // Build highlighted line by char positions
      const chars = [...content];
      const pre = chars.slice(0, colStart).join("");
      const mid = chars.slice(colStart, colEnd).join("");
      const post = chars.slice(colEnd).join("");
      line.appendChild(document.createTextNode(pre));
      const mark = document.createElement("mark");
      mark.className = "ps-match";
      mark.textContent = mid;
      line.appendChild(mark);
      line.appendChild(document.createTextNode(post));

      row.appendChild(lineNo);
      row.appendChild(line);

      const absPath = `${getProjectRoot()}/${hit.file}`;
      const jump = () => {
        hide();
        if (openFile) openFile(absPath, { line: hit.line, column: hit.column });
      };
      row.addEventListener("click", jump);
      row.addEventListener("keydown", (e) => {
        if (e.key === "Enter") jump();
      });

      group.appendChild(row);
    }

    frag.appendChild(group);
  }

  resultsEl.innerHTML = "";
  resultsEl.appendChild(frag);
}
