const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
const { getCurrentWindow, LogicalSize } = window.__TAURI__.window;

const WIDTH = 420;
const MAX_LINES = 8;
const $ = (id) => document.getElementById(id);

const ICONS = {
  terminal: '<path d="M4 17l6-5-6-5M12 19h8"/>',
  edit: '<path d="M12 20h9M16.5 3.5a2.1 2.1 0 013 3L7 19l-4 1 1-4z"/>',
  file: '<path d="M14 2H6a2 2 0 00-2 2v16a2 2 0 002 2h12a2 2 0 002-2V8z"/><path d="M14 2v6h6"/>',
  globe: '<circle cx="12" cy="12" r="10"/><path d="M2 12h20M12 2a15 15 0 010 20M12 2a15 15 0 000 20"/>',
  plug: '<path d="M9 2v6M15 2v6M6 8h12v4a6 6 0 01-12 0zM12 18v4"/>',
  list: '<path d="M8 6h13M8 12h13M8 18h13M3 6h.01M3 12h.01M3 18h.01"/>',
  question: '<circle cx="12" cy="12" r="10"/><path d="M9.1 9a3 3 0 015.8 1c0 2-3 3-3 3M12 17h.01"/>',
  tool: '<path d="M14.7 6.3a4 4 0 00-5.4 5.4L3 18l3 3 6.3-6.3a4 4 0 005.4-5.4l-2.5 2.5-2.4-.6-.6-2.4z"/>',
  check: '<path d="M20 6L9 17l-5-5"/>',
  x: '<path d="M18 6L6 18M6 6l12 12"/>',
  open: '<path d="M7 17L17 7M7 7h10v10"/>',
};
const icon = (name) => `<svg viewBox="0 0 24 24">${ICONS[name]}</svg>`;

const esc = (s) =>
  String(s ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]);

let pending = [];
let shownId = null;

async function load() {
  pending = await invoke("get_pending");
  render();
}

function render() {
  const card = $("card");
  const req = pending[0];
  if (!req) {
    card.hidden = true;
    shownId = null;
    return;
  }
  card.hidden = false;
  card.dataset.kind = req.kind;
  if (req.id !== shownId) {
    shownId = req.id;
    card.classList.remove("enter");
    void card.offsetWidth; // restart the entrance animation
    card.classList.add("enter");
  }

  const question = req.kind === "question";
  const app = req.terminal_app || "terminal";

  $("mascot").src = question ? "ghost-question.svg" : "ghost-permission.svg";
  $("eyebrow").textContent = question ? "Claude has a question" : "Permission needed";
  $("title").textContent = req.project;
  $("meta").textContent = [req.terminal_app, req.session_name].filter(Boolean).join(" · ") || req.cwd;
  $("counter").textContent = pending.length > 1 ? `1 / ${pending.length}` : "";

  const view = question ? describeQuestion(req.tool_input) : describeTool(req);
  $("tool").hidden = !view.chip;
  $("chip").innerHTML = view.chip ? `${icon(view.icon)}${esc(view.chip)}` : "";
  $("target").innerHTML = view.target || "";
  $("body").innerHTML = view.body || "";

  let note = view.note || "";
  if (req.kind === "prompt") {
    note = req.can_answer
      ? `Waiting in ${app}. Your answer is typed into that tab for you.`
      : `This prompt didn't come through Claude Companion, so only ${app} can answer it.`;
  }
  if (question) note = `Answer it in ${app}.`;
  $("note").textContent = note;

  const actions = $("actions");
  actions.replaceChildren();
  const button = (label, iconName, cls, action) => {
    const b = document.createElement("button");
    b.innerHTML = `${icon(iconName)}<span>${esc(label)}</span>`;
    b.className = cls;
    b.onclick = () => respond(req.id, action);
    actions.append(b);
  };
  if (req.kind === "permission" || req.can_answer) {
    button("Deny", "x", "danger", "deny");
    button(`Open ${app}`, "open", "", "terminal");
    button("Allow", "check", "primary", "allow");
  } else {
    button("Dismiss", "x", "", "dismiss");
    button(`Open ${app}`, "open", "primary", "terminal");
  }

  tick();
  fitWindow();
}

async function respond(id, action) {
  pending = pending.filter((p) => p.id !== id);
  render();
  await invoke("respond", { id, action });
}

function tick() {
  const req = pending[0];
  const footer = $("footer");
  footer.hidden = !req || req.kind !== "permission";
  if (footer.hidden) return;
  const total = req.timeout_secs * 1000;
  const left = Math.max(0, req.created_at + total - Date.now());
  const m = Math.floor(left / 60000);
  const s = String(Math.floor((left % 60000) / 1000)).padStart(2, "0");
  $("footer-text").textContent = `Hands back to ${req.terminal_app || "the terminal"} in ${m}:${s}`;
  $("bar").style.width = `${(left / total) * 100}%`;
}

