"use strict";

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const $ = (sel) => document.querySelector(sel);
const $$ = (sel) => Array.from(document.querySelectorAll(sel));

const els = {
  app: $("#app"),
  password: $("#password"),
  strength: $("#strength"),
  bits: $("#bits"),
  summary: $("#summary"),
  preset: $("#preset"),
  kinds: $$("[data-kind]"),
  lengthLabel: $("#length-label"),
  slider: $("#length-slider"),
  length: $("#length"),
  classes: $$("[data-class]"),
  symbolSet: $("#symbol-set"),
  noAmbiguous: $("#no-ambiguous"),
  separator: $("#separator"),
  capitalize: $("#capitalize"),
  wordDigit: $("#word-digit"),
  regenerate: $("#regenerate"),
  openSave: $("#open-save"),
  copy: $("#copy"),
  savePanel: $("#save-panel"),
  title: $("#title"),
  vault: $("#vault"),
  url: $("#url"),
  username: $("#username"),
  cancelSave: $("#cancel-save"),
  save: $("#save"),
  status: $("#status"),
  hint: $("#hint"),
};

const RULE_KEY = "mint.rule";
const VAULT_KEY = "mint.vault";

let rule = null;
let current = null; // last generated result
let presets = [];
let vaultsLoaded = false;
let clearAfter = 45;
let busy = false;
let statusTimer = null;

const isMac = navigator.userAgent.includes("Mac");
const MOD = isMac ? "⌘" : "Ctrl+";

function store(key, value) {
  try { localStorage.setItem(key, JSON.stringify(value)); } catch (_) { /* storage unavailable */ }
}
function load(key) {
  try { return JSON.parse(localStorage.getItem(key)); } catch (_) { return null; }
}

function errorText(e) {
  return (e && (e.error || e.message)) || String(e);
}

function setStatus(text, kind = "", ms = 0) {
  clearTimeout(statusTimer);
  els.status.className = kind;
  els.status.textContent = text;
  if (ms) statusTimer = setTimeout(() => setStatus(""), ms);
}

// ---------- rendering ----------

function strengthLevel(bits) {
  if (bits < 40) return 1;
  if (bits < 64) return 2;
  if (bits < 80) return 3;
  if (bits < 128) return 4;
  return 5;
}
const LEVEL_NAMES = ["", "Weak", "Fair", "Good", "Strong", "Excellent"];

function renderPassword(text) {
  const frag = document.createDocumentFragment();
  for (const ch of text) {
    const span = document.createElement("span");
    if (/[0-9]/.test(ch)) span.className = "d";
    else if (!/[A-Za-z ]/.test(ch)) span.className = "s";
    span.textContent = ch;
    frag.appendChild(span);
  }
  els.password.replaceChildren(frag);
  els.password.classList.remove("error");
  els.password.classList.toggle("long", text.length > 40 && text.length <= 120);
  els.password.classList.toggle("huge", text.length > 120);
}

function renderError(message) {
  current = null;
  els.password.classList.add("error");
  els.password.classList.remove("long", "huge");
  els.password.textContent = message;
  els.strength.dataset.level = "0";
  els.bits.textContent = "";
  els.summary.textContent = "";
}

function lengthBounds() {
  if (rule.kind === "words") return { min: 3, max: 12, label: "Words", value: rule.words };
  if (rule.kind === "pin") return { min: 4, max: 12, label: "Digits", value: rule.length };
  return { min: 4, max: 128, label: "Length", value: rule.length };
}

function renderControls() {
  for (const b of els.kinds) b.setAttribute("aria-checked", String(b.dataset.kind === rule.kind));
  const bounds = lengthBounds();
  els.lengthLabel.textContent = bounds.label;
  els.slider.min = bounds.min;
  els.slider.max = bounds.max;
  els.slider.value = Math.min(Math.max(bounds.value, bounds.min), bounds.max);
  els.length.max = rule.kind === "words" ? 512 : 4096;
  if (document.activeElement !== els.length) els.length.value = bounds.value;
  els.length.removeAttribute("aria-invalid");
  for (const b of els.classes) b.setAttribute("aria-pressed", String(rule[b.dataset.class]));
  if (document.activeElement !== els.symbolSet) els.symbolSet.value = rule.symbol_set;
  els.symbolSet.disabled = !rule.symbols;
  els.noAmbiguous.setAttribute("aria-pressed", String(rule.no_ambiguous));
  els.separator.value = rule.separator;
  els.capitalize.setAttribute("aria-pressed", String(rule.capitalize));
  els.wordDigit.setAttribute("aria-pressed", String(rule.word_digit));
  $$(".chars-only").forEach((el) => (el.hidden = rule.kind !== "chars"));
  $$(".words-only").forEach((el) => (el.hidden = rule.kind !== "words"));
  els.preset.value = rule.preset && presets.some((p) => p.name === rule.preset) ? rule.preset : "";
}

