// KlSM Bundler UI: one profile per client (clients/<name>.toml); toggles save immediately.
const { invoke } = window.__TAURI__.core;
const $ = (id) => document.getElementById(id);

const store = {
  get: (k) => { try { return localStorage.getItem(k); } catch { return null; } },
  set: (k, v) => { try { localStorage.setItem(k, v); } catch {} },
};

let config = null;   // bundle.toml path
let project = null;  // { root, features: [{ name, sides }], clients: [name] }
let client = null;   // selected client name
let disabled = new Set();

function toast(message, kind = "error") {
  const el = document.createElement("div");
  el.className = "toast";
  el.style.borderColor = kind === "error" ? "rgba(255,77,94,.4)" : "";
  el.textContent = message;
  $("toasts").append(el);
  setTimeout(() => el.remove(), 5000);
}

async function call(cmd, args) {
  try {
    return await invoke(cmd, args);
  } catch (e) {
    toast(String(e));
    throw e;
  }
}

function showEmpty(title, body) {
  $("emptyTitle").textContent = title;
  $("emptyBody").textContent = body;
  $("emptyState").hidden = false;
  $("clientView").hidden = true;
  $("logPanel").hidden = true;
}

async function openProject(path) {
  config = path.trim();
  $("configPath").value = config;
  project = await call("open_project", { config });
  store.set("config", config);
  renderClients();
  const remembered = store.get(`client:${config}`);
  const first = project.clients.includes(remembered) ? remembered : project.clients[0];
  if (first) {
    await selectClient(first);
  } else {
    showEmpty("Add your first client", "Name it on the left. Every feature starts enabled; switch off what they don't get.");
  }
}

function renderClients() {
  const nav = $("clients");
  nav.replaceChildren(
    ...project.clients.map((name) => {
      const b = document.createElement("button");
      b.type = "button";
      b.textContent = name;
      b.setAttribute("aria-current", String(name === client));
      b.onclick = () => selectClient(name);
      return b;
    }),
  );
}

async function selectClient(name) {
  const profile = await call("get_client", { config, name });
  client = name;
  disabled = new Set(profile.disabled);
  store.set(`client:${config}`, name);
  $("emptyState").hidden = true;
  $("clientView").hidden = false;
  $("logPanel").hidden = true;
  $("clientName").textContent = name;
  renderClients();
  renderFeatures();
}

function renderSummary() {
  const total = project.features.length;
  const on = project.features.filter((f) => !disabled.has(f.name)).length;
  $("clientSummary").textContent = `${on} of ${total} features ship to this client.`;
}

function renderFeatures() {
  $("features").replaceChildren(
    ...project.features.map((f) => {
      const card = document.createElement("label");
      card.className = "card feature";
      card.classList.toggle("is-off", disabled.has(f.name));

      const info = document.createElement("div");
      const name = document.createElement("div");
      name.className = "feature__name";
      name.textContent = f.name;
      const sides = document.createElement("div");
      sides.className = "feature__sides";
      for (const side of f.sides) {
        const tag = document.createElement("span");
        tag.className = "tag tag--neutral";
        tag.textContent = side || "feature";
        sides.append(tag);
      }
      info.append(name, sides);

      const toggle = document.createElement("span");
      toggle.className = "toggle";
      const input = document.createElement("input");
      input.type = "checkbox";
      input.checked = !disabled.has(f.name);
      input.setAttribute("aria-label", `Ship ${f.name}`);
      input.onchange = () => setFeature(f.name, input.checked, card);
      const track = document.createElement("span");
      track.className = "toggle__track";
      toggle.append(input, track);

      card.append(info, toggle);
      return card;
    }),
  );
  renderSummary();
}

async function save() {
  await call("save_client", { config, name: client, disabled: [...disabled] });
}

async function setFeature(name, on, card) {
  if (on) disabled.delete(name);
  else disabled.add(name);
  card.classList.toggle("is-off", !on);
  renderSummary();
  await save();
}

function classify(line) {
  if (line.startsWith("✓")) return "ok";
  if (line.startsWith("✗") || line.startsWith("error")) return "err";
  if (line.startsWith("warning")) return "warn";
  return "";
}

async function build() {
  const btn = $("buildBtn");
  btn.classList.add("is-loading");
  btn.disabled = true;
  try {
    const result = await call("build", { config, client });
    renderLog(result);
  } finally {
    btn.classList.remove("is-loading");
    btn.disabled = false;
  }
}

function renderLog({ ok, log, package: pkg }) {
  $("logPanel").hidden = false;
  const status = $("logStatus");
  status.textContent = ok ? `Built for ${client}` : `Build failed for ${client}`;
  status.style.setProperty("--c", ok ? "var(--us-success)" : "var(--us-error)");

  $("logBody").replaceChildren(
    ...log.map((l) => {
      const span = document.createElement("span");
      span.className = classify(l);
      span.textContent = l + "\n";
      return span;
    }),
  );

  const next = $("leftovers");
  next.replaceChildren();
  if (ok && pkg) {
    const alert = document.createElement("div");
    alert.className = "alert alert--success";
    const body = document.createElement("div");
    const strong = document.createElement("strong");
    strong.textContent = `${pkg.split("/").pop()} is ready`;
    const p = document.createElement("span");
    p.textContent =
      "Drop it into ServerScriptService of the client's place (replace the old package). On start it installs ReplicatedStorage and removes switched-off features by itself.";
    body.append(strong, p);
    alert.append(body);
    next.append(alert);
  }

  const reveal = $("reveal");
  reveal.hidden = !pkg;
  reveal.onclick = () => call("reveal", { path: pkg });
  $("logPanel").scrollIntoView({ behavior: "smooth", block: "nearest" });
}

$("projectForm").onsubmit = (e) => {
  e.preventDefault();
  if ($("configPath").value.trim()) openProject($("configPath").value);
};

$("newClient").onsubmit = async (e) => {
  e.preventDefault();
  if (!project) return toast("Open a bundle.toml first.");
  const name = $("newName").value.trim();
  if (!name) return;
  if (!project.clients.includes(name)) {
    await call("save_client", { config, name, disabled: [] });
    project.clients = [...project.clients, name].sort();
  }
  $("newName").value = "";
  await selectClient(name);
};

$("allOn").onclick = async () => {
  disabled.clear();
  renderFeatures();
  await save();
};

$("buildBtn").onclick = build;

(async () => {
  const path = store.get("config") || (await invoke("default_config"));
  if (path) await openProject(path).catch(() => showEmpty("Open a bundle.toml", "Couldn't open the last one. Paste a path above."));
})();
