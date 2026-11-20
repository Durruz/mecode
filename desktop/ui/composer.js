"use strict";

const pendingFiles = new Map();
const plannedRooms = new Set();
const fileLimit = 25 * 1024 * 1024;
const effortLabels = {
  low: "Low", medium: "Medium", high: "High",
  xhigh: "Extra High", max: "Max", ultra: "Ultra"
};
const composerPanels = {
  "tools-toggle": "composer-tools-panel",
  "model-toggle": "composer-model-panel",
  "context-toggle": "composer-context-panel"
};
let composerAgent = "claude";
let composerPanel = null;
let modelEdit = null;
let handoffPending = false;
let pickedForRoom = null;
let promptRoom = null;
let promptsBusy = false;

function modelEntry(agent, id) {
  const entries = modelCatalog?.[agent] || [];
  return entries.find((entry) => entry.id === id) || (!id ? entries.find((entry) => entry.is_default) : null);
}

function modelLabel(agent, id) {
  return modelEntry(agent, id)?.name || id || "По умолчанию";
}

function renderComposerControls(room) {
  const settings = room?.agents?.[composerAgent];
  const model = modelEntry(composerAgent, settings?.model || "");
  const effort = settings?.effort || model?.default_effort;
  $("model-dot").className = `agent-dot ${composerAgent}`;
  $("model-current").textContent = `${names[composerAgent]} · ${modelLabel(composerAgent, settings?.model || "")}`;
  $("effort-current").textContent = effortLabels[effort] || effort || "";
  $("model-toggle").title = $("model-current").textContent;
  $("model-toggle").disabled = !settings || !connected;
  $("permission-toggle").disabled = !settings || !connected;
  $("permission-toggle").classList.toggle("full-access", settings?.permission === "full");
  $("permission-current").textContent = permissions[settings?.permission]?.[0] || "Доступ";
  $("context-toggle").disabled = !room;
  $("tools-toggle").disabled = !room || !connected;
  $("attach-files").disabled = sending.has(activeId);
  $("toggle-plan").disabled = sending.has(activeId);
  const planned = plannedRooms.has(activeId);
  $("plan-chip").hidden = !planned;
  $("plan-chip").disabled = sending.has(activeId);
  $("toggle-plan").setAttribute("aria-pressed", String(planned));
  renderContextPopover(room);
}

function contextPercent(usage) {
  return usage?.window > 0 && usage.used != null
    ? Math.min(100, Math.max(0, Math.round(usage.used / usage.window * 100))) : null;
}

function contextLine(agent, usage) {
  const percentage = contextPercent(usage);
  const first = node("div", "meter-caption");
  first.append(node("strong", "", names[agent]), node("span", "", percentage === null ? "—" : `${percentage}% занято`));
  const second = node("div", "meter-caption");
  const tokens = usage?.window > 0 ? `${contextSummary(usage)} ток.` : contextSummary(usage);
  second.append(node("span", "", usage?.used == null ? "Нет данных" : tokens),
    node("span", "", percentage === null ? "" : `${100 - percentage}% свободно`));
  const block = node("div", `context-line ${agent}`);
  block.append(first, meter(usage?.used, usage?.window, `Контекст ${names[agent]}`), second);
  return block;
}

function renderContextPopover(room) {
  const usage = room?.context?.[composerAgent];
  const percentage = contextPercent(usage);
  const ring = $("context-ring");
  ring.classList.toggle("known", percentage !== null);
  ring.style.setProperty("--used", `${percentage || 0}%`);
  $("context-toggle").title = `${names[composerAgent]} · ${percentage === null ? "Контекст" : `${percentage}% контекста`}`;
  if (composerPanel !== "context-toggle") return;
  $("composer-context").replaceChildren(contextLine(composerAgent, usage));
  $("context-agents").replaceChildren(...agents.map((agent) => contextLine(agent, room?.context?.[agent])));
}

function closeComposerPopover(restoreFocus = false) {
  if (!composerPanel) return;
  const opener = $(composerPanel);
  $(composerPanels[composerPanel]).hidden = true;
  opener.setAttribute("aria-expanded", "false");
  composerPanel = null;
  modelEdit = null;
  if (restoreFocus) opener.focus();
}

