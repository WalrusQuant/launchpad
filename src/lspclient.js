// Frontend half of the language-server integration. The Rust side (lsp.rs)
// owns the server processes and message framing; here we run
// @codemirror/lsp-client and bridge its Transport over Tauri IPC:
//
//   client.send(msg)  --invoke lsp_send-->  Rust  -->  server stdin
//   server stdout  -->  Rust  --emit lsp-message-->  transport subscribers
//
// Foundation scope: diagnostics only (the empty lint gutter finally gets fed).
// Hover / completion / go-to-definition are deliberately NOT wired yet — they're
// the interactive features that need live verification.
import { LSPClient, serverDiagnostics } from "@codemirror/lsp-client";

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

// File extension → logical language id. Must match server_command() in lsp.rs.
// Only the proven typescript-language-server languages for now.
const LANG_BY_EXT = {
  ts: "typescript",
  tsx: "typescript",
  mts: "typescript",
  cts: "typescript",
  js: "javascript",
  jsx: "javascript",
  mjs: "javascript",
  cjs: "javascript",
};

export function lspLanguageForFile(fileName) {
  const ext = fileName.split(".").pop().toLowerCase();
  return LANG_BY_EXT[ext] || null;
}

// One client per "{language}:{projectPath}", reused across files. A cached
// `null` means "we tried and there's no server" — don't hammer lsp_start on
// every file open.
const clients = new Map();

function tauriTransport(serverId) {
  const handlers = new Set();
  // Long-lived: the listener lives as long as the client (app session).
  listen("lsp-message", (e) => {
    if (e.payload && e.payload.serverId === serverId) {
      for (const h of handlers) h(e.payload.message);
    }
  });
  return {
    send(message) {
      invoke("lsp_send", { serverId, message }).catch((err) =>
        console.warn("[lsp] send failed:", err)
      );
    },
    subscribe(handler) {
      handlers.add(handler);
    },
    unsubscribe(handler) {
      handlers.delete(handler);
    },
  };
}

async function getClient(language, projectPath) {
  const key = `${language}:${projectPath}`;
  if (clients.has(key)) return clients.get(key);

  let serverId;
  try {
    serverId = await invoke("lsp_start", { projectPath, language });
  } catch (err) {
    // No server on PATH, or spawn failed — degrade silently and remember the
    // miss so we don't retry on every open.
    console.warn(`[lsp] ${language} server unavailable:`, err);
    clients.set(key, null);
    return null;
  }

  const client = new LSPClient({ extensions: [serverDiagnostics()] });
  client.connect(tauriTransport(serverId));
  const entry = { client, serverId };
  clients.set(key, entry);
  return entry;
}

// Build the editor extension that syncs `filePath` with its language server and
// feeds diagnostics into the lint gutter. Resolves to `[]` when LSP is
// unavailable for the file (no server, no project, or start failed), so callers
// can always spread the result into a config.
export async function lspExtensionForFile(filePath, fileName, projectPath) {
  const language = lspLanguageForFile(fileName);
  if (!language || !projectPath) return [];
  const entry = await getClient(language, projectPath);
  if (!entry) return [];
  const uri = `file://${filePath}`;
  return entry.client.plugin(uri, language);
}

// Stop every running server and clear the cache. Called on window teardown so
// project switches (which reload the webview) don't orphan server processes in
// the Rust-side registry, which outlives the webview.
export async function shutdownAllLsp() {
  const ids = [];
  for (const entry of clients.values()) {
    if (entry && entry.serverId) ids.push(entry.serverId);
  }
  clients.clear();
  await Promise.all(ids.map((serverId) => invoke("lsp_stop", { serverId }).catch(() => {})));
}
