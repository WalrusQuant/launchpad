// Small DOM/string helpers shared across tab renderers (diff, rebase, …).

// Escape for use as HTML text content.
export function escapeText(s) {
  return String(s)
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;");
}

// Escape for use inside a double-quoted HTML attribute.
export function escapeAttr(s) {
  return escapeText(s).replace(/"/g, "&quot;");
}