// ---------- generation ----------

async function regenerate() {
  try {
    current = await invoke("generate", { rule });
    renderPassword(current.password);
    const level = strengthLevel(current.entropy_bits);
    els.strength.dataset.level = String(level);
    els.bits.textContent = `${Math.round(current.entropy_bits)} bits · ${LEVEL_NAMES[level]}`;
    els.summary.textContent = current.summary;
    els.summary.title = current.summary;
  } catch (e) {
    renderError(errorText(e));
  }
  store(RULE_KEY, rule);
  renderControls();
}

/** Any manual change makes the rule custom. */
function edit(change) {
  change();
  rule.preset = null;
  rule.length_min = null;
  regenerate();
}

// ---------- copy / save ----------

async function copyAndHide() {
  if (!current || busy) return;
  try {
    await invoke("copy", { password: current.password });
    els.password.classList.add("flash");
    setTimeout(() => {
      els.password.classList.remove("flash");
      invoke("hide");
    }, 140);
  } catch (e) {
    setStatus(errorText(e), "error");
  }
}

function defaultVault(vaults) {
  const remembered = load(VAULT_KEY);
  if (remembered && vaults.some((v) => v.id === remembered)) return remembered;
  const preferred = vaults.find((v) => /^(private|personal|employee)$/i.test(v.name));
  return (preferred || vaults[0] || {}).id || "";
}

async function loadVaults() {
  if (vaultsLoaded) return;
  els.vault.replaceChildren(new Option("Loading vaults…", ""));
  setStatus("Waiting for 1Password. Approve mint if it asks.");
  try {
    const vaults = await invoke("vaults");
    els.vault.replaceChildren(...vaults.map((v) => new Option(v.name, v.id)));
    els.vault.value = defaultVault(vaults);
    vaultsLoaded = true;
    setStatus("");
  } catch (e) {
    els.vault.replaceChildren(new Option("Vaults unavailable", ""));
    setStatus(errorText(e), "error");
  }
}

function openSave() {
  if (!current) return;
  els.savePanel.hidden = false;
  els.title.focus();
  loadVaults();
}

function closeSave() {
  els.savePanel.hidden = true;
  els.password.focus();
}

async function submitSave(event) {
  if (event) event.preventDefault();
  if (busy || !current) return;
  const title = els.title.value.trim();
  if (!title) {
    els.title.setAttribute("aria-invalid", "true");
    els.title.focus();
    setStatus("Give the item a title.", "error");
    return;
  }
  els.title.removeAttribute("aria-invalid");
  busy = true;
  els.save.disabled = true;
  setStatus("Saving to 1Password. Approve mint if it asks.");
  try {
    const item = await invoke("save", {
      request: {
        title,
        vault: els.vault.value || null,
        url: els.url.value.trim() || null,
        username: els.username.value.trim() || null,
        password: current.password,
      },
    });
    store(VAULT_KEY, els.vault.value);
    els.savePanel.hidden = true;
    els.title.value = els.url.value = els.username.value = "";
    showSaved(item);
    els.password.focus();
  } catch (e) {
    setStatus(errorText(e), "error");
  } finally {
    busy = false;
    els.save.disabled = false;
  }
}

function showSaved(item) {
  clearTimeout(statusTimer);
  els.status.className = "ok";
  els.status.textContent = `Saved “${item.title}” in ${item.vault}. `;
  if (item.link) {
    const a = document.createElement("a");
    a.textContent = "Open in 1Password";
    a.addEventListener("click", () => invoke("open_item", { link: item.link }));
    els.status.appendChild(a);
  }
}

// ---------- wiring ----------

