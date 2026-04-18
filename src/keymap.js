import { getSettings, saveSetting } from "./settings.js";

// Action registry — id → { label, default, notes? }
// These drive both the Settings UI and the matches() lookup at keydown time.
export const ACTIONS = [
  { id: "newWindow",           label: "New window",              default: "Cmd+Shift+N" },
  { id: "newTab",              label: "New terminal tab",        default: "Cmd+T" },
  { id: "closeTab",            label: "Close tab",               default: "Cmd+W" },
  { id: "clearTerminal",       label: "Clear terminal",          default: "Cmd+K" },
  { id: "splitPane",           label: "Split/unsplit pane",      default: "Cmd+D" },
  { id: "splitWorkspace",      label: "Split/unsplit workspace", default: "Cmd+\\" },
  { id: "focusLeftGroup",      label: "Focus left group",        default: "Cmd+Alt+ArrowLeft" },
  { id: "focusRightGroup",     label: "Focus right group",       default: "Cmd+Alt+ArrowRight" },
  { id: "moveTabToOtherGroup", label: "Move tab to other group", default: "Cmd+Shift+M" },
  { id: "gitPanel",            label: "Toggle git panel",        default: "Cmd+G" },
  { id: "quickOpen",           label: "Quick open file",         default: "Cmd+P" },
  { id: "projectFind",         label: "Find in project",         default: "Cmd+Shift+F" },
  { id: "findInEditor",        label: "Find in editor",          default: "Cmd+F" },
  { id: "findReplace",         label: "Find and replace",        default: "Cmd+H" },
  { id: "saveFile",            label: "Save file",               default: "Cmd+S" },
  { id: "openSettings",        label: "Open settings",           default: "Cmd+," },
  { id: "debugDump",           label: "Dump debug capture",      default: "Cmd+Shift+X" },
  { id: "shortcutsModal",      label: "Show shortcuts",          default: "Cmd+/" },
];

const DEFAULTS = Object.fromEntries(ACTIONS.map((a) => [a.id, a.default]));

function getBindings() {
  const s = getSettings();
  return { ...DEFAULTS, ...(s.keybindings || {}) };
}

// Parse a chord string like "Cmd+Shift+T" into a structured match
function parseChord(chord) {
  if (!chord) return null;
  const parts = chord.split("+").map((p) => p.trim());
  const key = parts.pop();
  const mods = new Set(parts.map((p) => p.toLowerCase()));
  return {
    cmd: mods.has("cmd") || mods.has("meta"),
    shift: mods.has("shift"),
    alt: mods.has("alt") || mods.has("option"),
    ctrl: mods.has("ctrl"),
    key: key,
  };
}

function keyMatches(eventKey, chordKey) {
  if (!chordKey) return false;
  // Direct key match (case-insensitive for letters)
  if (eventKey.toLowerCase() === chordKey.toLowerCase()) return true;
  // Special names like ArrowLeft / ArrowRight already match exactly above
  return false;
}

export function matches(event, actionId) {
  const bindings = getBindings();
  const chord = parseChord(bindings[actionId]);
  if (!chord) return false;
  if (!!event.metaKey !== chord.cmd) return false;
  if (!!event.shiftKey !== chord.shift) return false;
  if (!!event.altKey !== chord.alt) return false;
  if (!!event.ctrlKey !== chord.ctrl) return false;
  return keyMatches(event.key, chord.key);
}

export function getBinding(actionId) {
  return getBindings()[actionId] || "";
}

export async function setBinding(actionId, chord) {
  const s = getSettings();
  const next = { ...(s.keybindings || {}) };
  if (!chord || chord === DEFAULTS[actionId]) {
    delete next[actionId]; // fall back to default
  } else {
    next[actionId] = chord;
  }
  await saveSetting("keybindings", next);
}

export function getDefault(actionId) {
  return DEFAULTS[actionId] || "";
}

// Build a chord string from a KeyboardEvent — used by the Settings capture UI.
export function chordFromEvent(e) {
  const parts = [];
  if (e.metaKey) parts.push("Cmd");
  if (e.ctrlKey) parts.push("Ctrl");
  if (e.altKey) parts.push("Alt");
  if (e.shiftKey) parts.push("Shift");
  // Reject modifier-only presses
  if (["Meta", "Shift", "Alt", "Control"].includes(e.key)) return null;
  parts.push(e.key.length === 1 ? e.key.toUpperCase() : e.key);
  return parts.join("+");
}
