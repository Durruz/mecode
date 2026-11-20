"use strict";

const apiBase = window.MECODE_API;
const agents = ["claude", "codex"];
const names = { user: "Вы", claude: "Claude", codex: "Codex", system: "Система" };
const initials = { user: "В", claude: "Cl", codex: "Cx", system: "·" };
const mentions = [
  { token: "@claude", label: "Claude" },
  { token: "@codex", label: "Codex" },
  { token: "@all", label: "Оба агента" }
];
const $ = (id) => document.getElementById(id);
const rooms = new Map();
const history = new Map();
const drafts = new Map();
const streams = new Map();
const sending = new Set();
const savingTarget = new Map();
const stopping = new Set();
const radios = [...document.querySelectorAll('input[name="target"]')];
const composer = $("composer");
const draft = $("draft");
const feed = $("feed");
const feedScroll = $("feed-scroll");
const dialog = $("room-dialog");
const roomForm = $("room-form");
const markdown = window.markdownit({ breaks: true, html: false });
const clock = new Intl.DateTimeFormat("ru", { hour: "2-digit", minute: "2-digit" });
const date = new Intl.DateTimeFormat("ru", { day: "numeric", month: "long", year: "numeric" });
let activeId = null;
let desiredRoom = localStorage.getItem("mecode.room");
let health = null;
let connected = false;
let historySync = null;
let panel = "activity";
let mentionIndex = 0;
let options = [];
let roomAction = null;
let dialogTrigger = null;

// Agent output stays in the chat. Render link text and image captions without loading remote content.
markdown.renderer.rules.link_open = (tokens, index) => {
  const url = markdown.utils.escapeHtml(tokens[index].attrGet("href") || "");
  return `<span class="markdown-link" title="${url}">`;
};
markdown.renderer.rules.link_close = () => "</span>";
markdown.renderer.rules.image = (tokens, index) => markdown.utils.escapeHtml(tokens[index].content);

function node(tag, className, value) {
  const element = document.createElement(tag);
  if (className) element.className = className;
  if (value !== undefined) element.textContent = value;
  return element;
}

function showError(id, error) {
  const element = $(id);
  element.textContent = error ? (error.message || String(error)) : "";
  element.hidden = !error;
}

async function request(path, method = "GET", body) {
  const init = { method };
  if (body !== undefined) {
    init.headers = { "Content-Type": "application/json" };
    init.body = JSON.stringify(body);
  }
  const response = await fetch(apiBase + path, init);
  if (!response.ok) {
    const answer = await response.json().catch(() => ({}));
    throw new Error(answer.error || `Ошибка сервера (${response.status})`);
  }
  return response.json();
}

function roomPath(id, suffix = "") {
  return `/api/rooms/${encodeURIComponent(id)}${suffix}`;
}

function avatar(author) {
  const element = node("span", `avatar ${author}`, initials[author]);
  element.setAttribute("aria-hidden", "true");
  return element;
}

function messageNode(message) {
  const article = node("article", `message ${message.author} ${message.kind}`);
  const content = node("div", "message-content");
  const meta = node("div", "message-meta");
  const label = node("strong", "", names[message.author]);
  const time = node("time", "", clock.format(new Date(message.ts)));
  time.dateTime = message.ts;
  time.title = date.format(new Date(message.ts));
  meta.append(label, time);
  article.append(avatar(message.author), content);
  content.append(meta);

  if (message.kind === "activity") {
    const details = node("details", "activity-details");
    details.append(node("summary", "", message.text.split("\n")[0]), node("pre", "", message.text));
    content.append(details);
  } else if (message.kind === "chat" && agents.includes(message.author)) {
    const body = node("div", "markdown");
    body.innerHTML = markdown.render(message.text);
    content.append(body);
  } else {
    content.append(node("p", "message-body", message.text));
  }
  return article;
}

function messagesFor(id) {
  if (!history.has(id)) history.set(id, new Map());
  return history.get(id);
}

