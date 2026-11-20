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
const drawerTriggers = new Map();
const narrowInspector = matchMedia("(max-width: 940px)");
const narrowSidebar = matchMedia("(max-width: 650px)");
const permissions = {
  read: ["Только чтение", "Агент может читать файлы, запись отключена."],
  edits: ["Accept Edits", "Правки разрешены внутри папки проекта."],
  auto: ["Approve for me", "Правки ограничены папкой проекта, запросы на действия проверяются автоматически."],
  full: ["Full Access", "Любые файлы и команды доступны без песочницы и подтверждений."]
};
const claudePermissionDescriptions = {
  read: "Режим dontAsk. Claude читает файлы, действия с запросом разрешения отклоняются.",
  edits: "Правки проекта принимаются автоматически. Действия, требующие разрешения, отклоняются. Песочницы ОС нет.",
  auto: "Claude проверяет действия классификатором. Он может разрешить действие вне папки проекта."
};
const agentDialog = $("agent-dialog");
const agentForm = $("agent-form");
const tokenNumber = new Intl.NumberFormat("en", { notation: "compact", maximumFractionDigits: 1 });
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
let agentEdit = null;
let modelCatalog = null;
let modelCatalogLoadedAt = 0;
let catalogRequest = null;
let quotas = null;
let quotaRequest = false;

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
  $("project-name").textContent = workspace?.replace(/^\\\\\?\\/, "").split(/[\\/]/).filter(Boolean).at(-1) || "Выбрать папку";
  $("change-workspace").title = workspace ? `${workspace}\nНовый чат в другой папке` : "Выбрать папку проекта";
  $("change-workspace").disabled = !connected;
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
  for (const button of document.querySelectorAll("[data-agent-settings]")) {
    const agent = button.dataset.agentSettings;
    const settings = room?.agents?.[agent];
    const model = settings?.model;
    const modelName = model ? modelCatalog?.[agent]?.find((entry) => entry.id === model)?.name || model : "По умолчанию";
    const permission = permissions[settings?.permission]?.[0];
    const caption = button.querySelector(".agent-config");
    caption.textContent = [modelName, settings?.effort, permission].filter(Boolean).join(" · ");
    button.title = caption.textContent;
    button.disabled = !connected || !settings;
    button.classList.toggle("full-access", settings?.permission === "full");
    $("status-" + agent).textContent = contextSummary(room?.context?.[agent]);
  }
}

function contextSummary(context) {
  if (context?.used == null) return "—";
  const used = tokenNumber.format(context.used);
  return context.window > 0 ? `${used} / ${tokenNumber.format(context.window)}` : `${used} ток.`;
}

function meter(value, max, label) {
  const element = node("progress", "meter");
  element.max = max > 0 ? max : 1;
  element.value = value == null || !(max > 0) ? 0 : Math.min(max, Math.max(0, value));
  element.classList.toggle("unknown", value == null || !(max > 0));
  element.setAttribute("aria-label", label);
  if (value == null || !(max > 0)) element.setAttribute("aria-hidden", "true");
  return element;
}