function showComposerPopover(key) {
  const togglingOff = composerPanel === key;
  closeComposerPopover(togglingOff);
  if (togglingOff) return;
  closeMentions();
  composerPanel = key;
  $(key).setAttribute("aria-expanded", "true");
  $(composerPanels[key]).hidden = false;
  if (key === "model-toggle") {
    $("model-search").value = "";
    loadModelEditor();
    $("model-search").focus();
  } else if (key === "context-toggle") {
    $("composer-context-details").open = false;
    renderContextPopover(rooms.get(activeId));
    $("composer-context-details").querySelector("summary").focus();
  } else {
    $("attach-files").focus();
  }
}

async function persistAgent(id, agent, settings) {
  const result = await request(roomPath(id, `/agents/${agent}`), "PUT", settings);
  const room = rooms.get(id);
  if (room) {
    room.agents = { ...room.agents, [agent]: result.agents[agent] };
    renderControls();
  }
}

async function loadModelEditor() {
  const room = rooms.get(activeId);
  if (!room?.agents?.[composerAgent]) return;
  const edit = { id: activeId, agent: composerAgent, ...room.agents[composerAgent], loading: true, saving: false };
  modelEdit = edit;
  showError("model-error", null);
  renderModelPicker();
  try {
    await loadModels();
  } catch {
    if (modelEdit === edit) showError("model-error", "Каталог моделей недоступен.");
  } finally {
    if (modelEdit === edit) {
      edit.loading = false;
      renderModelPicker();
    }
  }
}

function renderModelPicker() {
  const edit = modelEdit;
  if (!edit) return;
  const list = $("model-options");
  const levels = $("reasoning-options");
  const activeModel = modelEntry(edit.agent, edit.model);
  const entries = [...(modelCatalog?.[edit.agent] || [])];
  if (!entries.some((entry) => entry.id === "")) entries.unshift({ id: "", name: "По умолчанию" });
  if (edit.model && !entries.some((entry) => entry.id === edit.model)) entries.push({ id: edit.model, name: edit.model });
  const search = $("model-search").value.trim().toLowerCase();
  const busy = edit.loading || edit.saving || handoffPending;
  for (const tab of document.querySelectorAll("[data-picker-agent]")) {
    tab.setAttribute("aria-pressed", String(tab.dataset.pickerAgent === edit.agent));
    tab.disabled = edit.saving || handoffPending;
  }
  list.replaceChildren();
  const matches = entries.filter((entry) => `${entry.name} ${entry.id}`.toLowerCase().includes(search));
  if (!matches.length) list.append(node("p", "empty-panel", "Модели не найдены"));
  for (const entry of matches) {
    const item = node("button", "model-option");
    const caption = node("span", "model-option-caption");
    caption.append(node("strong", "", entry.name));
    if (entry.description) caption.append(node("small", "", entry.description));
    item.type = "button";
    item.disabled = busy;
    item.dataset.model = entry.id;
    item.setAttribute("aria-pressed", String(edit.model === entry.id));
    item.append(caption, node("span", "model-check", edit.model === entry.id ? "✓" : ""));
    item.addEventListener("click", () => {
      if (edit.model !== entry.id) {
        edit.model = entry.id;
        edit.effort = "";
        if (!entry.fast) edit.fast = false;
        renderModelPicker();
        [...list.children].find((button) => button.dataset.model === entry.id)?.focus();
      }
    });
    list.append(item);
  }
  levels.replaceChildren();
  const efforts = new Set(["", ...(activeModel?.efforts || [])]);
  if (edit.effort) efforts.add(edit.effort);
  for (const value of efforts) {
    const defaultName = effortLabels[activeModel?.default_effort] || activeModel?.default_effort;
    const label = value ? (effortLabels[value] || value) : (defaultName ? `Авто · ${defaultName}` : "Авто");
    const chip = node("button", "reason-chip", label);
    chip.type = "button";
    chip.disabled = busy;
    chip.dataset.effort = value;
    chip.setAttribute("aria-pressed", String(edit.effort === value));
    chip.addEventListener("click", () => {
      edit.effort = value;
      for (const button of levels.children) button.setAttribute("aria-pressed", String(button === chip));
    });
    levels.append(chip);
  }
  $("fast-option").hidden = !activeModel?.fast;
  $("fast-mode").disabled = busy;
  $("fast-mode").checked = Boolean(edit.fast && activeModel?.fast);
  $("apply-model").disabled = busy || !modelCatalog || !connected;
  $("handoff").disabled = busy || !connected;
}

