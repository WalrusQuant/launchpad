let current = "auto";
const mql = window.matchMedia("(prefers-color-scheme: light)");
const listeners = new Set();

function resolve(pref) {
  if (pref === "light") return "light";
  if (pref === "dark") return "dark";
  return mql.matches ? "light" : "dark";
}

function apply(pref) {
  current = pref;
  const resolved = resolve(pref);
  document.documentElement.setAttribute("data-theme", resolved);
  for (const cb of listeners) cb(resolved);
}

export function initTheme(pref) {
  apply(pref || "auto");
  mql.addEventListener("change", () => {
    if (current === "auto") apply("auto");
  });
}

export function setTheme(pref) {
  apply(pref);
}

export function getResolvedTheme() {
  return resolve(current);
}

export function onThemeChange(cb) {
  listeners.add(cb);
  return () => listeners.delete(cb);
}
