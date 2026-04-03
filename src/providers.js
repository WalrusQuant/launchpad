import { loadSettings, saveSetting, getSettings } from "./settings.js";

const PRESETS = [
  {
    name: "Anthropic",
    type: "anthropic",
    baseUrl: "https://api.anthropic.com/v1/messages",
    models: ["claude-sonnet-4-20250514", "claude-opus-4-20250514", "claude-haiku-4-5-20251001"],
  },
  {
    name: "OpenAI",
    type: "openai",
    baseUrl: "https://api.openai.com/v1/chat/completions",
    models: ["gpt-4o", "gpt-4-turbo", "o3", "o4-mini"],
  },
  {
    name: "xAI (Grok)",
    type: "openai",
    baseUrl: "https://api.x.ai/v1/chat/completions",
    models: ["grok-3", "grok-3-mini"],
  },
  {
    name: "Google (Gemini)",
    type: "openai",
    baseUrl: "https://generativelanguage.googleapis.com/v1beta/openai/chat/completions",
    models: ["gemini-2.5-flash", "gemini-2.5-pro"],
  },
  {
    name: "OpenRouter",
    type: "openai",
    baseUrl: "https://openrouter.ai/api/v1/chat/completions",
    models: ["anthropic/claude-sonnet-4", "deepseek/deepseek-chat", "google/gemini-2.5-flash", "meta-llama/llama-4-scout"],
  },
  {
    name: "Custom (OpenAI-compatible)",
    type: "openai",
    baseUrl: "",
    models: [],
  },
];

export function getPresets() {
  return PRESETS;
}

export function getProviders() {
  const settings = getSettings();
  return settings.providers || [];
}

export function getActiveProvider() {
  const settings = getSettings();
  const providers = settings.providers || [];
  const activeId = settings.activeProvider;
  return providers.find((p) => p.id === activeId) || providers[0] || null;
}

export async function addProvider(provider) {
  const settings = getSettings();
  const providers = settings.providers || [];
  provider.id = `provider-${Date.now()}`;
  providers.push(provider);
  await saveSetting("providers", providers);
  if (!settings.activeProvider) {
    await saveSetting("activeProvider", provider.id);
  }
  return provider;
}

export async function removeProvider(id) {
  const settings = getSettings();
  const providers = (settings.providers || []).filter((p) => p.id !== id);
  await saveSetting("providers", providers);
  if (settings.activeProvider === id) {
    await saveSetting("activeProvider", providers[0]?.id || null);
  }
}

export async function setActiveProvider(id) {
  await saveSetting("activeProvider", id);
}

export async function updateProvider(id, updates) {
  const settings = getSettings();
  const providers = settings.providers || [];
  const provider = providers.find((p) => p.id === id);
  if (provider) {
    Object.assign(provider, updates);
    await saveSetting("providers", providers);
  }
}

export async function updateProviderModel(id, model) {
  const settings = getSettings();
  const providers = settings.providers || [];
  const provider = providers.find((p) => p.id === id);
  if (provider) {
    provider.model = model;
    await saveSetting("providers", providers);
  }
}
