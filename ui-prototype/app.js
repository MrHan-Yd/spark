const DATA = [
  { id: 1, title: "Windows Terminal", sub: "应用程序", source: "应用", kind: "app", icon: "Wt", keys: ["wt", "terminal", "终端"] },
  { id: 2, title: "Visual Studio Code", sub: "最近 · 3 分钟前", source: "历史", kind: "history", icon: "Vs", keys: ["code", "vscode"] },
  { id: 3, title: "Google Chrome", sub: "应用程序", source: "应用", kind: "app", icon: "Ch", keys: ["chrome", "浏览器"] },
  { id: 4, title: "计算 128 * 32", sub: "= 4096  · Enter 复制", source: "工具", kind: "calc", icon: "=", keys: ["128", "算"] },
  { id: 5, title: "Echo hello", sub: "插件命令", source: "Echo", kind: "plugin", icon: "Ec", keys: ["echo", "hello"] },
  { id: 6, title: "项目 README.md", sub: "D:\\demo\\test01\\docs", source: "文件", kind: "file", icon: "Md", keys: ["readme", "docs"] },
  { id: 7, title: "设置", sub: "打开启动器设置", source: "系统", kind: "app", icon: "Se", keys: ["settings", "设置", "prefs"] },
  { id: 8, title: "g rust async", sub: "Google 搜索", source: "快搜", kind: "plugin", icon: "G", keys: ["g ", "google", "rust"] },
  { id: 9, title: "文件资源管理器", sub: "应用程序", source: "应用", kind: "app", icon: "Ex", keys: ["explorer", "资源"] },
  { id: 10, title: "JSON 格式化", sub: "剪贴板 · 需权限", source: "插件", kind: "plugin", icon: "{}", keys: ["json", "格式"] },
  { id: 11, title: "锁定工作站", sub: "系统操作 · 需确认", source: "系统", kind: "app", icon: "Lk", keys: ["lock", "锁屏"] },
  { id: 12, title: "ARCHITECTURE.md", sub: "docs · 今天", source: "文件", kind: "file", icon: "Ar", keys: ["arch", "架构"] },
];

const GRID_COLS = 4;

const DEFAULT_FAV = {
  groups: [
    { id: "all", name: "全部" },
    { id: "work", name: "工作" },
    { id: "dev", name: "开发" },
    { id: "daily", name: "日常" },
  ],
  // itemId -> groupId（all 表示未分组也在「全部」里显示）
  items: [
    { itemId: 1, groupId: "dev" },   // Terminal
    { itemId: 2, groupId: "dev" },   // VS Code
    { itemId: 3, groupId: "daily" }, // Chrome
    { itemId: 9, groupId: "work" },  // Explorer
    { itemId: 10, groupId: "dev" },  // JSON
  ],
  activeGroup: "all",
};

function loadFav() {
  try {
    const raw = localStorage.getItem("launcher-fav");
    if (raw) return { ...DEFAULT_FAV, ...JSON.parse(raw) };
  } catch (_) {}
  return structuredClone(DEFAULT_FAV);
}

function saveFav() {
  localStorage.setItem(
    "launcher-fav",
    JSON.stringify({
      groups: state.fav.groups,
      items: state.fav.items,
      activeGroup: state.fav.activeGroup,
    })
  );
}

const $ = (s, el = document) => el.querySelector(s);
const $$ = (s, el = document) => [...el.querySelectorAll(s)];

const state = {
  active: 0,
  items: [],
  open: true,
  mode: "search", // search | settings —— 同一主窗口内切换
  view: localStorage.getItem("launcher-view") || "list", // list | grid
  fav: loadFav(),
  favExpanded: localStorage.getItem("launcher-fav-expanded") !== "0",
};

function highlight(text, q) {
  if (!q) return text;
  const i = text.toLowerCase().indexOf(q.toLowerCase());
  if (i < 0) return text;
  return (
    text.slice(0, i) +
    "<mark>" +
    text.slice(i, i + q.length) +
    "</mark>" +
    text.slice(i + q.length)
  );
}

