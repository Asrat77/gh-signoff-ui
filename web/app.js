/* Signoff — front end.
   Reads real state from the Go process, which reads it from the GitHub API.
   Nothing here fabricates state: if a value is unknown it renders as unknown. */

(() => {
  "use strict";

  const TOKEN = window.SIGNOFF.token;
  const READ_ONLY = window.SIGNOFF.readOnly;

  let state = null;
  let page = "dashboard";
  let termLines = [];
  let termOpen = false;
  let busy = false;

  const $ = (sel) => document.querySelector(sel);
  const esc = (s) =>
    String(s ?? "").replace(/[&<>"']/g, (c) =>
      ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c])
    );

  const announce = (msg) => { $("#live").textContent = msg; };

  // ---------------------------------------------------------------- helpers

  function relTime(iso) {
    if (!iso) return "";
    const then = new Date(iso).getTime();
    if (Number.isNaN(then)) return "";
    const secs = Math.round((Date.now() - then) / 1000);
    if (secs < 45) return "just now";
    if (secs < 90) return "1m ago";
    if (secs < 3600) return `${Math.round(secs / 60)}m ago`;
    if (secs < 86400) return `${Math.round(secs / 3600)}h ago`;
    if (secs < 604800) return `${Math.round(secs / 86400)}d ago`;
    return new Date(iso).toLocaleDateString();
  }

  function clockTime(iso) {
    const d = new Date(iso);
    if (Number.isNaN(d.getTime())) return "";
    const today = new Date();
    const sameDay = d.toDateString() === today.toDateString();
    return sameDay
      ? d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })
      : d.toLocaleDateString([], { month: "short", day: "numeric" });
  }

  const ICONS = {
    tests: '<path d="M9 16.2L4.8 12l-1.4 1.4L9 19 21 7l-1.4-1.4z"/>',
    lint: '<path d="M13 2L3 14h7l-1 8 10-12h-7l1-8z"/>',
    security: '<path d="M12 2l8 4v6c0 5-3.5 9-8 10-4.5-1-8-5-8-10V6l8-4z"/>',
    default: '<path d="M12 2l2.9 6.2 6.8.9-5 4.7 1.3 6.7L12 17.3 5.9 20.5 7.2 13.8l-5-4.7 6.8-.9z"/>',
    alt1: '<circle cx="12" cy="12" r="9"/>',
    alt2: '<rect x="4" y="4" width="16" height="16" rx="3"/>',
    alt3: '<path d="M12 3l9 16H3z"/>',
  };

  const CHECK_SVG =
    '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="3"><path d="M5 13l4 4L19 7"/></svg>';
  const X_SVG =
    '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="3"><path d="M6 6l12 12M18 6L6 18"/></svg>';
  const DOT_SVG =
    '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="3"><circle cx="12" cy="12" r="9"/></svg>';
  const SPIN_SVG =
    '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="3"><path d="M12 3a9 9 0 1 0 9 9"/></svg>';
  const WARN_SVG =
    '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2"><path d="M12 9v4M12 17h.01M10.3 3.9L2.4 18a2 2 0 0 0 1.7 3h15.8a2 2 0 0 0 1.7-3L13.7 3.9a2 2 0 0 0-3.4 0z"/></svg>';

  function badge(category) {
    const glyph = ICONS[category] || ICONS.default;
    return `<span class="cat-badge ${esc(category)}"><svg viewBox="0 0 24 24" fill="#fff">${glyph}</svg></span>`;
  }

  // ------------------------------------------------------------------- data

  async function fetchState() {
    const dot = $("#refreshDot");
    dot.classList.add("busy");
    try {
      const res = await fetch("/api/state", { headers: { "X-Signoff-Token": TOKEN } });
      if (!res.ok) throw new Error(`state request failed: ${res.status}`);
      state = await res.json();
      dot.classList.remove("stale");
      render();
    } catch (err) {
      dot.classList.add("stale");
      dot.title = `Could not refresh: ${err.message}`;
    } finally {
      dot.classList.remove("busy");
    }
  }

  // ------------------------------------------------------------- rendering

  function render() {
    if (!state) return;
    renderChrome();

    const env = state.env;
    const blocked = !env.ghInstalled || !env.authenticated || !env.inGitRepo;

    $("#page-dashboard").innerHTML = blocked ? setupScreen(env) : dashboard();
    $("#page-history").innerHTML = blocked ? setupScreen(env) : historyPage();
    $("#page-rules").innerHTML = blocked ? setupScreen(env) : rulesPage();

    wireDashboard();
  }

  function renderChrome() {
    const repo = state.repo || {};
    const name = repo.nameWithOwner || "—";
    const short = name.split("/").pop() || name;
    $("#repoName").textContent = short;
    $("#repoName").title = name;
    $("#repoBranch").textContent = repo.currentBranch || "";
    $("#repoInitials").textContent = short.slice(0, 2);
    $("#repoPicker").onclick = () => {
      if (repo.nameWithOwner) window.open(`https://github.com/${repo.nameWithOwner}`, "_blank", "noopener");
    };

    const v = state.env.signoffVersion ? `gh-signoff ${esc(state.env.signoffVersion)}` : "";
    $("#versionLine").innerHTML = v;

    $("#navCount").textContent = state.total ? `${state.passed}/${state.total}` : "";
  }

  function setupScreen(env) {
    if (!env.ghInstalled) {
      return empty(
        "GitHub CLI not found",
        "Signoff drives the <code>gh</code> command, so it needs to be installed and on your PATH.",
        "brew install gh"
      );
    }
    if (!env.authenticated) {
      return empty(
        "Not signed in to GitHub",
        "Authenticate the CLI once and this window picks it up — Signoff never handles your token itself.",
        "gh auth login"
      );
    }
    return empty(
      "Not a git repository",
      "Start Signoff from inside a repository, or point it at one with the <code>-dir</code> flag.",
      "cd path/to/repo && gh signoff-ui"
    );
  }

  function empty(title, body, cmd) {
    return `<div class="empty">
      <h3>${esc(title)}</h3>
      <p>${body}</p>
      ${cmd ? `<code class="cmd-chip">${esc(cmd)}</code>` : ""}
    </div>`;
  }

  function banner(text, kind = "amber") {
    return `<div class="banner ${kind === "red" ? "red" : ""}">${WARN_SVG}<span>${text}</span></div>`;
  }

  function dashboard() {
    const { pr, repo, checks, rules } = state;
    let html = "";

    if (state.error) return banner(esc(state.error), "red") + extensionNotice();
    if (state.warning) html += banner(esc(state.warning));
    html += extensionNotice();

    if (!pr) {
      html += `<div class="empty">
        <h3>No pull request for <code class="cmd-chip">${esc(repo.currentBranch || "this branch")}</code></h3>
        <p>Signoff follows the PR for your current branch. Open one and this fills in — signoff status still applies to the commit either way.</p>
        <code class="cmd-chip">gh pr create</code>
      </div>`;
      return html;
    }

    html += headerBlock(pr);

    if (rules.readable && !rules.enforced) {
      html += banner(
        `Branch protection on <code class="cmd-chip">${esc(rules.branch)}</code> does not require a signoff, so nothing here blocks a merge. ` +
        `Run <code class="cmd-chip">gh signoff install</code> to make it a requirement.`
      );
    }

    html += checksTable(checks);
    html += terminalBlock();
    html += footnoteBlock(rules);
    return html;
  }

  function extensionNotice() {
    if (state.env.signoffInstalled) return "";
    return banner(
      "The <code class=\"cmd-chip\">gh-signoff</code> extension isn't installed, so signing off from this window will fail. " +
      "Status shown here is read straight from GitHub and is still accurate. " +
      "<code class=\"cmd-chip\">gh extension install basecamp/gh-signoff</code>"
    );
  }

  function headerBlock(pr) {
    const total = state.total || 0;
    const passed = state.passed || 0;
    const pct = total ? passed / total : 0;
    const R = 34, C = 2 * Math.PI * R;
    const offset = C - pct * C;
    const ringColor = total === 0 ? "var(--mute)" : state.ready ? "var(--green)" : "var(--amber)";

    let pill;
    if (total === 0) {
      pill = `<span class="status-pill grey">${DOT_SVG} No signoff required</span>`;
    } else if (state.ready) {
      pill = `<span class="status-pill green">${CHECK_SVG} Ready to merge</span>`;
    } else if (checksHaveFailure()) {
      pill = `<span class="status-pill red">${X_SVG} ${esc(failingName())} failed</span>`;
    } else {
      pill = `<span class="status-pill amber">${DOT_SVG} Waiting on ${esc(state.blocking || "signoff")}</span>`;
    }

    // Blue appears exactly once per screen, and only when it is actionable.
    const merge = state.ready
      ? `<a class="btn blue merge-cta" href="${esc(pr.url)}" target="_blank" rel="noopener">
           Merge pull request
           <svg viewBox="0 0 24 24" fill="none" stroke="#fff" stroke-width="2.5"><path d="M5 12h14M13 6l6 6-6 6"/></svg>
         </a>`
      : "";

    return `<div class="dash-head">
      <div style="min-width:0">
        <h1 class="pr-title"><a href="${esc(pr.url)}" target="_blank" rel="noopener">${esc(pr.title)}</a></h1>
        <div class="pr-sub">
          PR #${esc(pr.number)} · ${esc(state.repo.nameWithOwner)} · opened by ${esc(pr.author || "unknown")}
          · into <code class="cmd-chip">${esc(pr.baseRef)}</code>
        </div>
        <div class="status-line">${pill}${merge}</div>
      </div>
      <div class="ring-wrap" role="img" aria-label="${passed} of ${total} required checks signed off">
        <svg width="80" height="80" viewBox="0 0 80 80">
          <circle class="ring-track" cx="40" cy="40" r="${R}" fill="none" stroke-width="8"/>
          <circle cx="40" cy="40" r="${R}" fill="none" stroke="${ringColor}" stroke-width="8"
                  stroke-linecap="round" stroke-dasharray="${C.toFixed(1)}" stroke-dashoffset="${offset.toFixed(1)}"
                  style="transition:stroke-dashoffset .6s ease, stroke .3s ease"/>
        </svg>
        <div class="ring-center">
          <div class="num">${passed}/${total}</div>
          <div class="lbl">signed off</div>
        </div>
      </div>
    </div>`;
  }

  const checksHaveFailure = () =>
    (state.checks || []).some((c) => c.required && (c.state === "failure" || c.state === "error"));
  const failingName = () =>
    ((state.checks || []).find((c) => c.required && (c.state === "failure" || c.state === "error")) || {}).name || "";

  function checksTable(checks) {
    if (!checks || !checks.length) {
      return `<div class="empty">
        <h3>No signoff contexts yet</h3>
        <p>Nothing on this commit and nothing required by branch protection.</p>
        <code class="cmd-chip">gh signoff install</code>
      </div>`;
    }

    const rows = checks.map((c) => {
      let chip;
      if (c.state === "success") {
        chip = `<span class="status-chip pass">${CHECK_SVG}${esc(relTime(c.at))}</span>`;
      } else if (c.state === "failure" || c.state === "error") {
        chip = `<span class="status-chip fail">${X_SVG}${esc(c.description || "failed")}</span>`;
      } else if (c.state === "pending") {
        chip = `<span class="status-chip spin">${SPIN_SVG}pending</span>`;
      } else {
        chip = `<span class="status-chip pending">not signed off</span>`;
      }

      const signedOff = c.state === "success";
      const action = signedOff
        ? `<button class="btn ghost" type="button" data-copy="${esc(c.command)}">Copy command</button>`
        : `<button class="btn primary" type="button" data-signoff="${esc(c.name === "signoff" ? "" : c.name)}">Sign off</button>`;

      const who = c.actor ? ` · by ${esc(c.actor)}` : "";

      return `<div class="chk-row ${c.required ? "" : "not-required"}" data-key="${esc(c.name)}">
        <div class="chk-id">
          ${badge(c.category)}
          <div class="chk-meta">
            <div class="name">${esc(c.name)}${c.required ? "" : '<span class="tag-optional">not required</span>'}</div>
            <div class="sub mono" title="${esc(c.command)}">${esc(c.command)}${who}</div>
          </div>
        </div>
        ${chip}
        <div class="chk-actions">${action}</div>
      </div>`;
    });

    return `<div class="checks-card">
      <div class="chk-head"><div>Check</div><div>Status</div><div></div></div>
      ${rows.join("")}
    </div>`;
  }

  function terminalBlock() {
    const body = termLines
      .map((l) => `<div class="${l.cls || ""}">${esc(l.text)}</div>`)
      .join("");
    return `<div class="term-wrap ${termOpen ? "open" : ""}" id="termWrap">
      <div class="term-head">
        <span>Command output</span>
        <span class="spacer"></span>
        <button class="btn ghost" type="button" id="termClose">Hide</button>
      </div>
      <div class="term" id="term">${body}</div>
    </div>`;
  }

  function footnoteBlock(rules) {
    if (!rules.readable) {
      return `<div class="section-title">Required for merge</div>
        <div class="footnote">Branch protection for <span class="code">${esc(rules.branch)}</span> isn't readable with your permissions — that endpoint needs admin. Readiness above assumes the default <span class="code">signoff</span> context.</div>`;
    }
    if (!rules.signoffContexts.length) {
      return `<div class="section-title">Required for merge</div>
        <div class="footnote">No signoff context is required on <span class="code">${esc(rules.branch)}</span>.</div>`;
    }
    const chips = rules.signoffContexts
      .map((c) => `<span class="code">${esc(c.replace(/^signoff\//, ""))}</span>`)
      .join(" ");
    return `<div class="section-title">Required for merge</div>
      <div class="footnote">
        Branch protection on <span class="code">${esc(rules.branch)}</span> requires ${chips}.
        Set via <span class="code">gh signoff install</span> — see
        <a href="#" data-nav="rules">Required checks</a>.
      </div>`;
  }

  function historyPage() {
    const events = state.history || [];
    if (!events.length) {
      return `<div class="section-title" style="margin-bottom:14px">Signoff activity</div>
        ${empty("No signoff activity on this commit", "Every signoff written to this commit shows up here, including failures and re-signoffs.")}`;
    }
    const rows = events
      .map((e) => {
        const cls = e.state === "success" ? "" : e.state === "pending" ? "p" : "f";
        const detail = e.state === "success"
          ? `signed off by <b>${esc(e.actor || "unknown")}</b>`
          : `${esc(e.state)}${e.detail ? ` — ${esc(e.detail)}` : ""}`;
        return `<div class="history-row">
          <div class="hdot ${cls}"></div>
          <div class="t">${esc(clockTime(e.at))}</div>
          <div>${esc(e.name)} <span class="who">— ${detail}</span></div>
        </div>`;
      })
      .join("");
    return `<div class="section-title" style="margin-bottom:14px">Signoff activity on this commit</div>${rows}`;
  }

  function rulesPage() {
    const r = state.rules;
    let html = `<div class="section-title" style="margin-bottom:14px">Required checks</div>`;

    if (!r.readable) {
      return html + empty(
        "Branch protection isn't readable",
        "GitHub only exposes branch protection to repository admins. Signoff shows what it can read and never guesses at the rest.",
        `gh api repos/${state.repo.nameWithOwner}/branches/${r.branch}/protection`
      );
    }

    const contexts = r.contexts || [];
    const rows = contexts.length
      ? contexts
          .map((c) => {
            const isSignoff = c.startsWith("signoff");
            return `<div class="rule-row">
              <span class="rr-name">${isSignoff ? badge("default") : ""}<span>${esc(c)}</span></span>
              <span class="state-chip ${isSignoff ? "on" : ""}">${isSignoff ? "signoff" : "external check"}</span>
            </div>`;
          })
          .join("")
      : `<div class="rule-row"><span class="rr-name">No required status checks</span></div>`;

    html += `<div class="rules-card">
      <div class="sc-head">
        <h3>${esc(r.branch)}</h3>
        <p><code class="cmd-chip">${esc(r.installCommand)}</code></p>
      </div>
      ${rows}
    </div>`;

    html += `<div class="footnote" style="margin-top:12px">
      Read-only. Editing protection rewrites the whole required-checks list, so v1 shows the
      <span class="code">gh signoff install</span> command to run rather than a switch that silently
      re-issues it.
    </div>`;
    return html;
  }

  // ------------------------------------------------------------- interaction

  function wireDashboard() {
    document.querySelectorAll("[data-signoff]").forEach((btn) => {
      btn.addEventListener("click", () => signoff(btn.dataset.signoff, btn));
    });
    document.querySelectorAll("[data-copy]").forEach((btn) => {
      btn.addEventListener("click", async () => {
        await navigator.clipboard.writeText(btn.dataset.copy);
        const old = btn.textContent;
        btn.textContent = "Copied";
        setTimeout(() => { btn.textContent = old; }, 1200);
      });
    });
    document.querySelectorAll("[data-nav]").forEach((a) => {
      a.addEventListener("click", (e) => { e.preventDefault(); go(a.dataset.nav); });
    });
    const close = $("#termClose");
    if (close) close.addEventListener("click", () => { termOpen = false; render(); });
  }

  function log(text, cls) {
    termLines.push({ text, cls });
    const term = $("#term");
    if (!term) return;
    const div = document.createElement("div");
    if (cls) div.className = cls;
    div.textContent = text;
    term.appendChild(div);
    term.scrollTop = term.scrollHeight;
  }

  async function signoff(name, btn) {
    if (busy) return;
    busy = true;
    if (btn) { btn.disabled = true; btn.textContent = "Signing off…"; }

    termLines = [];
    termOpen = true;
    const wrap = $("#termWrap");
    if (wrap) wrap.classList.add("open");
    const term = $("#term");
    if (term) term.innerHTML = "";

    announce(`Signing off ${name || "signoff"}`);

    try {
      const res = await fetch("/api/signoff", {
        method: "POST",
        headers: { "Content-Type": "application/json", "X-Signoff-Token": TOKEN },
        body: JSON.stringify({ name }),
      });
      if (!res.ok || !res.body) throw new Error(`request failed: ${res.status}`);

      const reader = res.body.getReader();
      const decoder = new TextDecoder();
      let buf = "";
      let exitCode = 0;

      while (true) {
        const { done, value } = await reader.read();
        if (done) break;
        buf += decoder.decode(value, { stream: true });

        let idx;
        while ((idx = buf.indexOf("\n\n")) !== -1) {
          const frame = buf.slice(0, idx);
          buf = buf.slice(idx + 2);

          let event = "message", data = "";
          for (const line of frame.split("\n")) {
            if (line.startsWith("event: ")) event = line.slice(7);
            else if (line.startsWith("data: ")) data = line.slice(6);
          }
          const text = data.replace(/\\n/g, "\n");

          if (event === "line") {
            const cls = text.startsWith("$") ? "prompt" : text.startsWith("✓") ? "ok" : "";
            log(text, cls);
          } else if (event === "done") {
            try { exitCode = JSON.parse(text).code ?? 0; } catch { exitCode = 0; }
          }
        }
      }

      if (exitCode === 0) {
        log(`✓ exit 0`, "ok");
        announce("Signed off");
        const row = document.querySelector(`.chk-row[data-key="${CSS.escape(name || "signoff")}"]`);
        if (row) row.classList.add("flash");
        setTimeout(fetchState, 700);
      } else {
        log(`✗ exit ${exitCode}`, "err");
        announce("Sign off failed");
        if (btn) { btn.disabled = false; btn.textContent = "Retry sign off"; }
      }
    } catch (err) {
      log(String(err), "err");
      if (btn) { btn.disabled = false; btn.textContent = "Retry sign off"; }
    } finally {
      busy = false;
    }
  }

  function go(next) {
    page = next;
    document.querySelectorAll(".page").forEach((p) => p.classList.add("hidden"));
    $(`#page-${next}`).classList.remove("hidden");
    document.querySelectorAll(".nav-item[data-page]").forEach((n) => {
      if (n.dataset.page === next) n.setAttribute("aria-current", "page");
      else n.removeAttribute("aria-current");
    });
  }

  document.querySelectorAll(".nav-item[data-page]").forEach((n) => {
    n.addEventListener("click", () => go(n.dataset.page));
  });

  // ---------------------------------------------------------------- palette

  let filtered = [], selIdx = 0;

  function paletteActions() {
    const acts = [];
    (state?.checks || []).forEach((c) => {
      if (c.state !== "success") {
        acts.push({
          label: `Sign off: ${c.name}`,
          sub: c.command,
          run: () => signoff(c.name === "signoff" ? "" : c.name, null),
        });
      }
      acts.push({
        label: `Copy command: ${c.command}`,
        sub: "clipboard",
        run: () => navigator.clipboard.writeText(c.command),
      });
    });
    acts.push({ label: "Go to Dashboard", sub: "nav", run: () => go("dashboard") });
    acts.push({ label: "Go to History", sub: "nav", run: () => go("history") });
    acts.push({ label: "Go to Required checks", sub: "nav", run: () => go("rules") });
    acts.push({ label: "Refresh", sub: "reload state", run: fetchState });
    if (state?.pr?.url) {
      acts.push({ label: "Open pull request on GitHub", sub: "browser", run: () => window.open(state.pr.url, "_blank", "noopener") });
    }
    return acts;
  }

  function openPalette() {
    $("#paletteOverlay").classList.add("show");
    const input = $("#paletteInput");
    input.value = "";
    filtered = paletteActions();
    selIdx = 0;
    renderPalette();
    setTimeout(() => input.focus(), 40);
  }

  function closePalette() {
    $("#paletteOverlay").classList.remove("show");
  }

  function renderPalette() {
    const wrap = $("#paletteResults");
    if (!filtered.length) {
      wrap.innerHTML = `<div class="palette-empty">No matching command</div>`;
      return;
    }
    wrap.innerHTML = filtered
      .map(
        (a, i) => `<button class="palette-item" type="button" role="option"
          aria-selected="${i === selIdx}" data-idx="${i}">
          <span class="pi-ic">›</span>
          <span class="pi-label">${esc(a.label)}</span>
          <span class="pi-sub">${esc(a.sub)}</span>
        </button>`
      )
      .join("");
    wrap.querySelectorAll(".palette-item").forEach((el) => {
      el.addEventListener("click", () => {
        const a = filtered[Number(el.dataset.idx)];
        closePalette();
        a.run();
      });
    });
  }

  $("#paletteBtn").addEventListener("click", openPalette);
  $("#paletteOverlay").addEventListener("click", (e) => {
    if (e.target === $("#paletteOverlay")) closePalette();
  });
  $("#paletteInput").addEventListener("input", (e) => {
    const q = e.target.value.toLowerCase();
    filtered = paletteActions().filter((a) => a.label.toLowerCase().includes(q));
    selIdx = 0;
    renderPalette();
  });

  document.addEventListener("keydown", (e) => {
    const overlay = $("#paletteOverlay");
    const isOpen = overlay.classList.contains("show");
    if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k") {
      e.preventDefault();
      isOpen ? closePalette() : openPalette();
      return;
    }
    if (!isOpen) return;
    if (e.key === "Escape") closePalette();
    else if (e.key === "ArrowDown") { e.preventDefault(); selIdx = Math.min(selIdx + 1, filtered.length - 1); renderPalette(); }
    else if (e.key === "ArrowUp") { e.preventDefault(); selIdx = Math.max(selIdx - 1, 0); renderPalette(); }
    else if (e.key === "Enter" && filtered[selIdx]) { const a = filtered[selIdx]; closePalette(); a.run(); }
  });

  // ---------------------------------------------------------------- polling

  // Poll while focused, and immediately on regaining focus. Acceptance
  // criterion §14: no manual refresh while the window is focused.
  let timer = null;
  function startPolling() {
    stopPolling();
    timer = setInterval(() => { if (!busy) fetchState(); }, 15000);
  }
  function stopPolling() { if (timer) clearInterval(timer); timer = null; }

  document.addEventListener("visibilitychange", () => {
    if (document.hidden) stopPolling();
    else { fetchState(); startPolling(); }
  });
  window.addEventListener("focus", () => { if (!busy) fetchState(); });

  fetchState().then(startPolling);
})();