function renderContext() {
  const room = rooms.get(activeId);
  const list = $("agent-list");
  list.replaceChildren();
  for (const agent of agents) {
    const card = node("div", "agent-card");
    const working = room?.busy.includes(agent);
    const status = working ? "Работает" : health && !health[agent] ? "CLI недоступен" : "Ожидает";
    const top = node("div", "agent-card-top");
    top.append(avatar(agent), node("strong", "", names[agent]), node("span", working ? "agent-state working" : "agent-state", status));
    const context = room?.context?.[agent];
    const usage = node("div", "meter-caption");
    usage.append(node("span", "", "Контекст"), node("span", "", contextSummary(context)));
    card.classList.add(agent);
    card.append(top, usage, meter(context?.used, context?.window, `Контекст ${names[agent]}`));
    if (health?.[agent]) card.title = health[agent];
    list.append(card);
  }
  const content = $("panel-content");
  content.replaceChildren();
  $("limits-toolbar").hidden = panel !== "limits";
  $("limits-error").hidden = panel !== "limits" || !$("limits-error").textContent;
  if (panel === "limits") {
    renderLimits(content);
    return;
  }
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

function resetText(epoch) {
  if (!(epoch > 0)) return "Сброс неизвестен";
  const minutes = Math.ceil((epoch * 1000 - Date.now()) / 60000);
  if (minutes <= 0) return "Время сброса прошло";
  const days = Math.floor(minutes / 1440);
  const hours = Math.floor(minutes % 1440 / 60);
  const rest = minutes % 60;
  const parts = [days && `${days} д`, hours && `${hours} ч`, rest && `${rest} мин`].filter(Boolean);
  return `Сброс через ${parts.join(" ")}`;
}

function mergeQuotas(next) {
  quotas ||= {};
  for (const agent of agents) {
    if (!Object.hasOwn(next, agent)) continue;
    const latest = next[agent];
    const current = quotas[agent];
    if (!latest || !current || Date.parse(latest.updated_at) >= Date.parse(current.updated_at)) quotas[agent] = latest;
  }
  const usage = agents.flatMap((agent) => {
    const percent = quotas[agent]?.windows?.find((window) => window.id === "5h")?.used_percent;
    return Number.isFinite(percent) ? [`${initials[agent]} ${Math.round(percent)}%`] : [];
  });
  $("status-limits").textContent = usage.length ? `5h · ${usage.join(" / ")}` : "";
  if (panel === "limits") renderContext();
}

function renderLimits(content) {
  for (const agent of agents) {
    const limits = quotas?.[agent];
    const group = node("section", `quota-card ${agent}`);
    const heading = node("div", "quota-heading");
    const plan = limits?.plan;
    const planLabel = plan ? plan.charAt(0).toUpperCase() + plan.slice(1) : "—";
    heading.append(node("strong", "", names[agent]), node("span", "", planLabel));
    group.append(heading);
    if (!limits?.windows?.length) {
      group.append(node("p", "empty-panel", quotaRequest ? "Загружаем…" : "Данных пока нет"));
    } else {
      for (const id of ["5h", "7d"]) {
        const window = limits.windows.find((item) => item.id === id);
        const value = Number.isFinite(window?.used_percent) ? window.used_percent : null;
        const caption = node("div", "meter-caption");
        caption.append(node("span", "", id), node("span", "", value === null ? "—" : `${Math.round(value)}% использовано`));
        const reset = node("small", "", resetText(window?.resets_at));
        if (window?.resets_at) reset.dataset.reset = String(window.resets_at);
        group.append(caption, meter(value, 100, `${names[agent]}, ${id}`), reset);
      }
    }
    if (limits?.updated_at) {
      const updated = new Date(limits.updated_at);
      if (!Number.isNaN(updated.getTime())) {
        group.title = `Обновлено ${date.format(updated)} в ${clock.format(updated)}`;
      }
    }
    content.append(group);
  }
}

async function loadLimits(refresh = false) {
  if (quotaRequest) return;
  quotaRequest = true;
  $("refresh-limits").disabled = true;
  $("refresh-limits").textContent = refresh ? "Обновляем…" : "↻ Обновить";
  showError("limits-error", null);
  if (panel === "limits") renderContext();
  try {
    const path = refresh ? "/api/limits/refresh" : "/api/limits";
    mergeQuotas(await request(path, refresh ? "POST" : "GET"));
  } catch (error) {
    if (refresh || panel === "limits") showError("limits-error", error);
  } finally {
    quotaRequest = false;
    $("refresh-limits").disabled = !connected;
    $("refresh-limits").textContent = "↻ Обновить";
    if (panel === "limits") renderContext();
  }
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
      if (event.room.id === activeId) renderContext();
    }
  } else if (event.type === "limits" && agents.includes(event.agent)) {
    if (event.limits) mergeQuotas({ [event.agent]: event.limits });
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
  $("refresh-limits").disabled = !value || quotaRequest;
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
      void loadLimits();
      if (rooms.get(activeId)?.agents) void loadModels().catch(() => {});
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

function openRoomDialog(mode, trigger = null) {
  const room = rooms.get(activeId);
  const creating = mode === "create";
  const deleting = mode === "delete";
  const titleInput = roomForm.elements.title;
  const workspace = roomForm.elements.workspace;
  roomAction = { mode, id: room?.id };
  dialogTrigger = trigger || (creating ? $("new-room") : $("room-actions-toggle"));
  setRoomMenu(false);
  roomForm.reset();
  titleInput.value = creating ? "" : room.title;
  workspace.value = creating ? (room?.workspace || "") : "";
  titleInput.closest("label").hidden = deleting;
  titleInput.required = mode === "rename";
  $("workspace-field").hidden = !creating;
  $("workspace-note").hidden = !creating;
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
$("change-workspace").addEventListener("click", () => {
  openRoomDialog("create", $("change-workspace"));
  if (!$("pick-folder").hidden) $("pick-folder").click();
  else roomForm.elements.workspace.focus();
});
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

async function loadModels() {
  if (modelCatalog && Date.now() - modelCatalogLoadedAt < 600000) return modelCatalog;
  if (!catalogRequest) {
    catalogRequest = request("/api/models").then((result) => {
      modelCatalog = result;
      modelCatalogLoadedAt = Date.now();
      renderControls();
      return result;
    }).finally(() => { catalogRequest = null; });
  }
  return catalogRequest;
}

function addOption(select, value, label) {
  const element = node("option", "", label);
  element.value = value;
  select.append(element);
}

function updateEfforts(savedEffort = "") {
  const { model, effort } = agentForm.elements;
  const available = modelCatalog?.[agentEdit.agent] || [];
  const details = model.value
    ? available.find((entry) => entry.id === model.value)
    : available.find((entry) => entry.is_default);
  $("model-description").textContent = details?.description || "";
  effort.replaceChildren();
  const automatic = details?.default_effort;
  addOption(effort, "", automatic ? `По умолчанию (${automatic})` : "По умолчанию");
  const supported = new Set(details?.efforts || []);
  if (savedEffort) supported.add(savedEffort);
  for (const value of supported) addOption(effort, value, value);
  effort.value = savedEffort;
}

function updatePermission() {
  const { permission, confirmFull } = agentForm.elements;
  const full = permission.value === "full";
  const providerDescription = agentEdit.agent === "claude" && claudePermissionDescriptions[permission.value];
  $("permission-description").textContent = providerDescription || permissions[permission.value][1];
  $("permission-description").classList.toggle("full-access", full);
  $("full-warning").hidden = !full || agentEdit.originalPermission === "full";
  confirmFull.required = !$("full-warning").hidden;
  confirmFull.checked = false;
}

async function openAgentSettings(button) {
  const agent = button.dataset.agentSettings;
  const settings = rooms.get(activeId)?.agents?.[agent];
  if (!settings || !connected) return;
  const edit = { id: activeId, agent, originalPermission: settings.permission, button };
  agentEdit = edit;
  agentForm.reset();
  $("agent-dialog-title").textContent = `Настройки ${names[agent]}`;
  const { model, effort, permission } = agentForm.elements;
  model.replaceChildren();
  addOption(model, "", "По умолчанию");
  if (settings.model) addOption(model, settings.model, settings.model);
  model.value = settings.model;
  permission.replaceChildren();
  for (const [id, [label]] of Object.entries(permissions)) addOption(permission, id, label);
  permission.value = Object.hasOwn(permissions, settings.permission) ? settings.permission : "read";
  updateEfforts(settings.effort);
  updatePermission();
  showError("agent-error", null);
  model.disabled = effort.disabled = $("save-agent").disabled = true;
  agentDialog.showModal();
  permission.focus();
  try {
    const catalog = await loadModels();
    if (agentEdit !== edit) return;
    const entries = catalog[agent] || [];
    model.replaceChildren();
    const defaultModel = entries.find((entry) => entry.is_default);
    addOption(model, "", defaultModel?.id ? `По умолчанию · ${defaultModel.name}` : "По умолчанию");
    for (const entry of entries) {
      if (entry.id) addOption(model, entry.id, entry.name);
    }
    if (settings.model && !entries.some((entry) => entry.id === settings.model)) {
      addOption(model, settings.model, settings.model);
    }
    model.value = settings.model;
    updateEfforts(settings.effort);
    model.disabled = effort.disabled = false;
  } catch {
    if (agentEdit === edit) showError("agent-error", "Каталог моделей недоступен. Можно изменить разрешения.");
  } finally {
    if (agentEdit === edit) $("save-agent").disabled = !connected;
  }
}

for (const button of document.querySelectorAll("[data-agent-settings]")) {
  button.addEventListener("click", () => void openAgentSettings(button));
}
agentForm.elements.model.addEventListener("change", () => updateEfforts());
agentForm.elements.permission.addEventListener("change", updatePermission);
$("cancel-agent").addEventListener("click", () => agentDialog.close());
agentDialog.addEventListener("cancel", (event) => {
  if ($("cancel-agent").disabled) event.preventDefault();
});
agentDialog.addEventListener("close", () => {
  agentEdit?.button.focus();
  agentEdit = null;
});
agentForm.addEventListener("submit", async (event) => {
  event.preventDefault();
  if (!agentEdit || !connected || $("save-agent").disabled) return;
  const { id, agent } = agentEdit;
  const { model, effort, permission } = agentForm.elements;
  const settings = { model: model.value, effort: effort.value, permission: permission.value };
  $("cancel-agent").disabled = $("save-agent").disabled = true;
  showError("agent-error", null);
  try {
    const result = await request(roomPath(id, `/agents/${agent}`), "PUT", settings);
    if (rooms.has(id)) {
      const room = rooms.get(id);
      room.agents = { ...room.agents, [agent]: result.agents[agent] };
      renderControls();
    }
    agentDialog.close();
  } catch (error) {
    showError("agent-error", error);
  } finally {
    $("cancel-agent").disabled = $("save-agent").disabled = false;
  }
});

function setDrawer(drawer, open, trigger = null) {
  const containsFocus = drawer.contains(document.activeElement);
  drawer.classList.toggle("is-open", open);
  const toggles = [...document.querySelectorAll(`[aria-controls="${drawer.id}"]`)];
  for (const button of toggles) button.setAttribute("aria-expanded", String(open));
  if (open) {
    drawerTriggers.set(drawer, trigger);
    drawer.querySelector("button")?.focus();
  } else {
    const origin = drawerTriggers.get(drawer);
    if (containsFocus) {
      const target = origin?.getClientRects().length ? origin : toggles.find((button) => button.getClientRects().length);
      target?.focus();
    }
    drawerTriggers.delete(drawer);
  }
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
  setDrawer($("sidebar"), !$("sidebar").classList.contains("is-open"), $("navigation-toggle"));
});
for (const toggle of document.querySelectorAll(".panel-toggle")) {
  toggle.addEventListener("click", () => {
    setDrawer($("sidebar"), false);
    setDrawer($("inspector"), !$("inspector").classList.contains("is-open"), toggle);
  });
}
narrowInspector.addEventListener("change", (event) => {
  if (!event.matches) setDrawer($("inspector"), false);
});
narrowSidebar.addEventListener("change", (event) => {
  if (!event.matches) setDrawer($("sidebar"), false);
});
document.querySelector(".panel-close").addEventListener("click", () => setDrawer($("inspector"), false));
function selectPanel(next) {
  panel = next;
  for (const tab of document.querySelectorAll("[data-panel]")) {
    tab.setAttribute("aria-pressed", String(tab.dataset.panel === next));
  }
  renderContext();
  if (panel === "limits" && !quotas && connected) void loadLimits();
}

for (const button of document.querySelectorAll("[data-panel]")) {
  button.addEventListener("click", () => selectPanel(button.dataset.panel));
}
$("open-limits").addEventListener("click", () => {
  setDrawer($("sidebar"), false);
  if (narrowInspector.matches) setDrawer($("inspector"), true, $("open-limits"));
  selectPanel("limits");
  document.querySelector('[data-panel="limits"]').focus();
});
$("refresh-limits").addEventListener("click", () => void loadLimits(true));
setInterval(() => {
  for (const element of document.querySelectorAll("[data-reset]")) {
    element.textContent = resetText(Number(element.dataset.reset));
  }
}, 30000);
document.addEventListener("keydown", (event) => {
  if (event.key !== "Escape" || event.defaultPrevented || dialog.open || agentDialog.open) return;
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