function filter(q) {
  const t = q.trim().toLowerCase();
  if (!t) return DATA.slice(0, 8);
  return DATA.filter(
    (x) =>
      x.title.toLowerCase().includes(t) ||
      x.sub.toLowerCase().includes(t) ||
      x.keys.some((k) => k.includes(t) || t.includes(k))
  );
}

let viewSwitching = false;

function prefersReducedMotion() {
  return document.documentElement.classList.contains("reduce-motion") ||
    window.matchMedia("(prefers-reduced-motion: reduce)").matches;
}

function clearResultsHeight(box) {
  if (!box) return;
  box.style.height = "";
  box.classList.remove("height-animating");
}

/** 将 results 从当前像素高度平滑过渡到内容自然高度 */
function animateResultsHeight(box, { from } = {}) {
  if (!box || prefersReducedMotion()) {
    clearResultsHeight(box);
    return;
  }

  const start = from != null ? from : box.getBoundingClientRect().height;
  // 清掉固定高度以测量目标
  box.style.height = "auto";
  box.style.transition = "none";
  const end = box.getBoundingClientRect().height;
  // 锁回起点
  box.style.height = `${start}px`;
  void box.offsetHeight; // reflow
  box.classList.add("height-animating");
  box.style.transition = "";
  box.style.height = `${end}px`;

  let done = false;
  const finish = () => {
    if (done) return;
    done = true;
    // 结束后保持 auto，方便后续搜索改内容高度
    clearResultsHeight(box);
  };

  box.addEventListener(
    "transitionend",
    (e) => {
      if (e.target === box && e.propertyName === "height") finish();
    },
    { once: true }
  );
  setTimeout(finish, 400);
}

function setView(view, { animate = true } = {}) {
  const next = view === "grid" ? "grid" : "list";
  const same = next === state.view;

  // 用户重复点同一视图：只同步按钮
  if (same && animate && !viewSwitching) {
    syncViewChrome();
    return;
  }

  const run = (opts = {}) => {
    const box = $("#results");
    const fromH = opts.fromHeight;

    state.view = next;
    localStorage.setItem("launcher-view", state.view);
    box.classList.toggle("results-list", state.view === "list");
    box.classList.toggle("results-grid", state.view === "grid");
    syncViewChrome();
    render({
      enterAnim: animate && !same && !prefersReducedMotion(),
      // 内容已换时再做高度动画
      heightFrom: animate && !same && !prefersReducedMotion() ? fromH : null,
    });
    viewSwitching = false;
  };

  // 初始化 / 减少动画 / 同视图强制刷新
  if (!animate || prefersReducedMotion() || same) {
    run();
    clearResultsHeight($("#results"));
    return;
  }

  if (viewSwitching) return;
  viewSwitching = true;
  const box = $("#results");
  const fromHeight = box.getBoundingClientRect().height;

  // 先锁高度，避免 leave 阶段窗口塌陷
  box.style.height = `${fromHeight}px`;
  box.classList.add("height-animating");
  box.classList.remove("is-entering");
  box.classList.add("is-leaving");

  let finished = false;
  const done = () => {
    if (finished) return;
    finished = true;
    box.classList.remove("is-leaving");
    run({ fromHeight });
  };

  box.addEventListener("transitionend", function onEnd(e) {
    if (e.target !== box || e.propertyName !== "opacity") return;
    box.removeEventListener("transitionend", onEnd);
    done();
  });
  setTimeout(done, 280);
}

function syncViewChrome() {
  $$(".view-btn").forEach((btn) => {
    const on = btn.dataset.view === state.view;
    btn.classList.toggle("active", on);
    btn.setAttribute("aria-pressed", on ? "true" : "false");
  });
  const sel = $("#defaultView");
  if (sel) sel.value = state.view;
}