function nearBottom() {
  return feedScroll.scrollHeight - feedScroll.clientHeight - feedScroll.scrollTop < 80;
}

function renderFeed(scrollToEnd = false) {
  const messages = [...(history.get(activeId)?.values() || [])].sort((a, b) => a.seq - b.seq);
  feed.replaceChildren();
  if (!messages.length) {
    const empty = node("div", "empty-feed");
    const title = activeId ? (historySync === activeId ? "Загружаем историю…" : "Начните разговор") : "Выберите или создайте чат";
    const description = activeId
      ? "Напишите задачу. Определите, кто ответит, с помощью адресата или @упоминания."
      : "Одна история для вас, Claude и Codex.";
    empty.append(node("h2", "", title), node("p", "", description));
    feed.append(empty);
  }
  for (const message of messages) appendMessage(message);
  renderStreams();
  if (scrollToEnd) feedScroll.scrollTop = feedScroll.scrollHeight;
}

function appendMessage(message) {
  const day = date.format(new Date(message.ts));
  if (feed.lastElementChild?.dataset.day !== day) feed.append(node("div", "day-divider", day));
  const article = messageNode(message);
  article.dataset.day = day;
  article.dataset.seq = String(message.seq);
  feed.append(article);
}

function renderStreams() {
  const live = $("live-drafts");
  live.replaceChildren();
  const current = streams.get(activeId);
  for (const author of agents) {
    const output = current?.get(author);
    if (output === undefined) continue;
    const message = node("article", `message ${author} live-message`);
    const content = node("div", "message-content");
    const meta = node("div", "message-meta");
    meta.append(node("strong", "", names[author]), node("span", "destination", "печатает…"));
    content.append(meta, node("p", "message-body", output));
    message.append(avatar(author), content);
    live.append(message);
  }
}

function renderRooms() {
  const list = $("room-list");
  const focusId = list.contains(document.activeElement) ? document.activeElement.dataset.room : null;
  list.replaceChildren();
  const sorted = [...rooms.values()].sort((a, b) => b.created.localeCompare(a.created));
  for (const room of sorted) {
    const button = node("button", "room-button");
    button.type = "button";
    button.dataset.room = room.id;
    button.title = room.title;
    button.setAttribute("aria-current", String(room.id === activeId));
    button.append(node("span", "chat-icon"), node("span", "room-name", room.title));
    if (room.busy.length) button.append(node("span", "room-working", "●"));
    list.append(button);
    if (focusId === room.id) button.focus();
  }
}

function renderControls() {
  const room = rooms.get(activeId);
  const workspace = room?.workspace;
  const title = room?.title || "Чаты проекта";
  $("room-title").textContent = title;
  $("room-actions").hidden = !room;
  $("project-name").textContent = workspace?.split(/[\\/]/).filter(Boolean).at(-1) || "Рабочая папка";
  $("workspace-path").textContent = workspace || "Выберите чат";
  $("workspace-path").title = workspace || "";
  $("new-room").disabled = !connected;
  $("rename-room").disabled = !connected;
  $("delete-room").disabled = !connected;
  draft.disabled = !room;
  const target = savingTarget.get(activeId) || room?.default_target || "all";
  for (const radio of radios) {
    radio.checked = radio.value === target;
    radio.disabled = !room || !connected || sending.has(activeId) || savingTarget.has(activeId);
  }
  $("send").disabled = !room || !connected || !draft.value.trim() || sending.has(activeId) || savingTarget.has(activeId);
  $("stop").hidden = !room?.busy.length;
  $("stop").disabled = !connected || stopping.has(activeId);
  $("queue-status").textContent = room?.busy.map((agent) => `${names[agent]} работает…`).join("  ") || "";
}

