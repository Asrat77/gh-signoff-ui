(() => {
  "use strict";

  const TOKEN = window.SIGNOFF.token;
  const READ_ONLY = window.SIGNOFF.readOnly;
  const desktop = window.signoffDesktop || null;
  const params = new URLSearchParams(location.search);
  const surface = params.get("surface") || "popover";
  document.body.dataset.surface = surface;

  const app = document.querySelector("#app");
  const paletteOverlay = document.querySelector("#paletteOverlay");
  const paletteInput = document.querySelector("#paletteInput");
  const paletteResults = document.querySelector("#paletteResults");
  const toast = document.querySelector("#toast");

  let product = null;
  let loading = true;
  let loadError = "";
  let selectedPRID = null;
  let filter = "attention";
  let settingsTab = params.get("tab") || "repositories";
  let selectedSettingsRepoID = null;
  let terminal = { open: false, running: false, phase: "", lines: [], prID: "", check: "" };
  let paletteActions = [];
  let paletteIndex = 0;
  let pollTimer = null;
  let terminalRenderQueued = false;

  const esc = (value) => String(value ?? "").replace(/[&<>"']/g, (char) => ({
    "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;",
  })[char]);

  function icon(name, cls = "") {
    const paths = {
      brand: '<path d="M7 4v7a4 4 0 0 0 4 4h2"/><circle cx="7" cy="4" r="2"/><circle cx="15" cy="15" r="2"/><path d="M15 13V8"/><circle cx="15" cy="6" r="2"/>',
      refresh: '<path d="M20 11a8.1 8.1 0 0 0-15.5-2M4 4v5h5"/><path d="M4 13a8.1 8.1 0 0 0 15.5 2M20 20v-5h-5"/>',
      settings: '<circle cx="12" cy="12" r="3"/><path d="M19.4 15a1.7 1.7 0 0 0 .3 1.9l.1.1-2.8 2.8-.1-.1a1.7 1.7 0 0 0-1.9-.3 1.7 1.7 0 0 0-1 1.6v.2h-4V21a1.7 1.7 0 0 0-1-1.6 1.7 1.7 0 0 0-1.9.3l-.1.1L4.2 17l.1-.1a1.7 1.7 0 0 0 .3-1.9A1.7 1.7 0 0 0 3 14H3v-4h.1a1.7 1.7 0 0 0 1.5-1 1.7 1.7 0 0 0-.3-1.9L4.2 7 7 4.2l.1.1A1.7 1.7 0 0 0 9 4.6 1.7 1.7 0 0 0 10 3V3h4v.1a1.7 1.7 0 0 0 1 1.5 1.7 1.7 0 0 0 1.9-.3l.1-.1L19.8 7l-.1.1a1.7 1.7 0 0 0-.3 1.9 1.7 1.7 0 0 0 1.6 1h.2v4H21a1.7 1.7 0 0 0-1.6 1Z"/>',
      back: '<path d="m15 18-6-6 6-6"/>',
      folder: '<path d="M3 7h6l2 2h10v10H3z"/><path d="M3 7V5h6l2 2"/>',
      terminal: '<path d="m5 7 4 4-4 4"/><path d="M11 17h8"/>',
      external: '<path d="M14 3h7v7"/><path d="m10 14 11-11"/><path d="M21 14v5a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5"/>',
      plus: '<path d="M12 5v14M5 12h14"/>',
      history: '<path d="M3 12a9 9 0 1 0 3-6.7"/><path d="M3 4v5h5"/><path d="M12 7v5l3 2"/>',
      repo: '<path d="M4 4h16v16H4z"/><path d="M8 8h8M8 12h8M8 16h5"/>',
      check: '<path d="m5 12 4 4L19 6"/>',
      x: '<path d="m6 6 12 12M18 6 6 18"/>',
      shield: '<path d="M12 3 20 7v5c0 5-3.5 8-8 9-4.5-1-8-4-8-9V7z"/>',
      bolt: '<path d="m13 2-9 12h7l-1 8 10-12h-7z"/>',
      test: '<path d="m5 12 4 4L19 6"/>',
      command: '<path d="M9 6v12M15 6v12M6 9h12M6 15h12"/>',
    };
    return `<svg class="${esc(cls)}" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">${paths[name] || paths.command}</svg>`;
  }

  function categoryIcon(category) {
    const name = category === "tests" ? "test" : category === "lint" ? "bolt" : category === "security" ? "shield" : "command";
    return `<span class="category ${esc(category)}">${icon(name)}</span>`;
  }

  function relativeTime(value) {
    if (!value) return "";
    const milliseconds = Date.now() - new Date(value).getTime();
    if (!Number.isFinite(milliseconds)) return "";
    const minutes = Math.max(0, Math.round(milliseconds / 60000));
    if (minutes < 1) return "now";
    if (minutes < 60) return `${minutes}m`;
    const hours = Math.round(minutes / 60);
    if (hours < 24) return `${hours}h`;
    return `${Math.round(hours / 24)}d`;
  }

  function formatDate(value) {
    const date = new Date(value);
    if (Number.isNaN(date.getTime())) return "—";
    return date.toLocaleString([], { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" });
  }

  async function api(path, options = {}) {
    const headers = new Headers(options.headers || {});
    headers.set("X-Signoff-Token", TOKEN);
    if (options.body && !headers.has("Content-Type")) headers.set("Content-Type", "application/json");
    const response = await fetch(path, { ...options, headers });
    if (!response.ok) {
      const message = (await response.text()).trim() || `${response.status} ${response.statusText}`;
      throw new Error(message);
    }
    if (response.status === 204) return null;
    return response.json();
  }

  async function loadProduct(force = false) {
    if (force) loading = true;
    loadError = "";
    if (force) render();
    try {
      product = await api(force ? "/api/refresh" : "/api/product", { method: force ? "POST" : "GET" });
      if (selectedPRID && !product.pullRequests.some((pr) => pr.id === selectedPRID)) selectedPRID = null;
      if (!selectedSettingsRepoID && product.repositories[0]) selectedSettingsRepoID = product.repositories[0].id;
    } catch (error) {
      loadError = error.message;
    } finally {
      loading = false;
      render();
    }
  }

  function stateLabel(pr) {
    if (pr.status === "ready") return "Ready to merge";
    if (pr.status === "failing") return pr.blocking ? `${pr.blocking} failed` : "Blocked";
    if (pr.status === "attention") return `Waiting on ${pr.blocking || "signoff"}`;
    return "No signoff required";
  }

  function summaryText() {
    if (!product) return "Loading";
    const count = product.summary.needsAttention;
    if (product.summary.status === "failing") return `${product.summary.failing} failing · ${count} need attention`;
    if (count) return `${count} PR${count === 1 ? "" : "s"} need attention`;
    if (product.summary.totalPullRequests) return `${product.summary.totalPullRequests} PR${product.summary.totalPullRequests === 1 ? "" : "s"} clear`;
    return "No open pull requests";
  }

  function environmentProblem() {
    if (!product) return null;
    if (!product.env.ghInstalled) return { title: "GitHub CLI is required", detail: "Install gh, then reopen Signoff. Authentication remains in gh's keychain.", command: "brew install gh" };
    if (!product.env.authenticated) return { title: "Sign in to GitHub", detail: "Authenticate once in Terminal. Signoff never reads or stores the token.", command: "gh auth login" };
    if (!product.env.signoffInstalled) return { title: "Install gh-signoff", detail: "The companion can read status, but run-and-sign-off requires Basecamp's extension.", command: "gh extension install basecamp/gh-signoff" };
    return null;
  }

  function popoverHeader(focusedPR = null) {
    if (focusedPR) {
      return `<header class="popover-header">
        <button class="back-button" data-action="back" aria-label="Back to pull requests">${icon("back")}</button>
        <div class="brand"><span>${esc(focusedPR.repo)}</span></div>
        <span class="header-spacer"></span>
        <button class="icon-button" data-action="refresh" aria-label="Refresh">${icon("refresh")}</button>
        <button class="icon-button" data-action="settings" aria-label="Open settings">${icon("settings")}</button>
      </header>`;
    }
    const status = product?.summary?.status || "neutral";
    return `<header class="popover-header">
      <div class="brand">${icon("brand", "brand-mark")}<span>Signoff</span></div>
      <span class="live-dot ${esc(status)} ${loading ? "busy" : ""}"></span>
      <span class="brand-summary">${esc(summaryText())}</span>
      <span class="header-spacer"></span>
      <button class="icon-button" data-action="refresh" aria-label="Refresh">${icon("refresh")}</button>
      <button class="icon-button" data-action="settings" aria-label="Open settings">${icon("settings")}</button>
    </header>`;
  }

  function emptyState(title, detail, action = "", command = "") {
    return `<section class="empty-state"><div class="empty-state-inner">
      <div class="empty-icon">${icon("repo")}</div>
      <h2>${esc(title)}</h2><p>${esc(detail)}</p>
      ${action ? `<div class="empty-actions">${action}</div>` : ""}
      ${command ? `<code class="setup-code">${esc(command)}</code>` : ""}
    </div></section>`;
  }

  function staleDataNotice() {
    return loadError ? `<div class="notice red" role="status">Refresh failed: ${esc(loadError)}. Showing the last successful state.</div>` : "";
  }

  function pullRequestLimitNotice() {
    const repositories = (product?.repositories || []).filter((repo) => repo.pullRequestsTruncated);
    if (!repositories.length) return "";
    const names = repositories.map((repo) => repo.nameWithOwner).join(", ");
    return `<div class="notice">Showing the first 30 open pull requests for ${esc(names)}. Open the repository on GitHub to inspect the remaining pull requests.</div>`;
  }

  function renderPopover() {
    if (loading && !product) {
      return `<div class="popover-shell">${popoverHeader()}${emptyState("Reading pull requests", "Checking tracked repositories and branch protection…")}</div>`;
    }
    if (loadError && !product) {
      return `<div class="popover-shell">${popoverHeader()}${emptyState("Signoff could not start", loadError, '<button class="button primary" data-action="refresh">Try again</button>')}</div>`;
    }
    const problem = environmentProblem();
    if (problem) {
      return `<div class="popover-shell">${popoverHeader()}${emptyState(problem.title, problem.detail, '<button class="button ghost" data-action="settings">Open diagnostics</button>', problem.command)}</div>`;
    }
    if (!product.onboarded) {
      return `<div class="popover-shell">${popoverHeader()}${emptyState("Track your first repository", "Choose a local checkout. Signoff will watch your pull requests and keep GitHub credentials in gh.", '<button class="button primary" data-action="add-repository">Choose repository…</button><button class="button ghost" data-action="settings">Open setup</button>')}</div>`;
    }
    const focusedPR = product.pullRequests.find((pr) => pr.id === selectedPRID);
    if (focusedPR) return renderFocusedPopover(focusedPR);

    const visible = product.pullRequests.filter((pr) => filter === "all" || pr.status === "attention" || pr.status === "failing");
    let content;
    if (!visible.length) {
      content = emptyState(filter === "attention" ? "Everything is clear" : "No open pull requests", filter === "attention" ? "No tracked pull request is waiting on a signoff." : "Signoff will update when you open a pull request in a tracked repository.");
    } else {
      content = `<div class="pr-list">${visible.map((pr) => `<button class="pr-row" data-pr-id="${esc(pr.id)}">
        <span class="status-dot ${esc(pr.status)}"></span>
        <span class="pr-copy"><span class="pr-row-title">${esc(pr.pr.title)}</span><span class="pr-meta"><span>${esc(pr.repo)}</span><span>·</span><span>#${pr.pr.number}</span><span>·</span><span class="branch mono">${esc(pr.pr.headRef)}</span></span></span>
        <span class="readiness ${esc(pr.status)}"><strong>${pr.total ? `${pr.passed}/${pr.total}` : "—"}</strong><span>checks</span></span>
      </button>`).join("")}</div>`;
    }
    return `<div class="popover-shell">
      ${popoverHeader()}
      ${staleDataNotice()}
      ${pullRequestLimitNotice()}
      <div class="filter-bar">
        <button class="filter-button ${filter === "attention" ? "active" : ""}" data-filter="attention">Needs attention <span class="filter-count">${product.summary.needsAttention}</span></button>
        <button class="filter-button ${filter === "all" ? "active" : ""}" data-filter="all">All <span class="filter-count">${product.summary.totalPullRequests}</span></button>
      </div>
      <div class="popover-content">${content}</div>
      <footer class="popover-footer"><button class="footer-button" data-action="settings">Manage repositories</button><button class="footer-button" data-action="history">History</button></footer>
    </div>`;
  }

  function renderFocusedPopover(pr) {
    const otherPRs = product.pullRequests.filter((item) => item.id !== pr.id).slice(0, 2);
    const checks = pr.checks.map((check) => {
      const passed = check.state === "success";
      const failed = check.state === "failure" || check.state === "error";
      let action = `<span class="check-state">Not signed off</span>`;
      if (passed) action = `<span class="check-state pass">✓ ${esc(relativeTime(check.at))}</span>`;
      else if (failed) action = `<span class="check-state fail">Failed</span>`;
      else if (check.configured && pr.checkedOut) action = `<button class="button primary" data-run-check="${esc(check.name)}">Run & sign off</button>`;
      else if (check.configured) action = `<span class="check-state checkout">Checkout PR</span>`;
      else action = `<button class="button ghost" data-configure-check="${esc(check.name)}">Configure</button>`;
      return `<div class="check-row ${!passed && !failed ? "actionable" : ""}">
        ${categoryIcon(check.category)}
        <div class="check-copy"><div class="check-name">${esc(check.name)}</div>
          <div class="check-command mono ${check.configured && !passed ? "local" : ""}">${esc(check.configured ? `Local: ${check.localCommand}` : check.command)}</div>
        </div>${action}
      </div>`;
    }).join("");
    const terminalHTML = terminal.prID === pr.id && (terminal.open || terminal.running || terminal.lines.length) ? renderTerminal() : "";
    const merge = pr.ready && pr.pr.url ? `<button class="button accent" data-open-url="${esc(pr.pr.url)}">Open pull request</button>` : `<button class="footer-button" data-open-url="${esc(pr.pr.url)}">Open on GitHub</button>`;
    return `<div class="popover-shell">
      ${popoverHeader(pr)}
      <div class="popover-content">
        ${staleDataNotice()}
        ${pullRequestLimitNotice()}
        <section class="focus-hero">
          <div class="eyebrow"><span class="repo">${esc(pr.repo)}</span><span>·</span><span>#${pr.pr.number}</span><span>·</span><span class="branch mono">${esc(pr.pr.headRef)}</span></div>
          <h1 class="focus-title">${esc(pr.pr.title)}</h1>
          <div class="focus-status"><span class="status-pill ${esc(pr.status)}">${esc(stateLabel(pr))}</span><span class="focus-progress"><strong>${pr.total ? `${pr.passed}/${pr.total}` : "—"}</strong> readiness</span></div>
        </section>
        ${!pr.ready && !pr.checkedOut ? `<div class="notice">This checkout is not at the PR commit. Check out <span class="mono">${esc(pr.pr.headRef)}</span> locally, then refresh before running a check.</div>` : ""}
        ${pr.warning ? `<div class="notice">${esc(pr.warning)}</div>` : ""}
        <div class="section-label">Required checks</div>
        <div class="checks">${checks || '<div class="check-row"><span></span><div class="check-copy"><div class="check-name">No signoff checks required</div></div><span></span></div>'}</div>
        ${terminalHTML}
        ${otherPRs.length ? `<div class="section-label">Other pull requests</div>${otherPRs.map((other) => `<button class="other-pr" data-pr-id="${esc(other.id)}"><span class="status-dot ${esc(other.status)}"></span><span class="title"><strong>#${other.pr.number}</strong> ${esc(other.pr.title)}</span><span class="branch mono">${esc(other.pr.headRef)}</span></button>`).join("")}` : ""}
      </div>
      <footer class="popover-footer">${merge}<span class="footer-spacer"></span><button class="footer-button" data-action="settings">Settings</button></footer>
    </div>`;
  }

  function renderTerminal() {
    const lines = terminal.lines.map((line) => `<div class="${esc(line.kind || "")}">${esc(line.text)}</div>`).join("");
    return `<section class="terminal-wrap"><div class="terminal-head">${icon("terminal")} Command output <span>${esc(terminal.phase || "")}</span><button data-action="toggle-terminal">${terminal.open ? "Hide" : "Show"}</button></div>${terminal.open ? `<div class="terminal" id="terminalOutput">${lines || '<span class="phase">Starting…</span>'}</div>` : ""}</section>`;
  }

  function settingsMeta() {
    const tabs = [
      ["repositories", "Repositories"], ["commands", "Commands"], ["rules", "Branch rules"],
      ["history", "History"], ["diagnostics", "Diagnostics"],
    ];
    return `<aside class="settings-sidebar"><div class="settings-brand">${icon("brand", "brand-mark")} Signoff</div><nav class="settings-nav">${tabs.map(([id, label]) => `<button class="${settingsTab === id ? "active" : ""}" data-settings-tab="${id}">${label}</button>`).join("")}</nav><footer>Menu-bar companion for <span class="mono">gh signoff</span><br>${esc(product?.env?.signoffVersion ? `gh-signoff ${product.env.signoffVersion}` : "")}</footer></aside>`;
  }

  function renderSettings() {
    if (loading && !product) return `<div class="settings-shell">${settingsMeta()}<section class="settings-main">${emptyState("Loading Signoff", "Reading configuration and GitHub status…")}</section></div>`;
    if (loadError && !product) return `<div class="settings-shell">${settingsMeta()}<section class="settings-main">${emptyState("Could not load settings", loadError, '<button class="button primary" data-action="refresh">Try again</button>')}</section></div>`;
    let page = "";
    if (settingsTab === "commands") page = settingsCommands();
    else if (settingsTab === "rules") page = settingsRules();
    else if (settingsTab === "history") page = settingsHistory();
    else if (settingsTab === "diagnostics") page = settingsDiagnostics();
    else page = settingsRepositories();
    return `<div class="settings-shell">${settingsMeta()}<section class="settings-main">${staleDataNotice()}${page}</section></div>`;
  }

  function settingsHeader(title, detail, action = "") {
    return `<header class="settings-header"><div><h1>${esc(title)}</h1><p>${esc(detail)}</p></div>${action}</header>`;
  }

  function settingsRepositories() {
    const rows = product.repositories.map((repo) => `<div class="repo-setting-row">
      <div><div class="repo-name">${esc(repo.nameWithOwner)}</div><div class="repo-path mono">${esc(repo.path)}</div>${repo.error ? `<div class="history-state failure">${esc(repo.error)}</div>` : ""}${repo.pullRequestsTruncated ? '<div class="notice">Showing the first 30 open pull requests.</div>' : ""}</div>
      <select class="select" data-repo-scope="${esc(repo.id)}" aria-label="Pull request scope"><option value="mine" ${repo.pullRequestScope !== "all" ? "selected" : ""}>My PRs</option><option value="all" ${repo.pullRequestScope === "all" ? "selected" : ""}>All open PRs</option></select>
      <div style="display:flex;align-items:center;gap:10px"><input class="toggle" type="checkbox" data-repo-enabled="${esc(repo.id)}" ${repo.enabled ? "checked" : ""} aria-label="Track ${esc(repo.nameWithOwner)}"><button class="button danger" data-remove-repo="${esc(repo.id)}">Remove</button></div>
    </div>`).join("");
    return `${settingsHeader("Tracked repositories", "Signoff watches pull requests in these local checkouts.", '<button class="button primary" data-action="add-repository">Add repository…</button>')}
      <div class="settings-section">${READ_ONLY ? '<div class="notice">Read-only mode is active. Configuration can change, but commands and GitHub writes remain disabled.</div>' : ""}${rows ? `<div class="list-card">${rows}</div>` : emptyState("No repositories yet", "Add a local Git checkout to start tracking pull requests.", '<button class="button primary" data-action="add-repository">Choose repository…</button>')}</div>`;
  }

  function activeSettingsRepo() {
    return product.repositories.find((repo) => repo.id === selectedSettingsRepoID) || product.repositories[0] || null;
  }

  function repoSelector(repo, attr = "settings-repo") {
    return `<select class="select" data-${attr}>${product.repositories.map((item) => `<option value="${esc(item.id)}" ${repo && item.id === repo.id ? "selected" : ""}>${esc(item.nameWithOwner)}</option>`).join("")}</select>`;
  }

  function settingsCommands() {
    const repo = activeSettingsRepo();
    if (!repo) return `${settingsHeader("Local commands", "Map each signoff context to the command that must pass first.")}${emptyState("Add a repository first", "Command mappings belong to a tracked local checkout.", '<button class="button primary" data-action="add-repository">Add repository…</button>')}`;
    const known = new Set(Object.keys(repo.commands || {}));
    product.pullRequests.filter((pr) => pr.repoId === repo.id).forEach((pr) => pr.checks.forEach((check) => known.add(check.name)));
    const rows = [...known].sort().map((name) => {
      const mapping = repo.commands?.[name] || { command: "", timeoutSeconds: 1800 };
      return `<div class="inline-form" data-command-row="${esc(name)}"><div class="field"><label>Check</label><input class="input mono" value="${esc(name)}" disabled></div><div class="field"><label>Local command</label><input class="input mono" data-command-value value="${esc(mapping.command)}" placeholder="bin/test"></div><div class="field"><label>Timeout</label><input class="input" data-command-timeout type="number" min="10" max="7200" value="${mapping.timeoutSeconds || 1800}"></div><button class="button ghost" data-save-command="${esc(name)}">Save</button></div>`;
    }).join("");
    return `${settingsHeader("Local commands", "A command runs in the repository checkout. Only exit 0 proceeds to gh signoff.", repoSelector(repo))}<div class="settings-section"><div class="notice">Commands are explicit local configuration. Run requests cannot supply or override shell text.</div><div class="list-card">${rows || '<div class="repo-setting-row"><div>No required checks have been discovered yet.</div></div>'}</div><form id="addCommandForm" class="form-actions"><input class="input mono" name="check" placeholder="check name" required><input class="input mono" name="command" placeholder="local command" required><button class="button primary" type="submit">Add mapping</button></form></div>`;
  }

  function settingsRules() {
    const repo = activeSettingsRepo();
    if (!repo) return `${settingsHeader("Branch rules", "Edit signoff contexts without disturbing unrelated protection.")}${emptyState("Add a repository first", "Branch rules are read from a tracked GitHub repository.")}`;
    const pr = product.pullRequests.find((item) => item.repoId === repo.id);
    const branch = pr?.rules?.branch || repo.defaultBranch || "main";
    const checks = (pr?.rules?.signoffContexts || []).map((name) => name === "signoff" ? "signoff" : name.replace(/^signoff\//, "")).join(", ");
    const unreadable = Boolean(pr) && !pr?.rules?.readable;
    return `${settingsHeader("Branch rules", "Change only required status-check contexts; reviews, restrictions, and other protection remain intact.", repoSelector(repo))}<div class="settings-section">
      ${unreadable ? '<div class="notice red">Branch protection is not readable with your current GitHub permissions, so editing is disabled.</div>' : '<div class="notice">Unlike gh signoff uninstall, this editor never deletes the branch protection rule. External status checks are preserved.</div>'}
      <form id="rulesForm" class="list-card" style="padding:16px">
        <div class="field"><label>Branch</label><input class="input mono" name="branch" value="${esc(branch)}" required></div>
        <div class="field"><label>Required signoff checks</label><input class="input mono" name="checks" value="${esc(checks)}" placeholder="tests, lint, security"><span style="color:var(--mute);font-size:10px">Comma-separated. Leave empty to remove only Signoff contexts.</span></div>
        <div class="form-actions"><button class="button primary" type="submit" ${unreadable || READ_ONLY ? "disabled" : ""}>Update branch rule</button><code>${esc(pr?.rules?.installCommand || `gh signoff install --branch ${branch}`)}</code></div>
      </form></div>`;
  }

  function settingsHistory() {
    const rows = product.history.map((event) => `<div class="history-row"><span class="status-dot ${event.state === "success" ? "ready" : event.state === "failure" || event.state === "error" ? "failing" : "attention"}"></span><span class="history-time">${esc(formatDate(event.at))}</span><span class="history-repo">${esc(event.repo || "Local")}${event.prNumber ? ` #${event.prNumber}` : ""}</span><span class="history-detail"><strong>${esc(event.check || event.kind)}</strong>${event.prTitle ? ` — ${esc(event.prTitle)}` : ""}${event.detail ? ` — ${esc(event.detail)}` : ""}</span><span class="history-state ${esc(event.state)}">${esc(event.state)}</span></div>`).join("");
    return `${settingsHeader("Activity history", "Runs, signoffs, and GitHub commit statuses across tracked pull requests.", '<button class="button ghost" data-action="refresh">Refresh</button>')}<div class="settings-section">${rows ? `<div class="list-card">${rows}</div>` : emptyState("No activity yet", "Run a configured check or sign off a pull request and it will appear here.")}</div>`;
  }

  function settingsDiagnostics() {
    const env = product.env;
    const row = (label, value) => `<dt>${esc(label)}</dt><dd class="mono">${esc(value === 0 ? "0" : value || "—")}</dd>`;
    return `${settingsHeader("Diagnostics", "Current local integration state. Authentication remains owned by GitHub CLI.", '<button class="button ghost" data-action="refresh">Run checks again</button>')}<div class="settings-section">${product.demo ? '<div class="notice">Demo mode is active. Nothing is read from or written to GitHub.</div>' : ""}<dl class="diagnostic-grid">${row("GitHub CLI", env.ghInstalled ? env.ghVersion : "Not installed")}${row("GitHub account", env.authenticated ? env.login : "Not authenticated")}${row("gh-signoff", env.signoffInstalled ? env.signoffVersion : "Not installed")}${row("Tracked repositories", product.repositories.length)}${row("Open pull requests", product.summary.totalPullRequests)}${row("Config", product.configPath)}${row("Last refresh", formatDate(product.fetchedAt))}${row("Mode", READ_ONLY ? "Read only" : "Read and write")}</dl></div>`;
  }

  function render() {
    app.innerHTML = surface === "settings" || surface === "full" ? renderSettings() : renderPopover();
    wire();
  }

  function wire() {
    document.querySelectorAll("[data-pr-id]").forEach((element) => element.addEventListener("click", () => {
      selectedPRID = element.dataset.prId;
      terminal = { open: false, running: false, phase: "", lines: [], prID: "", check: "" };
      render();
    }));
    document.querySelectorAll("[data-filter]").forEach((element) => element.addEventListener("click", () => { filter = element.dataset.filter; render(); }));
    document.querySelectorAll("[data-settings-tab]").forEach((element) => element.addEventListener("click", () => { settingsTab = element.dataset.settingsTab; render(); }));
    document.querySelectorAll("[data-action]").forEach((element) => element.addEventListener("click", () => handleAction(element.dataset.action)));
    document.querySelectorAll("[data-open-url]").forEach((element) => element.addEventListener("click", () => openExternal(element.dataset.openUrl)));
    document.querySelectorAll("[data-run-check]").forEach((element) => element.addEventListener("click", () => runCheck(element.dataset.runCheck)));
    document.querySelectorAll("[data-configure-check]").forEach((element) => element.addEventListener("click", () => openSettings("commands")));
    document.querySelectorAll("[data-repo-enabled]").forEach((element) => element.addEventListener("change", () => updateRepositorySetting(element.dataset.repoEnabled, { enabled: element.checked })));
    document.querySelectorAll("[data-repo-scope]").forEach((element) => element.addEventListener("change", () => updateRepositorySetting(element.dataset.repoScope, { pullRequestScope: element.value })));
    document.querySelectorAll("[data-remove-repo]").forEach((element) => element.addEventListener("click", () => removeRepository(element.dataset.removeRepo)));
    document.querySelectorAll("[data-save-command]").forEach((element) => element.addEventListener("click", () => saveCommand(element.dataset.saveCommand, element.closest("[data-command-row]"))));
    const repoSelect = document.querySelector("[data-settings-repo]");
    if (repoSelect) repoSelect.addEventListener("change", () => { selectedSettingsRepoID = repoSelect.value; render(); });
    const addCommand = document.querySelector("#addCommandForm");
    if (addCommand) addCommand.addEventListener("submit", addCommandMapping);
    const rules = document.querySelector("#rulesForm");
    if (rules) rules.addEventListener("submit", saveRules);
  }

  async function handleAction(action) {
    if (action === "back") { selectedPRID = null; render(); return; }
    if (action === "refresh") { await loadProduct(true); return; }
    if (action === "settings") { await openSettings("repositories"); return; }
    if (action === "history") { await openSettings("history"); return; }
    if (action === "add-repository") { await addRepository(); return; }
    if (action === "toggle-terminal") { terminal.open = !terminal.open; render(); scrollTerminal(); }
  }

  async function openSettings(tab) {
    if (desktop?.openSettings) {
      await desktop.openSettings(tab);
      return;
    }
    location.href = `/?surface=settings&tab=${encodeURIComponent(tab)}`;
  }

  async function openExternal(url) {
    if (!/^https:\/\/github\.com\//.test(url || "")) return;
    if (desktop?.openExternal) await desktop.openExternal(url);
    else window.open(url, "_blank", "noopener");
  }

  async function addRepository() {
    try {
      let path = "";
      if (desktop?.chooseRepository) path = await desktop.chooseRepository();
      else path = window.prompt("Absolute path to a local Git repository") || "";
      if (!path) return;
      await api("/api/repositories", { method: "POST", body: JSON.stringify({ path }) });
      showToast("Repository added");
      await loadProduct(true);
    } catch (error) { showToast(error.message, true); }
  }

  async function updateRepositorySetting(id, changes) {
    const repo = product.repositories.find((item) => item.id === id);
    if (!repo) return;
    try {
      await api(`/api/repositories/${encodeURIComponent(id)}`, { method: "PUT", body: JSON.stringify({ ...repo, ...changes }) });
      showToast("Repository updated");
      await loadProduct(true);
    } catch (error) { showToast(error.message, true); }
  }

  async function removeRepository(id) {
    const repo = product.repositories.find((item) => item.id === id);
    if (!repo || !window.confirm(`Stop tracking ${repo.nameWithOwner}? Local files and GitHub data will not be changed.`)) return;
    try {
      await api(`/api/repositories/${encodeURIComponent(id)}`, { method: "DELETE" });
      selectedSettingsRepoID = null;
      showToast("Repository removed");
      await loadProduct(true);
    } catch (error) { showToast(error.message, true); }
  }

  async function saveCommand(name, row) {
    const repo = activeSettingsRepo();
    if (!repo || !row) return;
    const command = row.querySelector("[data-command-value]").value.trim();
    const timeoutSeconds = Number(row.querySelector("[data-command-timeout]").value) || 1800;
    const commands = { ...(repo.commands || {}) };
    if (command) commands[name] = { command, timeoutSeconds };
    else delete commands[name];
    await persistCommands(repo, commands);
  }

  async function addCommandMapping(event) {
    event.preventDefault();
    const repo = activeSettingsRepo();
    const form = new FormData(event.currentTarget);
    const name = String(form.get("check") || "").trim();
    const command = String(form.get("command") || "").trim();
    if (!/^[A-Za-z0-9][A-Za-z0-9_.-]{0,63}$/.test(name)) { showToast("Check names may use letters, numbers, dot, dash, and underscore", true); return; }
    if (!command) return;
    const commands = { ...(repo.commands || {}), [name]: { command, timeoutSeconds: 1800 } };
    await persistCommands(repo, commands);
  }

  async function persistCommands(repo, commands) {
    try {
      await api(`/api/repositories/${encodeURIComponent(repo.id)}`, { method: "PUT", body: JSON.stringify({ ...repo, commands }) });
      showToast("Command mapping saved");
      await loadProduct(true);
    } catch (error) { showToast(error.message, true); }
  }

  async function saveRules(event) {
    event.preventDefault();
    const repo = activeSettingsRepo();
    const form = new FormData(event.currentTarget);
    const branch = String(form.get("branch") || "").trim();
    const checks = String(form.get("checks") || "").split(",").map((item) => item.trim()).filter(Boolean);
    if (!window.confirm(`Update required Signoff checks on ${repo.nameWithOwner}:${branch}? Unrelated branch-protection settings and external contexts will be preserved.`)) return;
    try {
      await api("/api/rules", { method: "PUT", body: JSON.stringify({ repoId: repo.id, branch, checks }) });
      showToast("Branch rule updated");
      await loadProduct(true);
    } catch (error) { showToast(error.message, true); }
  }

  async function runCheck(checkName) {
    const pr = product.pullRequests.find((item) => item.id === selectedPRID);
    if (!pr || terminal.running) return;
    terminal = { open: true, running: true, phase: "Starting", lines: [], prID: pr.id, check: checkName };
    render();
    try {
      const response = await fetch("/api/run", {
        method: "POST",
        headers: { "Content-Type": "application/json", "X-Signoff-Token": TOKEN },
        body: JSON.stringify({ repoId: pr.repoId, prNumber: pr.pr.number, check: checkName }),
      });
      if (!response.ok || !response.body) throw new Error((await response.text()).trim() || `Run failed: ${response.status}`);
      const reader = response.body.getReader();
      const decoder = new TextDecoder();
      let buffer = "";
      let result = { code: 1 };
      while (true) {
        const { done, value } = await reader.read();
        if (done) break;
        buffer += decoder.decode(value, { stream: true });
        let boundary;
        while ((boundary = buffer.indexOf("\n\n")) >= 0) {
          const frame = buffer.slice(0, boundary);
          buffer = buffer.slice(boundary + 2);
          let eventName = "message";
          let data = "";
          frame.split("\n").forEach((line) => {
            if (line.startsWith("event: ")) eventName = line.slice(7);
            if (line.startsWith("data: ")) data = line.slice(6).replace(/\\n/g, "\n");
          });
          if (eventName === "phase") {
            terminal.phase = data === "run" ? "Running local command" : "Signing off commit";
            terminal.lines.push({ kind: "phase", text: `— ${terminal.phase}` });
          } else if (eventName === "line") {
            terminal.lines.push({ kind: /^✓/.test(data) ? "success" : /^✗|failed|error/i.test(data) ? "failure" : "", text: data });
          } else if (eventName === "done") {
            try { result = JSON.parse(data); } catch { result = { code: 1 }; }
          }
          scheduleTerminalRender();
        }
      }
      terminal.running = false;
      terminal.phase = result.code === 0 ? "Complete" : "Failed";
      showToast(result.code === 0 ? `${checkName} passed and was signed off` : `${checkName} did not complete`, result.code !== 0);
      await loadProduct(false);
    } catch (error) {
      terminal.running = false;
      terminal.phase = "Failed";
      terminal.lines.push({ kind: "failure", text: error.message });
      showToast(error.message, true);
      render();
      scrollTerminal();
    }
  }

  function scrollTerminal() {
    requestAnimationFrame(() => {
      const output = document.querySelector("#terminalOutput");
      if (output) output.scrollTop = output.scrollHeight;
    });
  }

  function scheduleTerminalRender() {
    if (terminalRenderQueued) return;
    terminalRenderQueued = true;
    requestAnimationFrame(() => {
      terminalRenderQueued = false;
      render();
      scrollTerminal();
    });
  }

  function showToast(message, failure = false) {
    toast.textContent = message;
    toast.style.background = failure ? "var(--red)" : "var(--text)";
    toast.hidden = false;
    clearTimeout(showToast.timer);
    showToast.timer = setTimeout(() => { toast.hidden = true; }, 2600);
  }

  function buildPaletteActions() {
    const actions = [];
    (product?.pullRequests || []).forEach((pr) => {
      actions.push({
        label: `Open #${pr.pr.number} ${pr.pr.title}`, meta: pr.repo,
        run: () => { selectedPRID = pr.id; render(); },
      });
      pr.checks.filter((check) => check.required && check.state !== "success").forEach((check) => {
        if (check.configured && pr.checkedOut) actions.push({
          label: `Run ${check.name} on #${pr.pr.number}`, meta: pr.repo,
          run: () => { selectedPRID = pr.id; render(); runCheck(check.name); },
        });
        actions.push({
          label: `Copy ${check.command}`, meta: `${pr.repo} #${pr.pr.number}`,
          run: () => copyCommand(check.command),
        });
      });
    });
    actions.push({ label: "Refresh all repositories", meta: "status", run: () => loadProduct(true) });
    actions.push({ label: "Manage repositories", meta: "settings", run: () => openSettings("repositories") });
    actions.push({ label: "Configure commands", meta: "settings", run: () => openSettings("commands") });
    actions.push({ label: "Edit branch rules", meta: "settings", run: () => openSettings("rules") });
    actions.push({ label: "Open activity history", meta: "history", run: () => openSettings("history") });
    actions.push({ label: "Open diagnostics", meta: "settings", run: () => openSettings("diagnostics") });
    return actions;
  }

  async function copyCommand(command) {
    try {
      if (desktop?.copyText) await desktop.copyText(command);
      else await navigator.clipboard.writeText(command);
      showToast("Command copied");
    } catch (error) { showToast(error.message || "Could not copy command", true); }
  }

  function fuzzyScore(candidate, query) {
    if (!query) return 0;
    const exact = candidate.indexOf(query);
    if (exact >= 0) return 1000 - exact;
    let score = 0;
    let cursor = 0;
    let previous = -1;
    for (const character of query) {
      const found = candidate.indexOf(character, cursor);
      if (found < 0) return null;
      score += previous >= 0 && found === previous + 1 ? 8 : 2;
      score -= found - cursor;
      previous = found;
      cursor = found + 1;
    }
    return score;
  }

  function openPalette() {
    if (!product) return;
    paletteActions = buildPaletteActions();
    paletteIndex = 0;
    paletteInput.value = "";
    paletteOverlay.hidden = false;
    renderPalette();
    setTimeout(() => paletteInput.focus(), 10);
  }

  function closePalette() { paletteOverlay.hidden = true; }

  function renderPalette() {
    const query = paletteInput.value.trim().toLowerCase();
    const filtered = paletteActions.map((action, order) => ({
      ...action, order, score: fuzzyScore(`${action.label} ${action.meta}`.toLowerCase(), query),
    })).filter((action) => action.score !== null).sort((a, b) => b.score - a.score || a.order - b.order);
    paletteIndex = Math.min(paletteIndex, Math.max(0, filtered.length - 1));
    paletteResults.innerHTML = filtered.map((action, index) => `<button class="palette-item" role="option" aria-selected="${index === paletteIndex}" data-palette-index="${index}"><strong>${esc(action.label)}</strong><span>${esc(action.meta)}</span></button>`).join("") || '<div class="empty-state" style="min-height:100px">No matching action</div>';
    paletteResults.querySelectorAll("[data-palette-index]").forEach((element) => element.addEventListener("click", () => {
      const action = filtered[Number(element.dataset.paletteIndex)];
      closePalette();
      action?.run();
    }));
    renderPalette.filtered = filtered;
  }

  paletteInput.addEventListener("input", () => { paletteIndex = 0; renderPalette(); });
  paletteOverlay.addEventListener("click", (event) => { if (event.target === paletteOverlay) closePalette(); });
  document.addEventListener("keydown", (event) => {
    if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "k") {
      event.preventDefault();
      paletteOverlay.hidden ? openPalette() : closePalette();
      return;
    }
    if (paletteOverlay.hidden) return;
    if (event.key === "Escape") closePalette();
    if (event.key === "ArrowDown") { event.preventDefault(); paletteIndex = Math.min(paletteIndex + 1, (renderPalette.filtered || []).length - 1); renderPalette(); }
    if (event.key === "ArrowUp") { event.preventDefault(); paletteIndex = Math.max(0, paletteIndex - 1); renderPalette(); }
    if (event.key === "Enter") { event.preventDefault(); const action = (renderPalette.filtered || [])[paletteIndex]; closePalette(); action?.run(); }
  });

  function startPolling() {
    clearInterval(pollTimer);
    if (surface !== "popover") return;
    pollTimer = setInterval(() => { if (!terminal.running && !document.hidden) loadProduct(false); }, 30000);
  }

  document.addEventListener("visibilitychange", () => { if (!document.hidden && surface === "popover") loadProduct(false); });
  if (desktop?.onPopoverShown) desktop.onPopoverShown(() => loadProduct(false));

  loadProduct(false).then(startPolling);
})();