function render({ enterAnim = false, heightFrom = null } = {}) {
  const q = $("#query").value;
  state.items = filter(q);
  if (state.active >= state.items.length) state.active = Math.max(0, state.items.length - 1);

  const list = $("#results");
  list.classList.remove("is-leaving");
  $("#searchMeta").textContent = state.items.length ? `${state.items.length} 项` : "";

  if (!state.items.length) {
    list.innerHTML = `<div class="empty">未找到相关结果<br/><span style="opacity:.7;font-size:12px">尝试应用名、前缀命令或插件关键字</span></div>`;
    $("#footerSource").textContent = "无匹配";
    if (enterAnim) playEnter(list);
    if (heightFrom != null) {
      requestAnimationFrame(() => animateResultsHeight(list, { from: heightFrom }));
    }
    return;
  }

  const isGrid = state.view === "grid";

  list.innerHTML = state.items
    .map((item, i) => {
      const active = i === state.active ? "active" : "";
      if (isGrid) {
        return `
        <div class="result-item ${active}" role="option" data-idx="${i}" aria-selected="${!!active}">
          <div class="result-icon ${item.kind}">${item.icon}</div>
          <div class="result-body">
            <div class="result-title">${highlight(item.title, q)}</div>
          </div>
          <div class="result-source">${item.source}</div>
        </div>`;
      }
      return `
      <div class="result-item ${active}" role="option" data-idx="${i}" aria-selected="${!!active}">
        <div class="result-icon ${item.kind}">${item.icon}</div>
        <div class="result-body">
          <div class="result-title">${highlight(item.title, q)}</div>
          <div class="result-sub">${item.sub}</div>
        </div>
        <div class="result-source">${item.source}</div>
        <div class="result-kbd">${i < 9 ? `${i + 1}` : ""}</div>
      </div>`;
    })
    .join("");

  const cur = state.items[state.active];
  $("#footerSource").textContent = cur
    ? `${cur.source} · ${isGrid ? "平铺" : "列表"}`
    : "本地 · 极速";

  const activeEl = list.querySelector(".result-item.active");
  if (activeEl && heightFrom == null) activeEl.scrollIntoView({ block: "nearest" });

  $$(".result-item", list).forEach((el) => {
    el.addEventListener("mouseenter", () => {
      state.active = +el.dataset.idx;
      $$(".result-item", list).forEach((n, i) => {
        n.classList.toggle("active", i === state.active);
        n.setAttribute("aria-selected", i === state.active ? "true" : "false");
      });
      const c = state.items[state.active];
      if (c) $("#footerSource").textContent = `${c.source} · ${isGrid ? "平铺" : "列表"}`;
    });
    el.addEventListener("click", () => {
      state.active = +el.dataset.idx;
      invoke();
    });
  });

  if (enterAnim) playEnter(list);

  // 列表 ↔ 平铺：高度从旧值过渡到新内容高度（整窗跟着变）
  if (heightFrom != null) {
    requestAnimationFrame(() => animateResultsHeight(list, { from: heightFrom }));
  }

  const favEl = $("#favorites");
  if (favEl) {
    favEl.classList.toggle("dimmed", q.trim().length > 0);
    favEl.classList.remove("hidden");
  }
  renderFavorites({ animate: false });
}

function playEnter(el) {
  el.classList.remove("is-entering");
  // 强制重绘以重触发 animation
  void el.offsetWidth;
  el.classList.add("is-entering");
  const clear = () => el.classList.remove("is-entering");
  el.addEventListener("animationend", clear, { once: true });
  setTimeout(clear, 450);
}

function byId(id) {
  return DATA.find((x) => x.id === id);
}

function shortName(title) {
  if (title.length <= 6) return title;
  return title.replace(/^计算\s+/, "").slice(0, 6);
}

function favEntriesForGroup(groupId) {
  const rows = state.fav.items;
  if (groupId === "all") return rows;
  return rows.filter((r) => r.groupId === groupId);
}