function renderContext() {
  const room = rooms.get(activeId);
  const list = $("agent-list");
  list.replaceChildren();
  for (const agent of agents) {
    const card = node("div", "agent-card");
    const working = room?.busy.includes(agent);
    const status = working ? "Работает" : health && !health[agent] ? "CLI недоступен" : "Ожидает";
    card.append(avatar(agent), node("strong", "", names[agent]), node("span", working ? "agent-state working" : "agent-state", status));
    if (health?.[agent]) card.title = health[agent];
    list.append(card);
  }
  const content = $("panel-content");
  content.replaceChildren();
  if (panel === "workspace") {
    content.append(node("p", "workspace-value", room?.workspace || "Выберите чат"));
    return;
  }
  const events = [...(history.get(activeId)?.values() || [])]
    .filter((message) => message.kind === "activity" || message.kind === "error")
    .sort((a, b) => b.seq - a.seq).slice(0, 12);
  if (!events.length) {
    content.append(node("p", "empty-panel", "Здесь появятся действия агентов и ошибки."));
    return;
  }
  const timeline = node("ol", "events");
  for (const event of events) {
    const row = node("li", event.kind);
    const detail = node("small", "", event.text.split("\n")[0]);
    detail.title = detail.textContent;
    row.append(node("strong", "", names[event.author]), detail);
    timeline.append(row);
  }
  content.append(timeline);
}

async function loadHistory(id, scrollToEnd = false) {
  historySync = id;
  try {
    const snapshot = await request(roomPath(id, "/messages"));
    if (!rooms.has(id)) return;
    const messages = messagesFor(id);
    for (const message of snapshot) messages.set(message.seq, message);
  } finally {
    if (historySync === id) historySync = null;
    if (activeId === id) {
      renderFeed(scrollToEnd || nearBottom());
      renderContext();
    }
  }
}

function selectRoom(id, { load = true } = {}) {
  activeId = id;
  if (id) localStorage.setItem("mecode.room", id);
  else localStorage.removeItem("mecode.room");
  draft.value = drafts.get(id) || "";
  setRoomMenu(false);
  showError("inline-error", null);
  closeMentions();
  renderRooms();
  renderControls();
  renderFeed(true);
  renderContext();
  if (id && load) loadHistory(id, true).catch((error) => {
    if (activeId === id) showError("inline-error", error);
  });
}

function forgetRoom(id) {
  rooms.delete(id);
  history.delete(id);
  streams.delete(id);
  drafts.delete(id);
  if (activeId === id) selectRoom(rooms.keys().next().value || null);
  else renderRooms();
}

function applyEvent(event) {
  const id = event.room_id;
  const room = rooms.get(id);
  if (event.type === "room_updated") {
    const previous = rooms.get(event.room.id);
    rooms.set(event.room.id, { ...event.room, busy: previous?.busy || [] });
    if (!activeId) selectRoom(event.room.id);
    else {
      renderRooms();
      renderControls();
    }
  } else if (event.type === "room_deleted") {
    forgetRoom(id);
  } else if (event.type === "agent_status" && room) {
    const busy = new Set(room.busy);
    if (event.busy) busy.add(event.agent);
    else {
      busy.delete(event.agent);
      streams.get(id)?.delete(event.agent);
    }
    room.busy = [...busy];
    renderRooms();
    if (activeId === id) {
      renderControls();
      renderStreams();
      renderContext();
    }
  } else if (event.type === "delta" && room) {
    if (!streams.has(id)) streams.set(id, new Map());
    streams.get(id).set(event.agent, event.text);
    if (activeId === id) {
      const follow = nearBottom();
      renderStreams();
      if (follow) feedScroll.scrollTop = feedScroll.scrollHeight;
    }
  } else if (event.type === "message" && room) {
    const message = event.message;
    if (message.kind === "chat") streams.get(id)?.delete(message.author);
    const messages = messagesFor(id);
    const exists = messages.has(message.seq);
    messages.set(message.seq, message);
    if (activeId === id) {
      const follow = nearBottom();
      if (!exists) {
        const last = Number(feed.lastElementChild?.dataset.seq);
        if (last < message.seq) appendMessage(message);
        else renderFeed();
      }
      renderStreams();
      if (message.kind !== "chat") renderContext();
      if (follow) feedScroll.scrollTop = feedScroll.scrollHeight;
    }
  }
}

