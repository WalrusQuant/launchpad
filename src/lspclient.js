// Frontend half of the language-server integration. The Rust side (lsp.rs)
// owns the server processes and message framing; here we run
// @codemirror/lsp-client and bridge its Transport over Tauri IPC:
//
//   client.send(msg)  --invoke lsp_send-->  Rust  -->  server stdin
//   server stdout  -->  Rust  --emit lsp-message-->  transport subscribers
//
// Full language support: diagnostics (lint gutter), completion, hover tooltips,
// signature help, and the keymaps for go-to-definition / rename / format /
// find-references — all bundled by languageServerExtensions().
import { LSPClient, languageServerExtensions } from "@codemirror/lsp-client";

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

// File extension → logical language id. Must match server_command() in lsp.rs
// (typescript-language-server, rust-analyzer, pyright).
const LANG_BY_EXT = {
  ts: "typescript",
  tsx: "typescript",
  mts: "typescript",
  cts: "typescript",
  js: "javascript",
  jsx: "javascript",
  mjs: "javascript",
  cjs: "javascript",
  rs: "rust",
  py: "python",
  pyi: "python",
};

export function lspLanguageForFile(fileName) {
  const ext = fileName.split(".").pop().toLowerCase();
  return LANG_BY_EXT[ext] || null;
}

// Build a proper file URI: percent-encode each path segment so paths with
// spaces or other special characters produce a valid URI the server accepts
// (an unencoded space breaks didOpen and every request keyed on the URI).
function fileUri(path) {
  return "file://" + path.split("/").map(encodeURIComponent).join("/");
}

// One client per "{language}:{projectPath}", reused across files. A cached
// `null` means "we tried and there's no server" — don't hammer lsp_start on
// every file open. A non-null entry is { client, serverId, handlers, unlisten }.
const clients = new Map();

// Drop a client: unregister its transport listener, disconnect, and forget it
// so the next open re-spawns. Leaves a cached `null` (binary-missing) in place.
function evictClient(key) {
  const entry = clients.get(key);
  if (!entry) return;
  clients.delete(key);
  if (entry.unlisten) entry.unlisten();
  try {
    entry.client.disconnect();
  } catch (_) {
    /* already gone */
  }
}

// One global listener: when the host reports a server exited (crash or stop),
// evict the matching client so a later open re-spawns instead of reusing a
// dead connection.
let exitListenerReady = false;
function ensureExitListener() {
  if (exitListenerReady) return;
  exitListenerReady = true;
  listen("lsp-exit", (e) => {
    const serverId = e.payload;
    for (const [key, entry] of clients) {
      if (entry && entry.serverId === serverId) {
        evictClient(key);
        break;
      }
    }
  });
}

async function getClient(language, projectPath) {
  ensureExitListener();
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

  const handlers = new Set();
  const transport = {
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
  // Await the subscription so the unlisten handle is captured before we hand the
  // entry out (so evict/shutdown can always clean it up).
  const unlisten = await listen("lsp-message", (e) => {
    if (e.payload && e.payload.serverId === serverId) {
      for (const h of handlers) h(e.payload.message);
    }
  });

  const client = new LSPClient({ extensions: languageServerExtensions() });
  client.connect(transport);
  const entry = { client, serverId, handlers, unlisten };
  clients.set(key, entry);
  return entry;
}

// Build the editor extension that syncs `filePath` with its language server and
// enables the full feature set (diagnostics, completion, hover, signature help,
// go-to-def / rename / format / find-references keymaps). Resolves to `[]` when
// LSP is unavailable for the file (no server, no project, or start failed), so
// callers can always spread the result into a config.
export async function lspExtensionForFile(filePath, fileName, projectPath) {
  const language = lspLanguageForFile(fileName);
  if (!language || !projectPath) return [];
  const entry = await getClient(language, projectPath);
  if (!entry) return [];
  return entry.client.plugin(fileUri(filePath), language);
}

// Request textDocument/documentSymbol for an open file. Returns the raw LSP
// result (DocumentSymbol[] or SymbolInformation[]) or null when LSP is
// unavailable. Only uses an already-connected client — never spawns a server
// just for the outline (the editor wiring starts one on open when enabled).
export async function lspDocumentSymbols(filePath, fileName, projectPath) {
  const language = lspLanguageForFile(fileName);
  if (!language || !projectPath) return null;
  const entry = clients.get(`${language}:${projectPath}`);
  if (!entry || !entry.client) return null;
  try {
    await entry.client.initializing;
    entry.client.sync(); // flush pending edits so positions are current
    const result = await entry.client.request("textDocument/documentSymbol", {
      textDocument: { uri: fileUri(filePath) },
    });
    return result ?? null;
  } catch (err) {
    console.warn("[lsp] documentSymbol failed:", err);
    return null;
  }
}

// Stop every running server and clear the cache. Called on window teardown so
// project switches (which reload the webview) don't orphan server processes in
// the Rust-side registry, which outlives the webview.
export async function shutdownAllLsp() {
  const ids = [];
  for (const [, entry] of clients) {
    if (!entry) continue;
    if (entry.unlisten) entry.unlisten();
    try {
      entry.client.disconnect();
    } catch (_) {
      /* already gone */
    }
    ids.push(entry.serverId);
  }
  clients.clear();
  await Promise.all(ids.map((serverId) => invoke("lsp_stop", { serverId }).catch(() => {})));
}