function fillFavItems(box, gid) {
  const entries = favEntriesForGroup(gid);
  if (!entries.length) {
    box.innerHTML = `
      <div class="fav-empty">
        此分组暂无收藏
        <button type="button" id="favHintPin">从结果 Tab → 固定</button>
      </div>`;
    const hint = $("#favHintPin");
    if (hint) {
      hint.addEventListener("click", () => {
        $("#actionSheet").classList.remove("hidden");
        $("#footerSource").textContent = "选择「固定到收藏」";
      });
    }
    return;
  }

  box.innerHTML = entries
    .map((row) => {
      const item = byId(row.itemId);
      if (!item) return "";
      const gname = state.fav.groups.find((g) => g.id === row.groupId)?.name || "";
      return `
        <button type="button" class="fav-item" data-item-id="${item.id}" title="${item.title} · ${gname}">
          <div class="result-icon ${item.kind}">${item.icon}</div>
          <span class="fav-item-name">${shortName(item.title)}</span>
        </button>`;
    })
    .join("");

  $$(".fav-item", box).forEach((el) => {
    el.addEventListener("click", (e) => {
      e.stopPropagation();
      const id = +el.dataset.itemId;
      const item = byId(id);
      if (!item) return;
      if (item.id === 7) {
        openSettings();
        return;
      }
      flash(item.title);
    });
  });
}

function setFavExpanded(expanded, { persist = true } = {}) {
  state.favExpanded = !!expanded;
  if (persist) localStorage.setItem("launcher-fav-expanded", state.favExpanded ? "1" : "0");

  const root = $("#favorites");
  const btn = $("#favToggle");
  if (root) root.classList.toggle("is-collapsed", !state.favExpanded);
  if (btn) {
    btn.setAttribute("aria-expanded", state.favExpanded ? "true" : "false");
    btn.title = state.favExpanded ? "收起收藏" : "展开收藏";
  }
  updateFavCount();
}

function toggleFavExpanded() {
  setFavExpanded(!state.favExpanded);
}

function updateFavCount() {
  const el = $("#favCount");
  if (!el) return;
  const n = state.fav.items.length;
  el.textContent = n ? `(${n})` : "";
}

function renderFavorites({ animate = false } = {}) {
  const tabs = $("#favGroups");
  const box = $("#favItems");
  if (!tabs || !box) return;

  updateFavCount();
  setFavExpanded(state.favExpanded, { persist: false });

  const gid = state.fav.activeGroup;
  tabs.innerHTML = state.fav.groups
    .map(
      (g) =>
        `<button type="button" class="fav-group-tab ${g.id === gid ? "active" : ""}" data-group="${g.id}" role="tab" aria-selected="${g.id === gid}">${g.name}</button>`
    )
    .join("");

  $$(".fav-group-tab", tabs).forEach((btn) => {
    btn.addEventListener("click", (e) => {
      e.stopPropagation();
      if (!state.favExpanded) setFavExpanded(true);
      if (btn.dataset.group === state.fav.activeGroup) return;
      state.fav.activeGroup = btn.dataset.group;
      saveFav();
      renderFavorites({ animate: !prefersReducedMotion() });
    });
  });

  const apply = () => {
    box.classList.remove("is-leaving");
    fillFavItems(box, gid);
    if (animate) {
      box.classList.remove("is-entering");
      void box.offsetWidth;
      box.classList.add("is-entering");
      const clear = () => box.classList.remove("is-entering");
      box.addEventListener("animationend", clear, { once: true });
      setTimeout(clear, 400);
    }
  };

  if (animate) {
    box.classList.add("is-leaving");
    setTimeout(apply, 160);
  } else {
    apply();
  }
}