function setConnection(value) {
  if (value && !connected) showError("inline-error", null);
  connected = value;
  $("connection").classList.toggle("online", value);
  $("connection-text").textContent = value ? "Локально" : "Переподключение…";
  renderControls();
}

function connect() {
  const socket = new WebSocket(apiBase.replace(/^http/, "ws") + "/api/events");
  let pendingEvents = null;
  async function sync() {
    if (pendingEvents) return;
    pendingEvents = [];
    try {
      const snapshot = await request("/api/rooms");
      if (socket.readyState !== WebSocket.OPEN) return;
      rooms.clear();
      for (const room of snapshot) rooms.set(room.id, room);
      const id = rooms.has(activeId) ? activeId
        : rooms.has(desiredRoom) ? desiredRoom : (rooms.keys().next().value || null);
      desiredRoom = null;
      if (id !== activeId) selectRoom(id, { load: false });
      streams.clear();
      if (id) await loadHistory(id, true);
      if (socket.readyState !== WebSocket.OPEN) return;
      const queued = pendingEvents;
      pendingEvents = null;
      let resync = false;
      for (const event of queued) {
        if (event.type === "resync") resync = true;
        else applyEvent(event);
      }
      setConnection(true);
      renderRooms();
      renderContext();
      if (resync) void sync();
    } catch (error) {
      showError("inline-error", error);
      socket.close();
    } finally {
      if (socket.readyState !== WebSocket.OPEN) pendingEvents = null;
    }
  }
  socket.addEventListener("open", () => void sync());
  socket.addEventListener("close", () => {
    setConnection(false);
    streams.clear();
    renderStreams();
    setTimeout(connect, 1500);
  });
  socket.addEventListener("message", ({ data }) => {
    const event = JSON.parse(data);
    if (pendingEvents) pendingEvents.push(event);
    else if (event.type === "resync") void sync();
    else applyEvent(event);
  });
}

function mentionAtCursor() {
  const before = draft.value.slice(0, draft.selectionStart);
  const match = /(?:^|\s)(@[a-z]*)$/i.exec(before);
  return match ? { start: before.length - match[1].length, token: match[1].toLowerCase() } : null;
}

function closeMentions() {
  options = [];
  $("mention-list").hidden = true;
  draft.removeAttribute("aria-activedescendant");
}

function acceptMention(token) {
  const match = mentionAtCursor();
  if (!match) return;
  const remainder = /^[a-z]*/i.exec(draft.value.slice(draft.selectionStart))[0];
  draft.setRangeText(token + " ", match.start, draft.selectionStart + remainder.length, "end");
  drafts.set(activeId, draft.value);
  closeMentions();
  renderControls();
  draft.focus();
}

function renderMentions(reset = false) {
  const mention = mentionAtCursor();
  options = mention ? mentions.filter((option) => option.token.startsWith(mention.token)) : [];
  if (!options.length) return closeMentions();
  if (reset || mentionIndex >= options.length) mentionIndex = 0;
  const list = $("mention-list");
  list.replaceChildren();
  options.forEach(({ token, label }, index) => {
    const option = node("div", "mention-option");
    option.id = `mention-option-${index}`;
    option.setAttribute("role", "option");
    option.setAttribute("aria-selected", String(index === mentionIndex));
    option.append(node("code", "", token), node("span", "", label));
    option.addEventListener("pointerdown", (event) => event.preventDefault());
    option.addEventListener("click", () => acceptMention(token));
    list.append(option);
  });
  list.hidden = false;
  draft.setAttribute("aria-activedescendant", `mention-option-${mentionIndex}`);
}

