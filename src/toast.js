// Floating toast for app-level errors (file-open failures, rename/delete
// errors, etc.) that aren't bound to a specific panel. Lazy-creates the
// container on first use; auto-dismisses after 4s. type is "error" | "info".
// (The git panel keeps its own panel-scoped showGitFeedback.)
export function showToast(message, type = "error") {
  let container = document.getElementById("app-toast-container");
  if (!container) {
    container = document.createElement("div");
    container.id = "app-toast-container";
    container.className = "app-toast-container";
    document.body.appendChild(container);
  }
  const el = document.createElement("div");
  el.className = `app-toast app-toast-${type}`;
  el.textContent = message;
  container.appendChild(el);
  setTimeout(() => {
    el.classList.add("app-toast-leaving");
    setTimeout(() => el.remove(), 200);
  }, 4000);
}