function pinToGroup(groupId) {
  const item = state.items[state.active];
  if (!item) {
    $("#footerSource").textContent = "请先选中一条结果";
    return;
  }
  const g =
    groupId && state.fav.groups.some((x) => x.id === groupId)
      ? groupId
      : state.fav.activeGroup === "all"
        ? "work"
        : state.fav.activeGroup;

  const exists = state.fav.items.find((r) => r.itemId === item.id);
  if (exists) {
    exists.groupId = g;
  } else {
    state.fav.items.push({ itemId: item.id, groupId: g });
  }
  state.fav.activeGroup = g;
  saveFav();
  setFavExpanded(true);
  renderFavorites();
  $("#actionSheet").classList.add("hidden");
  const name = state.fav.groups.find((x) => x.id === g)?.name || g;
  $("#footerSource").textContent = `已固定到「${name}」`;
}

function addFavGroup() {
  const name = prompt("新分组名称", "新分组");
  if (!name || !name.trim()) return;
  const id = "g_" + Date.now().toString(36);
  state.fav.groups.push({ id, name: name.trim().slice(0, 8) });
  state.fav.activeGroup = id;
  saveFav();
  renderFavorites();
}

function moveActive(delta) {
  if (!state.items.length) return;
  if (state.view === "grid") {
    const cols = GRID_COLS;
    const i = state.active;
    let next = i + delta;
    if (delta === 1 || delta === -1) {
      next = i + delta;
    } else if (delta === cols || delta === -cols) {
      next = i + delta;
    }
    if (next < 0 || next >= state.items.length) return;
    state.active = next;
  } else {
    state.active = Math.max(0, Math.min(state.items.length - 1, state.active + delta));
  }
  render();
}

function invoke() {
  const item = state.items[state.active];
  if (!item) return;
  if (item.title === "设置" || item.id === 7) {
    openSettings();
    return;
  }
  flash(item.title);
}

function flash(msg) {
  const meta = $("#footerSource");
  meta.textContent = `已执行：${msg}`;
  setTimeout(() => {
    hideLauncher();
  }, 450);
}

function showLauncher() {
  const el = $("#launcher");
  el.classList.remove("hidden", "closing");
  state.open = true;
  $("#query").focus();
  render();
}

function hideLauncher() {
  const el = $("#launcher");
  el.classList.add("closing");
  setTimeout(() => {
    el.classList.add("hidden");
    el.classList.remove("closing");
    state.open = false;
    $("#actionSheet").classList.add("hidden");
    // 隐藏后立刻回到搜索（无动画），下次唤起是搜索
    if (state.mode === "settings") {
      switchMode("search", { animate: false });
    }
  }, 150);
}

function toggleLauncher() {
  if (state.open) hideLauncher();
  else showLauncher();
}

let modeSwitching = false;

function animateGlassHeight(fromH) {
  const glass = $(".launcher-glass");
  if (!glass || prefersReducedMotion()) {
    if (glass) {
      glass.style.height = "";
      glass.classList.remove("height-animating");
    }
    return;
  }
  const start = fromH != null ? fromH : glass.getBoundingClientRect().height;
  glass.style.height = "auto";
  glass.style.transition = "none";
  const end = glass.getBoundingClientRect().height;
  glass.style.height = `${start}px`;
  void glass.offsetHeight;
  glass.classList.add("height-animating");
  glass.style.transition = "";
  glass.style.height = `${end}px`;

  let done = false;
  const finish = () => {
    if (done) return;
    done = true;
    glass.style.height = "";
    glass.classList.remove("height-animating");
  };
  glass.addEventListener(
    "transitionend",
    (e) => {
      if (e.target === glass && e.propertyName === "height") finish();
    },
    { once: true }
  );
  setTimeout(finish, 420);
}