draft.addEventListener("input", () => {
  drafts.set(activeId, draft.value);
  renderControls();
  renderMentions(true);
});
draft.addEventListener("blur", closeMentions);
draft.addEventListener("click", () => renderMentions(true));
draft.addEventListener("keydown", (event) => {
  if (event.isComposing) return;
  if (options.length && ["ArrowUp", "ArrowDown"].includes(event.key)) {
    event.preventDefault();
    mentionIndex = (mentionIndex + (event.key === "ArrowUp" ? -1 : 1) + options.length) % options.length;
    renderMentions();
  } else if (options.length && (event.key === "Tab" && !event.shiftKey || event.key === "Enter" && !event.shiftKey)) {
    event.preventDefault();
    acceptMention(options[mentionIndex].token);
  } else if (options.length && event.key === "Escape") {
    event.preventDefault();
    closeMentions();
  } else if (event.key === "Enter" && !event.shiftKey) {
    event.preventDefault();
    if (!$("send").disabled) composer.requestSubmit();
  }
});

composer.addEventListener("change", async (event) => {
  if (event.target.name !== "target") return;
  const id = activeId;
  const target = event.target.value;
  savingTarget.set(id, target);
  renderControls();
  try {
    await request(roomPath(id, "/target"), "PUT", { target });
    if (rooms.has(id)) rooms.get(id).default_target = target;
  } catch (error) {
    if (activeId === id) showError("inline-error", error);
  } finally {
    savingTarget.delete(id);
    renderControls();
  }
});

composer.addEventListener("submit", async (event) => {
  event.preventDefault();
  if ($("send").disabled) return;
  const id = activeId;
  const raw = draft.value;
  sending.add(id);
  closeMentions();
  showError("inline-error", null);
  renderControls();
  try {
    await request(roomPath(id, "/messages"), "POST", { text: raw.trim() });
    if (drafts.get(id) === raw) {
      drafts.delete(id);
      if (activeId === id) draft.value = "";
    }
  } catch (error) {
    if (activeId === id) showError("inline-error", error);
  } finally {
    sending.delete(id);
    renderControls();
  }
});

$("stop").addEventListener("click", async () => {
  const id = activeId;
  stopping.add(id);
  renderControls();
  try {
    await request(roomPath(id, "/stop"), "POST");
  } catch (error) {
    if (activeId === id) showError("inline-error", error);
  } finally {
    stopping.delete(id);
    renderControls();
  }
});

function openRoomDialog(mode) {
  const room = rooms.get(activeId);
  const creating = mode === "create";
  const deleting = mode === "delete";
  const titleInput = roomForm.elements.title;
  const workspace = roomForm.elements.workspace;
  roomAction = { mode, id: room?.id };
  dialogTrigger = creating ? $("new-room") : $("room-actions-toggle");
  setRoomMenu(false);
  roomForm.reset();
  titleInput.value = creating ? "" : room.title;
  workspace.value = creating ? (room?.workspace || "") : "";
  titleInput.closest("label").hidden = deleting;
  titleInput.required = mode === "rename";
  $("workspace-field").hidden = !creating;
  workspace.required = creating;
  $("delete-warning").hidden = !deleting;
  if (deleting) $("delete-warning").textContent = `История чата «${room.title}» будет удалена. Работа агентов в этом чате остановится.`;
  $("room-dialog-title").textContent = creating ? "Новый чат" : deleting ? "Удалить чат?" : "Переименовать чат";
  $("create-room").textContent = creating ? "Создать чат" : deleting ? "Удалить чат" : "Сохранить";
  $("create-room").classList.toggle("danger", deleting);
  showError("room-error", null);
  dialog.showModal();
  if (deleting) $("cancel-room").focus();
  else titleInput.focus();
}