async function saveModelEditor() {
  const edit = modelEdit;
  if (!edit || $("apply-model").disabled) return;
  const { id, agent, model, effort } = edit;
  const current = rooms.get(id)?.agents?.[agent];
  if (!current) return;
  edit.saving = true;
  renderModelPicker();
  showError("model-error", null);
  try {
    await persistAgent(id, agent, {
      ...current, model, effort, fast: Boolean(edit.fast && modelEntry(agent, model)?.fast)
    });
    if (modelEdit === edit) closeComposerPopover(true);
  } catch (error) {
    if (modelEdit === edit) showError("model-error", error);
  } finally {
    edit.saving = false;
    if (modelEdit === edit) renderModelPicker();
  }
}

async function handoffComposer() {
  if (!activeId || handoffPending || $("handoff").disabled) return;
  const id = activeId;
  handoffPending = true;
  renderModelPicker();
  showError("model-error", null);
  try {
    const room = await request(roomPath(id, "/handoff"), "POST", {});
    applyEvent({ type: "room_updated", room });
    if (activeId === id) {
      selectRoom(room.id);
      draft.focus();
    }
  } catch (error) {
    if (activeId === id) showError(composerPanel === "model-toggle" ? "model-error" : "inline-error", error);
  } finally {
    handoffPending = false;
    renderModelPicker();
  }
}

function fileSize(bytes) {
  if (bytes < 1024) return `${bytes} Б`;
  const unit = bytes < 1024 ** 2 ? [1024, "КиБ"] : [1024 ** 2, "МиБ"];
  return `${new Intl.NumberFormat("ru", { maximumFractionDigits: 1 }).format(bytes / unit[0])} ${unit[1]}`;
}

function attachmentChip(file, remove) {
  const chip = node("span", "attachment-chip");
  const name = node("span", "file-name", file.name);
  name.title = file.name;
  chip.append(node("span", "", "↗"), name, node("span", "file-size", fileSize(file.size)));
  if (remove) {
    const button = node("button", "", "×");
    button.type = "button";
    button.setAttribute("aria-label", `Убрать ${file.name}`);
    button.addEventListener("click", remove);
    chip.append(button);
  }
  return chip;
}

function renderDraftFiles() {
  const id = activeId;
  const container = $("draft-files");
  const files = pendingFiles.get(id) || [];
  container.replaceChildren();
  container.hidden = !files.length;
  for (const entry of files) {
    container.append(attachmentChip(entry.file, () => {
      if (sending.has(id)) return;
      pendingFiles.set(id, files.filter((item) => item !== entry));
      renderDraftFiles();
      renderControls();
    }));
  }
  for (const button of container.querySelectorAll("button")) button.disabled = sending.has(id);
}

async function uploadFiles(id, files) {
  const ids = [];
  for (const entry of files) {
    if (!entry.id) {
      const query = new URLSearchParams({ name: entry.file.name });
      const response = await request(roomPath(id, `/attachments?${query}`), "POST", entry.file, "application/octet-stream");
      entry.id = response.id;
    }
    ids.push(entry.id);
  }
  return ids;
}

function addPickedFiles(event) {
  const id = pickedForRoom;
  const files = [...event.target.files];
  event.target.value = "";
  pickedForRoom = null;
  if (!rooms.has(id) || sending.has(id)) return;
  const accepted = files.filter((file) => file.size <= fileLimit);
  const previous = pendingFiles.get(id) || [];
  pendingFiles.set(id, [...previous, ...accepted.map((file) => ({ file }))]);
  if (activeId === id) {
    const rejected = files.filter((file) => file.size > fileLimit);
    showError("inline-error", rejected.length ? `Файлы больше 25 МиБ: ${rejected.map((file) => file.name).join(", ")}` : null);
    renderDraftFiles();
    renderControls();
  }
}

function togglePlan() {
  if (!activeId || sending.has(activeId)) return;
  if (plannedRooms.has(activeId)) plannedRooms.delete(activeId);
  else plannedRooms.add(activeId);
  closeComposerPopover();
  renderControls();
  draft.focus();
}