function bind() {
  els.password.addEventListener("click", copyAndHide);
  els.copy.addEventListener("click", copyAndHide);
  els.regenerate.addEventListener("click", regenerate);
  els.openSave.addEventListener("click", openSave);
  els.cancelSave.addEventListener("click", closeSave);
  els.savePanel.addEventListener("submit", submitSave);
  els.title.addEventListener("input", () => els.title.removeAttribute("aria-invalid"));

  for (const b of els.kinds) {
    b.addEventListener("click", () => edit(() => (rule.kind = b.dataset.kind)));
  }
  els.slider.addEventListener("input", () => {
    const n = Number(els.slider.value);
    edit(() => (rule.kind === "words" ? (rule.words = n) : (rule.length = n)));
  });
  els.length.addEventListener("input", () => {
    const n = Number(els.length.value);
    const max = Number(els.length.max);
    if (!Number.isInteger(n) || n < 1 || n > max) {
      els.length.setAttribute("aria-invalid", "true");
      return;
    }
    edit(() => (rule.kind === "words" ? (rule.words = n) : (rule.length = n)));
  });
  for (const b of els.classes) {
    b.addEventListener("click", () => edit(() => (rule[b.dataset.class] = !rule[b.dataset.class])));
  }
  els.symbolSet.addEventListener("input", () => edit(() => (rule.symbol_set = els.symbolSet.value)));
  els.noAmbiguous.addEventListener("click", () => edit(() => (rule.no_ambiguous = !rule.no_ambiguous)));
  els.separator.addEventListener("change", () => edit(() => (rule.separator = els.separator.value)));
  els.capitalize.addEventListener("click", () => edit(() => (rule.capitalize = !rule.capitalize)));
  els.wordDigit.addEventListener("click", () => edit(() => (rule.word_digit = !rule.word_digit)));
  els.preset.addEventListener("change", () => {
    const p = presets.find((x) => x.name === els.preset.value);
    if (p) {
      rule = structuredClone(p.rule);
      regenerate();
    } else {
      edit(() => {});
    }
  });

  document.addEventListener("keydown", (e) => {
    const mod = isMac ? e.metaKey : e.ctrlKey;
    const inSave = els.savePanel.contains(document.activeElement);
    const key = e.key.toLowerCase();
    if (mod && key === "r") {
      e.preventDefault();
      regenerate();
    } else if (mod && key === "s") {
      e.preventDefault();
      if (els.savePanel.hidden) openSave();
      else submitSave();
    } else if (mod && key === "p") {
      e.preventDefault();
      els.preset.focus();
      if (els.preset.showPicker) try { els.preset.showPicker(); } catch (_) { /* not supported */ }
    } else if (mod && key === "c" && !window.getSelection().toString() && !isEditable(document.activeElement)) {
      e.preventDefault();
      copyAndHide();
    } else if (e.key === "Escape") {
      e.preventDefault();
      if (!els.savePanel.hidden) closeSave();
      else invoke("hide");
    } else if (e.key === "Enter" && !inSave && !e.isComposing) {
      const t = document.activeElement;
      // Enter on a button presses that button; anywhere else it copies.
      if (t && t.tagName === "BUTTON" && t !== els.password && t !== els.copy) return;
      if (t && t.tagName === "SELECT") return;
      e.preventDefault();
      copyAndHide();
    } else if (mod && (key === "w" || key === "q")) {
      e.preventDefault();
      invoke("hide");
    }
  });
  // Stop the webview's own reload and context menu.
  document.addEventListener("contextmenu", (e) => {
    if (!isEditable(e.target)) e.preventDefault();
  });

  new ResizeObserver(() => {
    invoke("set_height", { height: Math.ceil(els.app.getBoundingClientRect().height) });
  }).observe(els.app);
}

function isEditable(el) {
  return el && (el.tagName === "INPUT" || el.tagName === "SELECT" || el.tagName === "TEXTAREA");
}

async function start() {
  for (const k of $$("kbd.mod")) k.textContent = MOD + k.textContent;
  bind();
  const init = await invoke("init");
  clearAfter = init.clear_after;
  rule = load(RULE_KEY) || init.rule;
  try {
    presets = await invoke("presets_list");
  } catch (e) {
    setStatus(errorText(e), "error");
  }
  els.preset.replaceChildren(
    new Option("Custom", ""),
    ...presets.map((p) => {
      const o = new Option(p.name, p.name);
      o.title = p.description;
      return o;
    }),
  );
  const keys = init.hotkey ? prettyHotkey(init.hotkey) : "";
  els.hint.textContent = keys ? `${keys} toggles · clears in ${clearAfter} s` : `clears in ${clearAfter} s`;
  if (init.hotkey_error) setStatus(init.hotkey_error, "error");
  if (!clearAfter) els.hint.textContent = keys ? `${keys} toggles` : "";
  await regenerate();
  els.password.focus();

  await listen("mint://shown", (event) => {
    if (busy) return;
    if (event.payload) {
      closeSaveSilently();
      if (!els.status.classList.contains("error")) setStatus("");
      regenerate();
    }
    els.password.focus();
  });
}

function closeSaveSilently() {
  els.savePanel.hidden = true;
}

function prettyHotkey(combo) {
  if (!isMac) return combo;
  const map = { ctrl: "⌃", control: "⌃", alt: "⌥", option: "⌥", shift: "⇧", cmd: "⌘", command: "⌘", super: "⌘", meta: "⌘" };
  return combo
    .split("+")
    .map((p) => map[p.trim().toLowerCase()] || p.trim().toUpperCase())
    .join("");
}

start().catch((e) => renderError(errorText(e)));