function switchMode(to, { animate = true } = {}) {
  if (modeSwitching) return;
  if (state.mode === to) return;

  const fromEl = to === "settings" ? $("#modeSearch") : $("#modeSettings");
  const toEl = to === "settings" ? $("#modeSettings") : $("#modeSearch");
  const glass = $(".launcher-glass");
  const fromH = glass?.getBoundingClientRect().height;

  $("#actionSheet")?.classList.add("hidden");

  const showTo = () => {
    fromEl.classList.add("hidden");
    fromEl.classList.remove("is-leaving");
    toEl.classList.remove("hidden");
    state.mode = to;
    modeSwitching = false;

    if (animate && !prefersReducedMotion()) {
      toEl.classList.remove("is-entering");
      void toEl.offsetWidth;
      toEl.classList.add("is-entering");
      const clear = () => toEl.classList.remove("is-entering");
      toEl.addEventListener("animationend", clear, { once: true });
      setTimeout(clear, 400);
      requestAnimationFrame(() => animateGlassHeight(fromH));
    } else if (glass) {
      glass.style.height = "";
      glass.classList.remove("height-animating");
    }

    if (to === "search") $("#query")?.focus();
  };

  if (!animate || prefersReducedMotion()) {
    fromEl.classList.add("hidden");
    fromEl.classList.remove("is-leaving", "is-entering");
    toEl.classList.remove("hidden", "is-leaving", "is-entering");
    state.mode = to;
    if (to === "search") $("#query")?.focus();
    return;
  }

  modeSwitching = true;
  // 锁住当前高度，避免 leave 时塌陷
  if (glass && fromH) {
    glass.style.height = `${fromH}px`;
    glass.classList.add("height-animating");
  }

  fromEl.classList.remove("is-entering");
  fromEl.classList.add("is-leaving");

  let finished = false;
  const done = () => {
    if (finished) return;
    finished = true;
    showTo();
  };

  fromEl.addEventListener("animationend", done, { once: true });
  setTimeout(done, 260);
}

function openSettings() {
  switchMode("settings");
}

function closeSettings() {
  switchMode("search");
}

// Input
$("#query").addEventListener("input", () => {
  state.active = 0;
  render();
});

// View toggle buttons
$$(".view-btn").forEach((btn) => {
  btn.addEventListener("click", (e) => {
    e.preventDefault();
    e.stopPropagation();
    setView(btn.dataset.view);
    $("#query").focus();
  });
});

// Favorites
$("#favToggle")?.addEventListener("click", (e) => {
  e.stopPropagation();
  toggleFavExpanded();
});

$("#favAddGroup")?.addEventListener("click", (e) => {
  e.stopPropagation();
  if (!state.favExpanded) setFavExpanded(true);
  addFavGroup();
});

// Action sheet: pin
$$("[data-action]").forEach((btn) => {
  btn.addEventListener("click", (e) => {
    e.stopPropagation();
    const a = btn.dataset.action;
    if (a === "pin") pinToGroup(state.fav.activeGroup === "all" ? "work" : state.fav.activeGroup);
    else if (a === "pin-dev") pinToGroup("dev");
    else if (a === "open") invoke();
    else {
      $("#actionSheet").classList.add("hidden");
      $("#footerSource").textContent = `动作：${btn.textContent.trim()}`;
    }
  });
});