async function renderPrompts() {
  const dialog = $("prompts-dialog");
  showError("prompt-error", null);
  const list = $("prompt-list");
  list.replaceChildren(node("p", "empty-panel", "Загружаем…"));
  try {
    const entries = await request("/api/prompts");
    if (!dialog.open) return;
    list.replaceChildren();
    if (!entries.length) list.append(node("p", "empty-panel", "Сохранённых промптов пока нет."));
    for (const entry of entries) {
      const row = node("div", "prompt-row");
      const insert = node("button", "prompt-insert", entry.title);
      const remove = node("button", "icon-button prompt-remove", "×");
      insert.title = entry.text;
      remove.setAttribute("aria-label", `Удалить промпт ${entry.title}`);
      insert.type = remove.type = "button";
      insert.addEventListener("click", () => {
        if (promptRoom !== activeId) return;
        const start = draft.selectionStart;
        const end = draft.selectionEnd;
        draft.setRangeText(entry.text, start, end, "end");
        drafts.set(activeId, draft.value);
        dialog.close();
        renderControls();
        draft.focus();
      });
      remove.addEventListener("click", () => confirmPromptRemoval(row, entry, remove));
      row.append(insert, remove);
      list.append(row);
    }
  } catch (error) {
    list.replaceChildren();
    showError("prompt-error", error);
  }
}

function confirmPromptRemoval(row, entry, remove) {
  if (row.querySelector(".prompt-confirm")) return;
  const confirm = node("button", "icon-button prompt-confirm", "Удалить");
  const cancel = node("button", "icon-button", "Отмена");
  confirm.type = cancel.type = "button";
  remove.hidden = true;
  row.append(confirm, cancel);
  cancel.focus();
  cancel.addEventListener("click", () => {
    confirm.remove();
    cancel.remove();
    remove.hidden = false;
    remove.focus();
  });
  confirm.addEventListener("click", async () => {
    if (promptsBusy) return;
    promptsBusy = confirm.disabled = cancel.disabled = true;
    try {
      await request(`/api/prompts/${encodeURIComponent(entry.id)}`, "DELETE");
      await renderPrompts();
    } catch (error) {
      showError("prompt-error", error);
    } finally {
      promptsBusy = confirm.disabled = cancel.disabled = false;
    }
  });
}

function initializeComposer() {
  for (const key of Object.keys(composerPanels)) $(key).addEventListener("click", () => showComposerPopover(key));
  $("permission-toggle").addEventListener("click", () => {
    closeComposerPopover();
    openAgentSettings($("permission-toggle"));
  });
  $("model-search").addEventListener("input", renderModelPicker);
  $("fast-mode").addEventListener("change", (event) => {
    if (modelEdit) modelEdit.fast = event.target.checked;
  });
  $("apply-model").addEventListener("click", () => void saveModelEditor());
  $("handoff").addEventListener("click", () => void handoffComposer());
  for (const button of document.querySelectorAll("[data-picker-agent]")) {
    button.addEventListener("click", () => {
      if (composerAgent === button.dataset.pickerAgent) return;
      composerAgent = button.dataset.pickerAgent;
      $("model-search").value = "";
      renderControls();
      void loadModelEditor();
    });
  }
  $("toggle-plan").addEventListener("click", togglePlan);
  $("plan-chip").addEventListener("click", togglePlan);
  $("attach-files").addEventListener("click", () => {
    pickedForRoom = activeId;
    closeComposerPopover(true);
    $("attachments-picker").click();
  });
  $("attachments-picker").addEventListener("change", addPickedFiles);
  $("open-prompts").addEventListener("click", () => {
    closeComposerPopover();
    promptRoom = activeId;
    const form = $("prompt-form");
    form.reset();
    $("save-prompt").disabled = !draft.value.trim();
    $("prompts-dialog").showModal();
    void renderPrompts();
  });
  $("close-prompts").addEventListener("click", () => $("prompts-dialog").close());
  $("prompt-form").addEventListener("submit", async (event) => {
    event.preventDefault();
    if (promptsBusy || promptRoom !== activeId || !draft.value.trim()) return;
    const submit = $("save-prompt");
    promptsBusy = submit.disabled = true;
    showError("prompt-error", null);
    try {
      await request("/api/prompts", "POST", { title: event.target.elements.title.value.trim(), text: draft.value.trim() });
      event.target.reset();
      await renderPrompts();
    } catch (error) {
      showError("prompt-error", error);
    } finally {
      promptsBusy = submit.disabled = false;
    }
  });
  document.addEventListener("pointerdown", (event) => {
    if (composerPanel && !($(composerPanel).contains(event.target) || $(composerPanels[composerPanel]).contains(event.target))) {
      closeComposerPopover();
    }
  });
  document.addEventListener("keydown", (event) => {
    if (event.key === "Escape" && composerPanel) {
      closeComposerPopover(true);
      event.preventDefault();
    }
  });
}