function fitWindow() {
  requestAnimationFrame(() => {
    const height = Math.ceil($("card").getBoundingClientRect().height);
    getCurrentWindow().setSize(new LogicalSize(WIDTH, height));
  });
}

// ---- describing tool calls ----

function toolLabel(name) {
  const mcp = /^mcp__(.+?)__(.+)$/.exec(name);
  return mcp ? `${mcp[1]} · ${mcp[2]}` : name;
}

/** "src/a/b/c/file.ts" -> dimmed "src/…/c/" + bold "file.ts" */
function pathHtml(path, cwd) {
  if (!path) return "";
  let rel = cwd && path.startsWith(cwd + "/") ? path.slice(cwd.length + 1) : path;
  const parts = rel.split("/");
  const file = parts.pop();
  let dirs = parts;
  if (dirs.length > 3) dirs = [dirs[0], "…", ...dirs.slice(-2)];
  const dir = dirs.length ? dirs.join("/") + "/" : "";
  return `<span class="dir">${esc(dir)}</span><b>${esc(file)}</b>`;
}

function clip(lines) {
  if (lines.length <= MAX_LINES) return { lines, more: 0 };
  return { lines: lines.slice(0, MAX_LINES), more: lines.length - MAX_LINES };
}

function codeBlock(text, prompt = false) {
  const { lines, more } = clip(String(text ?? "").split("\n"));
  const head = prompt ? '<span class="prompt">$</span>' : "";
  const tail = more ? `\n<span class="more">… ${more} more lines</span>` : "";
  return `<pre>${head}${esc(lines.join("\n"))}${tail}</pre>`;
}

function diffBlock(oldText, newText) {
  const del = clip(String(oldText ?? "").split("\n"));
  const add = clip(String(newText ?? "").split("\n"));
  const line = (cls, sign, text) => `<span class="line ${cls}">${sign} ${esc(text)}</span>`;
  const more = (n) => (n ? `<span class="line more">  … ${n} more</span>` : "");
  return (
    "<pre>" +
    (oldText ? del.lines.map((l) => line("del", "−", l)).join("") + more(del.more) : "") +
    add.lines.map((l) => line("add", "+", l)).join("") +
    more(add.more) +
    "</pre>"
  );
}

function describeTool({ tool_name: name, tool_input, cwd }) {
  const input = tool_input || {};
  if (!name) return { note: "" };
  switch (name) {
    case "Bash":
      return { chip: "Bash", icon: "terminal", body: codeBlock(input.command, true), note: input.description };
    case "Edit":
      return {
        chip: "Edit",
        icon: "edit",
        target: pathHtml(input.file_path, cwd),
        body: diffBlock(input.old_string, input.new_string),
      };
    case "MultiEdit": {
      const first = (input.edits || [])[0] || {};
      const n = (input.edits || []).length;
      return {
        chip: `Edit ×${n}`,
        icon: "edit",
        target: pathHtml(input.file_path, cwd),
        body: diffBlock(first.old_string, first.new_string),
      };
    }
    case "Write":
      return { chip: "Write", icon: "file", target: pathHtml(input.file_path, cwd), body: diffBlock("", input.content) };
    case "Read":
      return { chip: "Read", icon: "file", target: pathHtml(input.file_path, cwd) };
    case "NotebookEdit":
      return { chip: "Notebook", icon: "edit", target: pathHtml(input.notebook_path, cwd), body: codeBlock(input.new_source) };
    case "WebFetch":
      return { chip: "Fetch", icon: "globe", target: esc(input.url), note: input.prompt };
    case "WebSearch":
      return { chip: "Search", icon: "globe", target: esc(input.query) };
    case "ExitPlanMode":
      return { chip: "Plan", icon: "list", body: codeBlock(input.plan), note: "Claude wants to start on this plan." };
    default:
      return {
        chip: toolLabel(name),
        icon: name.startsWith("mcp__") ? "plug" : "tool",
        body: Object.keys(input).length ? codeBlock(JSON.stringify(input, null, 2)) : "",
      };
  }
}

function describeQuestion(input) {
  const questions = ((input || {}).questions || []).map((q) => q.question).filter(Boolean);
  return {
    chip: "Question",
    icon: "question",
    body: questions.length ? codeBlock(questions.join("\n\n")) : "",
  };
}

listen("pending-changed", load);
setInterval(tick, 1000);
load();