// Keyboard
document.addEventListener("keydown", (e) => {
  // Alt+Space / Ctrl+Space
  if (e.code === "Space" && (e.altKey || e.ctrlKey)) {
    e.preventDefault();
    toggleLauncher();
    return;
  }

  // Ctrl+L 列表 / Ctrl+G 平铺 / Ctrl+B 收藏展开收起
  if (state.open && e.ctrlKey && !e.altKey && (e.key === "l" || e.key === "L")) {
    e.preventDefault();
    setView("list");
    return;
  }
  if (state.open && e.ctrlKey && !e.altKey && (e.key === "g" || e.key === "G")) {
    e.preventDefault();
    setView("grid");
    return;
  }
  if (state.open && e.ctrlKey && !e.altKey && (e.key === "b" || e.key === "B")) {
    e.preventDefault();
    toggleFavExpanded();
    return;
  }

  if (!state.open && e.key !== "Escape") return;

  // Ctrl+, 打开设置
  if (state.open && e.ctrlKey && e.key === ",") {
    e.preventDefault();
    if (state.mode === "settings") closeSettings();
    else openSettings();
    return;
  }

  if (e.key === "Escape") {
    e.preventDefault();
    if (state.mode === "settings") {
      closeSettings();
      return;
    }
    if (!$("#actionSheet").classList.contains("hidden")) {
      $("#actionSheet").classList.add("hidden");
      return;
    }
    if ($("#query").value) {
      $("#query").value = "";
      state.active = 0;
      render();
    } else {
      hideLauncher();
    }
    return;
  }

  if (state.mode === "settings") return;

  if (state.view === "grid") {
    if (e.key === "ArrowRight") {
      e.preventDefault();
      moveActive(1);
    } else if (e.key === "ArrowLeft") {
      e.preventDefault();
      moveActive(-1);
    } else if (e.key === "ArrowDown") {
      e.preventDefault();
      moveActive(GRID_COLS);
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      moveActive(-GRID_COLS);
    } else if (e.key === "Enter") {
      e.preventDefault();
      invoke();
    } else if (e.key === "Tab") {
      e.preventDefault();
      $("#actionSheet").classList.toggle("hidden");
    }
  } else {
    if (e.key === "ArrowDown") {
      e.preventDefault();
      moveActive(1);
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      moveActive(-1);
    } else if (e.key === "Enter") {
      e.preventDefault();
      invoke();
    } else if (e.key === "Tab") {
      e.preventDefault();
      $("#actionSheet").classList.toggle("hidden");
    }
  }

  if (e.ctrlKey && e.key >= "1" && e.key <= "9") {
    const idx = +e.key - 1;
    if (state.items[idx]) {
      state.active = idx;
      invoke();
    }
  }
});

// Click outside → 隐藏主窗（正式版也是失焦隐藏）
$("#desktop").addEventListener("mousedown", (e) => {
  if (e.target.closest(".launcher, .action-sheet")) return;
  if (state.open) hideLauncher();
});

$("#openSettings")?.addEventListener("click", (e) => {
  e.stopPropagation();
  openSettings();
});

$("#closeSettings")?.addEventListener("click", (e) => {
  e.stopPropagation();
  closeSettings();
});

$$(".nav-item").forEach((btn) => {
  btn.addEventListener("click", () => {
    $$(".nav-item").forEach((b) => b.classList.remove("active"));
    btn.classList.add("active");
    $$(".pane").forEach((p) => p.classList.remove("active"));
    $(`#pane-${btn.dataset.pane}`).classList.add("active");
  });
});

$$(".preset").forEach((btn) => {
  btn.addEventListener("click", () => {
    $$(".preset").forEach((b) => b.classList.remove("active"));
    btn.classList.add("active");
    $(".desktop-hint").innerHTML = `<kbd>${btn.dataset.hk.replace("+", "</kbd> + <kbd>")}</kbd> 唤起 · 列表/平铺可切换 · 主题可切换`;
  });
});

$("#themeSelect").addEventListener("change", (e) => {
  const v = e.target.value;
  if (v === "system") {
    const dark = window.matchMedia("(prefers-color-scheme: dark)").matches;
    document.documentElement.dataset.theme = dark ? "dark" : "light";
  } else {
    document.documentElement.dataset.theme = v;
  }
});

$("#defaultView").addEventListener("change", (e) => {
  setView(e.target.value);
});

$("#widthRange").addEventListener("input", (e) => {
  document.documentElement.style.setProperty("--launcher-width", `${e.target.value}px`);
});

$("#reduceMotion").addEventListener("change", (e) => {
  document.documentElement.classList.toggle("reduce-motion", e.target.checked);
});

// Init
document.documentElement.dataset.theme = "dark";
$(".desktop-hint").innerHTML =
  `<kbd>Alt</kbd> + <kbd>Space</kbd> 唤起 · 列表/平铺切换 · 底部收藏分组`;
setView(state.view, { animate: false });
setFavExpanded(state.favExpanded, { persist: false });
renderFavorites({ animate: false });
$("#query").focus();