$("new-room").addEventListener("click", () => openRoomDialog("create"));
$("rename-room").addEventListener("click", () => openRoomDialog("rename"));
$("delete-room").addEventListener("click", () => openRoomDialog("delete"));
$("cancel-room").addEventListener("click", () => dialog.close());
dialog.addEventListener("close", () => {
  if (dialogTrigger?.getClientRects().length) dialogTrigger.focus();
  else if ($("new-room").getClientRects().length) $("new-room").focus();
});
dialog.addEventListener("cancel", (event) => {
  if ($("create-room").disabled) event.preventDefault();
});
$("pick-folder").hidden = !window.__TAURI__?.dialog;
$("pick-folder").addEventListener("click", async () => {
  try {
    const folder = await window.__TAURI__.dialog.open({ directory: true, multiple: false, title: "Выберите папку проекта" });
    if (folder && dialog.open && roomAction.mode === "create") roomForm.elements.workspace.value = folder;
  } catch (error) {
    showError("room-error", error);
  }
});
roomForm.addEventListener("submit", async (event) => {
  event.preventDefault();
  const { id, mode } = roomAction;
  const submit = $("create-room");
  const cancel = $("cancel-room");
  const title = roomForm.elements.title.value.trim();
  const workspace = roomForm.elements.workspace.value.trim();
  submit.disabled = cancel.disabled = true;
  showError("room-error", null);
  try {
    if (mode === "delete") {
      await request(roomPath(id), "DELETE");
      forgetRoom(id);
    } else {
      const room = mode === "create"
        ? await request("/api/rooms", "POST", { title, workspace })
        : await request(roomPath(id), "PATCH", { title });
      applyEvent({ type: "room_updated", room });
      if (mode === "create") selectRoom(room.id);
    }
    dialog.close();
    if (mode === "create") {
      setDrawer($("sidebar"), false);
      requestAnimationFrame(() => draft.focus());
    }
  } catch (error) {
    showError("room-error", error);
  } finally {
    submit.disabled = cancel.disabled = false;
  }
});

function setDrawer(drawer, open) {
  const containsFocus = drawer.contains(document.activeElement);
  drawer.classList.toggle("is-open", open);
  const toggles = [...document.querySelectorAll(`[aria-controls="${drawer.id}"]`)];
  for (const button of toggles) button.setAttribute("aria-expanded", String(open));
  if (open) drawer.querySelector("button")?.focus();
  else if (containsFocus) toggles.find((button) => button.getClientRects().length)?.focus();
}

function setRoomMenu(open) {
  $("room-action-list").hidden = !open;
  $("room-actions-toggle").setAttribute("aria-expanded", String(open));
}

$("room-actions-toggle").addEventListener("click", () => setRoomMenu($("room-action-list").hidden));
$("room-list").addEventListener("click", (event) => {
  const button = event.target.closest("button[data-room]");
  if (!button) return;
  selectRoom(button.dataset.room);
  setDrawer($("sidebar"), false);
});
$("navigation-toggle").addEventListener("click", () => {
  setDrawer($("inspector"), false);
  setDrawer($("sidebar"), !$("sidebar").classList.contains("is-open"));
});
for (const toggle of document.querySelectorAll(".panel-toggle")) {
  toggle.addEventListener("click", () => {
    setDrawer($("sidebar"), false);
    setDrawer($("inspector"), !$("inspector").classList.contains("is-open"));
  });
}
document.querySelector(".panel-close").addEventListener("click", () => setDrawer($("inspector"), false));
for (const button of document.querySelectorAll("[data-panel]")) {
  button.addEventListener("click", () => {
    panel = button.dataset.panel;
    for (const tab of document.querySelectorAll("[data-panel]")) tab.setAttribute("aria-pressed", String(tab === button));
    renderContext();
  });
}
document.addEventListener("keydown", (event) => {
  if (event.key !== "Escape" || event.defaultPrevented || dialog.open) return;
  if (!$("room-action-list").hidden) {
    setRoomMenu(false);
    $("room-actions-toggle").focus();
  } else {
    setDrawer($("inspector"), false);
    setDrawer($("sidebar"), false);
  }
});
document.addEventListener("click", (event) => {
  if (!$("room-actions").contains(event.target)) setRoomMenu(false);
});

selectRoom(null, { load: false });
if (apiBase) {
  connect();
  request("/api/health").then((result) => {
    health = result;
    renderContext();
  }).catch(() => {});
} else {
  $("connection-text").textContent = "Сервер недоступен";
}
