// indiff UI. Plain JS, no build step. Talks to the Rust engine through Tauri;
// outside the app (a browser) it runs on bundled demo data.
(() => {
  'use strict';

  const T = window.__TAURI__;
  const inApp = !!(T && T.core && T.core.invoke);
  const DEMO = window.INDIFF_DEMO;

  // ---------- backend ----------
  function demoFile(path, side) {
    const lines = {};
    let max = 0;
    for (const h of DEMO.diff.hunks) {
      const f = side === 'old' ? h.old_file : h.file;
      if (f !== path) continue;
      for (const l of h.lines) {
        const n = side === 'old' ? l.old : l.new;
        if (n) { lines[n] = l.text; max = Math.max(max, n); }
      }
    }
    if (!max) return null;
    const out = [];
    for (let i = 1; i <= max; i++) out.push(i in lines ? lines[i] : '…');
    return out.join('\n');
  }
  function demo(cmd, a) {
    switch (cmd) {
      case 'launch_args': return { repo: DEMO.diff.repo, revs: ['main..feature/webhook-retry'], staged: false };
      case 'load_diff': return DEMO.diff;
      case 'diff_key': return DEMO.diff.diff_key;
      case 'run_review': return new Promise((r) => setTimeout(() => r(DEMO.review), 700));
      case 'get_settings': return { provider: '', model: '', base_url: '', speed: 'fast', has_key: true, key_from_env: false };
      case 'save_settings': return { provider: a.update.provider, model: a.update.model, base_url: a.update.base_url, speed: a.update.speed, has_key: true, key_from_env: false };
      case 'list_transcripts': return [];
      case 'list_branches': return ['main', 'feature/webhook-retry'];
      case 'read_file': return demoFile(a.path, a.side);
      case 'prompt_preview': return 'The exact prompt is shown here in the app. This is demo data.';
      case 'apply_fix': return [...new Set(a.fix.edits.map((e) => e.file))];
      case 'open_in_claude': return Promise.reject('The demo can’t open a terminal.');
      default: return null;
    }
  }
  const call = (cmd, args = {}) => (inApp ? T.core.invoke(cmd, args) : Promise.resolve(demo(cmd, args)));

  // ---------- state ----------
  const S = {
    repo: null, revs: '', staged: false,
    diff: null, loading: false, loadError: null,
    out: null, reviewing: false, reviewError: null,
    ai: true, aim: 'all', sel: null, expanded: {}, reviewed: {},
    intent: '', transcripts: [], transcript: '', branches: [],
    settings: null, modal: null, file: null, prompt: null,
    // Proposed fixes by finding id: { state: 'applying' | 'applied' | 'error', files, msg }.
    fixes: {},
    // The files changed after the review: the diff was refreshed, the review wasn't.
    // newSince holds the ids of hunks the review never saw.
    outStale: false, newSince: new Set(),
    // Compare with whitespace changes ignored (git diff -w). Remembered between runs.
    ignoreWs: (() => { try { return localStorage.getItem('indiff:ignoreWs') === '1'; } catch { return false; } })(),
    // Extra context lines shown around a hunk: { [hunkId]: { above, below } }.
    ctx: {},
    // File contents fetched for context, by 'side|path': string, or null when missing.
    files: {},
    // Last Claude Code handoff: { key, state: 'opening' | 'opened' | 'copied' | 'error', msg }.
    handoff: null,
  };

  const K = {
    bug: { sym: '●', label: 'Bug', color: 'var(--red)' },
    security: { sym: '◆', label: 'Security', color: 'var(--red)' },
    scope_creep: { sym: '✱', label: 'Not asked for', color: 'var(--red)' },
    needs_decision: { sym: '▲', label: 'Needs you', color: 'var(--orange)' },
    test_gap: { sym: '○', label: 'Test gap', color: 'var(--orange)' },
    achieved: { sym: '✓', label: 'Achieved', color: 'var(--green)' },
    note: { sym: '·', label: 'Note', color: 'var(--muted)' },
  };
  const RANK = { bug: 0, security: 1, scope_creep: 2, needs_decision: 3, test_gap: 4, note: 5, achieved: 6 };
  const kind = (k) => K[k] || K.note;
  const STATUS = { achieved: ['✓', 'Achieved', 'var(--green)'], partial: ['◐', 'Partly achieved', 'var(--orange)'], not_achieved: ['✗', 'Not achieved', 'var(--red)'], unclear: ['?', 'Unclear', 'var(--muted)'] };

  const esc = (s) => String(s ?? '').replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
  const revList = () => S.revs.trim().split(/\s+/).filter(Boolean);
  const base = () => ({ repo: S.repo, revs: revList(), staged: S.staged });
  const withWs = () => ({ ...base(), ignoreWs: S.ignoreWs });

  // A hunk's identity by content, not position, so state like "reviewed" survives
  // a refresh that renumbers or shifts hunks. Two 32-bit FNV-1a hashes.
  const hashes = new WeakMap();
  function hunkHash(h) {
    let v = hashes.get(h);
    if (v) return v;
    const text = h.file + '\0' + h.lines.map((l) => l.kind + l.text).join('\n');
    let a = 0x811c9dc5;
    let b = 0x01000193 ^ text.length;
    for (let i = 0; i < text.length; i++) {
      const c = text.charCodeAt(i);
      a = Math.imul(a ^ c, 0x01000193);
      b = Math.imul(b ^ c, 0x5bd1e995);
    }
    v = (a >>> 0).toString(16) + (b >>> 0).toString(16);
    hashes.set(h, v);
    return v;
  }

  // Rendered line HTML, which is the slow part of a render. Cleared when the diff changes.
  const htmlCache = new Map();
  function cached(key, make) {
    if (!htmlCache.has(key)) htmlCache.set(key, make());
    return htmlCache.get(key);
  }
  const ago = (secs) => {
    const d = Date.now() / 1000 - secs;
    if (d < 3600) return `${Math.max(1, Math.round(d / 60))} min ago`;
    if (d < 86400) return `${Math.round(d / 3600)} h ago`;
    return `${Math.round(d / 86400)} days ago`;
  };
  // Reviewed marks, per repository and keyed by hunk content, so an unchanged hunk
  // stays reviewed when other files change.
  const store = {
    key() { return S.diff ? `indiff:reviewed2:${S.diff.repo}` : null; },
    read() { try { return JSON.parse(localStorage.getItem(this.key()) || '{}'); } catch { return {}; } },
    load() {
      const m = S.diff ? this.read() : {};
      S.reviewed = {};
      if (S.diff) for (const h of S.diff.hunks) if (m[hunkHash(h)]) S.reviewed[h.id] = true;
    },
    save() {
      if (!S.diff) return;
      const m = this.read();
      for (const h of S.diff.hunks) {
        if (S.reviewed[h.id]) m[hunkHash(h)] = Date.now(); else delete m[hunkHash(h)];
      }
      // Keep the newest few thousand.
      const keep = Object.entries(m).sort((a, b) => b[1] - a[1]).slice(0, 3000);
      try { localStorage.setItem(this.key(), JSON.stringify(Object.fromEntries(keep))); } catch { /* storage unavailable */ }
    },
  };

  // ---------- derived data ----------
  function derive() {
    const d = S.diff;
    const rv = S.out && S.out.review;
    const ai = S.ai && !!rv;
    const findings = rv ? rv.findings.slice().sort((a, b) => (RANK[a.kind] ?? 5) - (RANK[b.kind] ?? 5)) : [];
    const byId = Object.fromEntries(findings.map((f) => [f.id, f]));
    const key = (side, file, line) => `${side === 'old' ? 'old' : 'new'}|${file}|${line}`;
    const lineFindings = {};
    const anchors = {};
    // After the files change, a citation's line may hold different code. Only mark
    // lines that still contain the quoted code.
    let still = () => true;
    if (S.outStale && d) {
      const text = {};
      for (const h of d.hunks) {
        for (const l of h.lines) {
          if (l.new && l.kind !== '-') text[key('new', h.file, l.new)] = l.text;
          if (l.old && l.kind === '-') text[key('old', h.old_file, l.old)] = l.text;
        }
      }
      still = (k, c) => k in text && text[k].includes((c.quote || '').split('\n')[0].trim());
    }
    for (const f of findings) {
      f.evidence.forEach((c, i) => {
        const k = key(c.side, c.file, c.line);
        if (!still(k, c)) return;
        (lineFindings[k] = lineFindings[k] || []).push(f);
        if (i === 0 && f.kind !== 'achieved' && !anchors[k]) anchors[k] = f;
      });
    }
    const sel = S.sel && byId[S.sel] ? byId[S.sel] : null;
    const selKeys = new Set(sel ? sel.evidence.map((c) => key(c.side, c.file, c.line)) : []);
    const aims = rv ? rv.aims : [];
    const linked = new Set(aims.flatMap((a) => a.hunks));
    const orphans = d ? d.hunks.filter((h) => !linked.has(h.id) && !S.newSince.has(h.id)).map((h) => h.id) : [];
    const must = rv ? rv.attention.must_read : [];
    return { d, rv, ai, findings, byId, key, lineFindings, anchors, sel, selKeys, aims, orphans, must };
  }

  // ---------- rendering ----------
  const app = document.getElementById('app');

  function render() {
    if (!S.repo) { app.innerHTML = renderWelcome(); return; }
    const x = derive();
    const scroll = [...app.querySelectorAll('.col')].map((c) => c.scrollTop);
    app.innerHTML = renderTop(x) + `<div class="cols">
      <aside class="col left" aria-label="Intent and aims">${renderLeft(x)}</aside>
      <main class="col mid" aria-label="Diff">${renderMid(x)}</main>
      <aside class="col right" aria-label="Findings">${renderRight(x)}</aside></div>` + renderModal(x);
    app.querySelectorAll('.col').forEach((c, i) => { if (scroll[i]) c.scrollTop = scroll[i]; });
    if (S.modal === 'file') {
      const hl = app.querySelector('.modal .ln.hl');
      if (hl) hl.scrollIntoView({ block: 'center' });
    }
  }

  function renderWelcome() {
    return `<div class="top" data-tauri-drag-region><span class="brand">indiff</span></div>
    <div class="welcome"><div class="card">
      <h1>Review a change</h1>
      <p class="muted">Open a git repository to see its changes with the review layered on top. Next time, run <code>git indiff</code> in a repository to open it straight away.</p>
      <div><button class="btn primary" data-a="open-repo">Open repository…</button></div>
      ${!inApp ? '<p class="small muted">Running outside the app, so demo data is shown.</p>' : ''}
    </div></div>`;
  }

  function renderTop(x) {
    const d = x.d;
    const name = S.repo.split(/[\\/]/).filter(Boolean).pop();
    const mustDone = x.must.filter((h) => S.reviewed[h]).length;
    let status = '';
    if (d && x.rv && S.ai) {
      status = x.must.length
        ? `<span class="stat ${mustDone < x.must.length ? 'warn-text' : ''}">${mustDone} of ${x.must.length} must-read hunks reviewed</span>`
        : '<span class="stat">Nothing flagged as must-read</span>';
    } else if (d) {
      status = `<span class="stat">${Object.values(S.reviewed).filter(Boolean).length} of ${d.hunks.length} hunks reviewed</span>`;
    }
    const reviewBtn = S.reviewing
      ? '<button class="btn" disabled>Reviewing…</button>'
      : `<button class="btn" data-a="review" data-force="${S.out ? '1' : ''}" ${!d || !d.hunks.length ? 'disabled' : ''}>${S.out ? 'Re-run review' : 'Review'}</button>`;
    return `<header class="top" data-tauri-drag-region>
      <span class="brand" data-tauri-drag-region>indiff</span>
      <button class="repo-btn" data-a="open-repo" title="${esc(S.repo)}">${esc(name)}</button>
      <form class="revs" data-a="load-form">
        <label class="small muted" for="revs">Compare</label>
        <input id="revs" name="revs" list="branches" value="${esc(S.revs)}" placeholder="working tree vs HEAD" spellcheck="false" autocomplete="off">
        <datalist id="branches">${S.branches.flatMap((b) => [`<option value="main..${esc(b)}">`, `<option value="${esc(b)}">`]).join('')}</datalist>
        <button class="btn small" type="submit">Load</button>
      </form>
      <label class="check small" title="Hide changes that only add or remove spaces, tabs or line breaks (git diff -w)"><input type="checkbox" id="ignore-ws" ${S.ignoreWs ? 'checked' : ''}> Ignore whitespace</label>
      ${d ? `<span class="stat">${d.hunks.length} hunks · <span style="color:var(--add-sign)">+${d.adds}</span> <span style="color:var(--del-sign)">−${d.dels}</span></span>` : ''}
      <span class="spacer" data-tauri-drag-region></span>
      ${status}
      ${reviewBtn}
      <button class="btn ${S.ai ? 'primary' : ''}" data-a="toggle-ai" aria-pressed="${S.ai}">AI layer: ${S.ai ? 'on' : 'off'}</button>
      <button class="icon-btn" data-a="settings" aria-label="Settings" title="Settings">
        <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><circle cx="12" cy="12" r="3"/><path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 1 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06A1.65 1.65 0 0 0 4.68 15a1.65 1.65 0 0 0-1.51-1H3a2 2 0 1 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06A1.65 1.65 0 0 0 9 4.68a1.65 1.65 0 0 0 1-1.51V3a2 2 0 1 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06A1.65 1.65 0 0 0 19.4 9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 1 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z"/></svg>
      </button>
    </header>`;
  }

  function renderLeft(x) {
    const d = x.d;
    if (!d) return '';
    const tOpts = [`<option value="">No agent session</option>`].concat(
      S.transcripts.map((t) => `<option value="${esc(t.path)}" ${S.transcript === t.path ? 'selected' : ''}>${esc(t.agent)} · ${esc(ago(t.modified))}${t.first_prompt ? ' · ' + esc(t.first_prompt.slice(0, 50)) : ''}</option>`),
    );
    let html = `<section class="section">
      <div class="label">Intent</div>
      <textarea class="intent" id="intent" placeholder="What was this change meant to do? (Optional. Commit messages and the agent session are read automatically.)" aria-label="Intent">${esc(S.intent)}</textarea>
      ${S.transcripts.length ? `<label class="small muted" for="transcript">Agent session</label><select class="sel" id="transcript">${tOpts.join('')}</select>` : ''}
      ${S.out ? `<div class="small muted">Used: ${esc(S.out.intent_sources.length ? S.out.intent_sources.map(prettySource).join(', ') : 'the code only')}</div>` : ''}
    </section>`;
    if (x.ai && x.aims.length) {
      const count = (ids) => `${ids.length} ${ids.length === 1 ? 'hunk' : 'hunks'}`;
      const worstFor = (aimId) => {
        const fs = x.findings.filter((f) => f.aim === aimId && f.kind !== 'achieved');
        return fs.length ? fs[0] : null;
      };
      const items = [`<button class="aim ${S.aim === 'all' ? 'on' : ''}" data-a="aim" data-id="all" aria-pressed="${S.aim === 'all'}">
        <span class="t">All changes <span class="c">${count(d.hunks)}</span></span>
        <span class="small muted">${x.findings.filter((f) => f.kind !== 'achieved').length} findings</span></button>`];
      for (const a of x.aims) {
        const st = STATUS[a.status] || STATUS.unclear;
        const w = worstFor(a.id);
        const extra = w ? ` · ${kind(w.kind).sym} ${esc(kind(w.kind).label)}` : '';
        items.push(`<button class="aim ${S.aim === a.id ? 'on' : ''}" data-a="aim" data-id="${esc(a.id)}" aria-pressed="${S.aim === a.id}">
          <span class="t">${esc(a.text)} <span class="c">${count(a.hunks)}</span></span>
          <span class="small" style="color:${st[2]}">${st[0]} ${st[1]}${a.source === 'inferred' ? ' · inferred' : ''}<span style="color:${w ? kind(w.kind).color : 'inherit'}">${extra}</span></span></button>`);
      }
      if (x.orphans.length) {
        items.push(`<button class="aim ${S.aim === 'none' ? 'on' : ''}" data-a="aim" data-id="none" aria-pressed="${S.aim === 'none'}">
          <span class="t">Not linked to any aim <span class="c">${count(x.orphans)}</span></span>
          <span class="small" style="color:var(--red)">✱ Check this was wanted</span></button>`);
      }
      html += `<section class="section"><div class="label">Aims</div>${items.join('')}</section>`;
    }
    return html + renderFiles(x);
  }

  // Every changed file, with its counts and worst finding. Clicking one jumps to its first hunk.
  function renderFiles(x) {
    const d = x.d;
    const rows = d.files.map((f) => {
      const path = f.new_path || f.old_path || '?';
      const hs = d.hunks.filter((h) => h.file === path);
      let a = 0;
      let r = 0;
      for (const h of hs) for (const l of h.lines) { if (l.kind === '+') a++; if (l.kind === '-') r++; }
      let worst = null;
      if (x.ai) {
        for (const h of hs) {
          for (const l of h.lines) {
            const k = l.kind === '-' ? x.key('old', h.old_file, l.old) : x.key('new', h.file, l.new);
            const top = x.anchors[k];
            if (top && (!worst || RANK[top.kind] < RANK[worst.kind])) worst = top;
          }
        }
      }
      const done = hs.length && hs.every((h) => S.reviewed[h.id]);
      const renamed = f.old_path && f.new_path && f.old_path !== f.new_path;
      const counts = f.binary ? 'binary'
        : hs.length ? `<span style="color:var(--add-sign)">+${a}</span> <span style="color:var(--del-sign)">−${r}</span>`
        : renamed ? 'renamed'
        : S.ignoreWs ? 'whitespace only' : 'no text changes';
      const status = worst ? `<span style="color:${kind(worst.kind).color}" title="${esc(kind(worst.kind).label)}">${kind(worst.kind).sym}</span>`
        : done ? '<span style="color:var(--green)" title="All hunks reviewed">✓</span>' : '<span></span>';
      const name = path.split('/').pop();
      const dir = path.slice(0, path.length - name.length);
      const label = `<span class="fname" title="${esc(renamed ? f.old_path + ' → ' + f.new_path : path)}">${esc(name)}${dir ? `<span class="dir">${esc(dir.replace(/\/$/, ''))}</span>` : ''}</span>`;
      return hs.length
        ? `<button class="file-item" data-a="goto-file" data-file="${esc(path)}">${status}${label}<span class="cnt">${counts}</span></button>`
        : `<div class="file-item quiet">${status}${label}<span class="cnt">${counts}</span></div>`;
    });
    return `<section class="section"><div class="label">Files <span class="c">${d.files.length}</span></div>${rows.join('')}</section>`;
  }

  function prettySource(s) {
    if (s === '--intent') return 'your note';
    if (s.startsWith('transcript ')) return 'agent session';
    return s;
  }

  function renderMid(x) {
    if (S.loading) return '<div class="empty">Loading diff…</div>';
    if (S.loadError) return `<div class="banner err"><span class="grow">${esc(S.loadError)}</span></div>`;
    const d = x.d;
    if (!d) return '';
    if (!d.hunks.length) {
      return S.ignoreWs && d.files.length
        ? `<div class="empty">Only whitespace changed in ${esc(d.comparison)}. Turn off Ignore whitespace to see it.</div>`
        : `<div class="empty">No changes in ${esc(d.comparison)}.</div>`;
    }
    let html = '';
    if (S.outStale && S.out) {
      const n = S.newSince.size;
      html += `<div class="banner"><span class="grow"><strong>The files changed after this review.</strong> The diff is up to date${n ? `, and ${n} ${n === 1 ? 'hunk the review hasn’t seen is' : 'hunks the review hasn’t seen are'} marked “Changed since review”` : ''}. Findings may be out of date.</span><button class="btn small primary" data-a="review" data-force="1" ${S.reviewing ? 'disabled' : ''}>Re-run review</button></div>`;
    }

    if (S.ai) {
      if (S.settings && !S.settings.has_key && !S.out) {
        html += `<div class="banner"><span class="grow">Add an API key in Settings to review this change. The diff works without one.</span><button class="btn small" data-a="settings">Settings</button></div>`;
      }
      if (S.reviewError) {
        html += `<div class="banner err"><span class="grow"><strong>Review failed.</strong> ${esc(S.reviewError)}</span><button class="btn small" data-a="review" data-force="1">Try again</button></div>`;
      }
      if (S.reviewing) {
        html += `<div class="card card-pad" aria-busy="true"><div class="row-between"><h2>Reviewing ${d.hunks.length} hunks…</h2><span class="small muted">The diff below is ready to read meanwhile</span></div>
          <div class="skeleton" style="width:70%"></div><div class="skeleton" style="width:45%"></div><div class="skeleton" style="width:60%"></div></div>`;
      }
    }

    if (x.ai) {
      html += renderBudget(x) + renderBehaviour(x);
    }

    let hunks = d.hunks.slice();
    if (x.ai && S.aim !== 'all') {
      const ids = S.aim === 'none' ? new Set(x.orphans) : new Set((x.aims.find((a) => a.id === S.aim) || { hunks: [] }).hunks);
      hunks = hunks.filter((h) => ids.has(h.id));
    }
    if (x.ai) {
      // Must-read and unreviewed-by-the-model first, then the rest, keeping diff order within each.
      const first = (h) => (x.must.includes(h.id) || S.newSince.has(h.id) ? 0 : 1);
      hunks.sort((a, b) => first(a) - first(b));
    }
    html += hunks.map((h) => renderHunk(h, x)).join('');
    if (!hunks.length) html += '<div class="empty">No hunks for this aim.</div>';
    return html;
  }

  function renderBudget(x) {
    const d = x.d;
    const must = d.hunks.filter((h) => x.must.includes(h.id));
    const lines = must.reduce((n, h) => n + h.lines.length, 0);
    const done = must.filter((h) => S.reviewed[h.id]).length;
    const mins = x.rv.attention.minutes ? `, about ${Math.max(1, Math.round(x.rv.attention.minutes))} min` : '';
    const pct = must.length ? Math.round((done / must.length) * 100) : 100;
    const rest = d.hunks.length - must.length;
    return `<section class="card card-pad">
      <div class="row-between"><h2>Attention budget</h2>
        <span class="small" style="color:${done === must.length ? 'var(--green)' : 'var(--muted)'}">${must.length ? (done === must.length ? '✓ All must-read hunks reviewed' : `${done} of ${must.length} must-read hunks reviewed`) : ''}</span></div>
      <p style="color:var(--text-2)">${must.length
        ? `Read <strong style="color:var(--text)">${must.length} ${must.length === 1 ? 'hunk' : 'hunks'}</strong> (${lines} lines${mins}).${rest ? ` The other ${rest} ${rest === 1 ? 'is' : 'are'} collapsed with the reason ${rest === 1 ? 'it is' : 'they are'} safe to skip.` : ''}`
        : 'Nothing was flagged as must-read. The hunks are collapsed with the reason each is safe to skip.'}</p>
      <div class="bar" role="progressbar" aria-label="Must-read hunks reviewed" aria-valuemin="0" aria-valuemax="${must.length}" aria-valuenow="${done}"><div style="width:${pct}%"></div></div>
    </section>`;
  }

  function renderBehaviour(x) {
    const rows = x.rv.behaviour;
    if (!rows.length) return '';
    let last = null;
    const body = rows.map((r, i) => {
      const f = r.finding ? x.byId[r.finding] : null;
      const k = f ? kind(f.kind) : null;
      const on = f && x.sel && x.sel.id === f.id;
      const serious = f && f.kind !== 'achieved';
      const head = r.subject !== last ? `<div class="brow subject">${esc(r.subject)}</div>` : '';
      last = r.subject;
      const btn = f
        ? `<button class="chip-btn" data-a="sel" data-id="${esc(f.id)}" style="color:${k.color};${on ? 'border-color:var(--accent)' : ''}" aria-label="Open finding: ${esc(f.title)}">${k.sym} ${esc(k.label)}</button>`
        : `<button class="chip-btn" data-a="cite" data-i="${i}" style="color:var(--muted)" aria-label="Show evidence for ${esc(r.aspect)}">Evidence</button>`;
      return `${head}<div class="brow ${on ? 'on' : ''}">
        <span style="color:var(--text-2)">${esc(r.aspect)}</span>
        <span class="before">${esc(r.before)}</span>
        <span style="color:${serious ? k.color : 'var(--text)'};font-weight:${serious ? 600 : 400}">${esc(r.after)}</span>
        ${btn}</div>`;
    }).join('');
    return `<section class="card" style="overflow:hidden">
      <div class="row-between" style="padding:14px 16px 8px"><h2>Behaviour diff</h2><span class="small muted">Reasoned from code, not run · every row has checked evidence</span></div>
      <div class="bdiff"><div class="bgrid">
        <div class="brow head"><span>Behaviour</span><span>Before</span><span>After</span><span>Finding</span></div>${body}
      </div></div></section>`;
  }

  // ---------- hunks ----------
  // The lines a hunk covers on one side, [first, last]. When the hunk has no lines on
  // that side (a pure insertion or deletion), the header says where it sits: [at + 1, at].
  function span(h, side) {
    const ns = h.lines.map((l) => (side === 'old' ? l.old : l.new)).filter((n) => n);
    if (ns.length) return [ns[0], ns[ns.length - 1]];
    const m = h.header.match(side === 'old' ? /-(\d+)/ : /\+(\d+)/);
    const at = m ? +m[1] : 0;
    return [at + 1, at];
  }

  // Extra context comes from the new version of the file, or the old one when it was deleted.
  function ctxSide(h) {
    const f = S.diff.files.find((f) => (f.new_path || f.old_path) === h.file);
    return f && !f.new_path ? 'old' : 'new';
  }

  function fileLines(text) {
    const a = text.split('\n');
    if (a.length && a[a.length - 1] === '') a.pop();
    return a;
  }

  // Context lines the user asked to see around a hunk, stopping at the file's ends
  // and at the neighbouring hunks. left: how many more there are (null when unknown).
  function extraLines(h) {
    const side = ctxSide(h);
    const other = side === 'old' ? 'new' : 'old';
    const path = side === 'old' ? h.old_file : h.file;
    const text = S.files[side + '|' + path];
    const all = typeof text === 'string' ? fileLines(text) : null;
    const same = S.diff.hunks.filter((o) => (side === 'old' ? o.old_file : o.file) === path);
    const i = same.indexOf(h);
    const [first, last] = span(h, side);
    const [oFirst, oLast] = span(h, other);
    const lo = i > 0 ? span(same[i - 1], side)[1] + 1 : 1;
    const hi = i < same.length - 1 ? span(same[i + 1], side)[0] - 1 : (all ? all.length : Infinity);
    const c = S.ctx[h.id] || { above: 0, below: 0 };
    const from = all ? Math.max(lo, first - c.above) : first;
    const to = all ? Math.min(hi, last + c.below) : last;
    const make = (n, at, oAt) => ({ kind: ' ', text: all[n - 1], [side]: n, [other]: side === 'old' ? null : n - at + oAt });
    const above = [];
    const below = [];
    for (let n = from; n < first; n++) above.push(make(n, first, oFirst));
    for (let n = last + 1; n <= to; n++) below.push(make(n, last, oLast));
    const code = (ls) => (ls.length
      ? cached(`x|${S.diff.diff_key}|${side}|${path}|${ls[0][side]}|${ls.length}`, () => (window.IndiffCode ? IndiffCode.highlight(path, ls.map((l) => l.text)) : ls.map((l) => esc(l.text))))
      : []);
    return {
      above, below, aboveHtml: code(above), belowHtml: code(below),
      left: {
        above: text === null ? 0 : Math.max(0, from - lo),
        below: text === null ? 0 : all ? Math.max(0, hi - to) : null,
      },
    };
  }

  function moreBtn(h, dir, ex) {
    const left = ex.left[dir];
    if (left === 0) return '';
    const n = left != null && left <= 20 ? left : 20;
    const label = left == null ? 'Show more lines' : `Show ${n} more ${n === 1 ? 'line' : 'lines'}`;
    return `<button class="ctx-more" data-a="more-ctx" data-id="${h.id}" data-dir="${dir}">${dir === 'above' ? '↑' : '↓'} ${label}</button>`;
  }

  // One diff line, with its finding marker and the note under a finding's first citation.
  function lineRow(h, l, code, x, acc, extra) {
    const ai = x.ai;
    const kNew = l.new ? x.key('new', h.file, l.new) : null;
    const kOld = l.old ? x.key('old', h.old_file, l.old) : null;
    const fs = ai ? [...(kNew && l.kind !== '-' ? x.lineFindings[kNew] || [] : []), ...(kOld && l.kind === '-' ? x.lineFindings[kOld] || [] : [])] : [];
    fs.sort((a, b) => RANK[a.kind] - RANK[b.kind]);
    const top = fs[0];
    const anchor = ai ? ((kNew && l.kind !== '-' && x.anchors[kNew]) || (kOld && l.kind === '-' && x.anchors[kOld]) || null) : null;
    if (anchor) { acc.issues++; if (!acc.worst || RANK[anchor.kind] < RANK[acc.worst.kind]) acc.worst = anchor; }
    const hl = ai && ((kNew && l.kind !== '-' && x.selKeys.has(kNew)) || (kOld && l.kind === '-' && x.selKeys.has(kOld)));
    const cls = ['ln', l.kind === '+' ? 'add' : l.kind === '-' ? 'del' : '', hl ? 'hl' : '', extra ? 'extra' : ''].join(' ');
    const marker = top
      ? `<button data-a="sel" data-id="${esc(top.id)}" style="color:${kind(top.kind).color}" aria-label="${esc(kind(top.kind).label)}: ${esc(top.title)}" title="${esc(kind(top.kind).label)}: ${esc(top.title)}">${kind(top.kind).sym}</button>`
      : '';
    const note = anchor
      ? `<div class="note ${x.sel && x.sel.id === anchor.id ? 'on' : ''}" style="border-color:${kind(anchor.kind).color}">
          <span style="color:${kind(anchor.kind).color}">${kind(anchor.kind).sym}</span>
          <strong style="color:${kind(anchor.kind).color}">${esc(kind(anchor.kind).label)}</strong>
          <span class="ttl">${esc(anchor.title)}</span>
          <button class="btn small" data-a="sel" data-id="${esc(anchor.id)}">Open</button></div>`
      : '';
    return `<div><div class="${cls}"><span class="g">${marker}</span><span class="o">${l.old ?? ''}</span><span class="n">${l.new ?? ''}</span><span class="s">${l.kind === '+' ? '+' : l.kind === '-' ? '−' : ' '}</span><span class="x">${code || ' '}</span></div>${note}</div>`;
  }

  function renderHunk(h, x) {
    const ai = x.ai;
    const must = x.must.includes(h.id);
    const fresh = S.newSince.has(h.id);
    const collapsed = ai && !must && !fresh && !S.expanded[h.id];
    const rev = !!S.reviewed[h.id];
    const acc = { issues: 0, worst: null };
    const code = cached(`h|${x.d.diff_key}|${h.id}`, () => (window.IndiffCode ? IndiffCode.hunkLines(h) : h.lines.map((l) => esc(l.text))));
    const body = h.lines.map((l, i) => lineRow(h, l, code[i], x, acc, false)).join('');
    let lines = '';
    if (!collapsed) {
      const ex = extraLines(h);
      lines = moreBtn(h, 'above', ex)
        + ex.above.map((l, i) => lineRow(h, l, ex.aboveHtml[i], x, acc, true)).join('')
        + body
        + ex.below.map((l, i) => lineRow(h, l, ex.belowHtml[i], x, acc, true)).join('')
        + moreBtn(h, 'below', ex);
    }

    const tags = ai
      ? x.aims.filter((a) => a.hunks.includes(h.id)).map((a) => `<span class="tag">${esc(a.text)}</span>`).join('') +
        (x.orphans.includes(h.id) ? '<span class="tag creep">Not linked to any aim</span>' : '')
      : '';
    const freshChip = S.out && fresh ? '<span class="chip" style="color:var(--orange)">Changed since review</span>' : '';
    const risk = ai && acc.worst ? `<span class="chip" style="color:${kind(acc.worst.kind).color}">${kind(acc.worst.kind).sym} ${acc.issues} ${acc.issues === 1 ? 'finding' : 'findings'}</span>` : '';
    const mustChip = ai && must ? '<span class="chip" style="color:var(--accent-text)">Must read</span>' : '';
    const collapseBtn = ai && !must && !fresh && S.expanded[h.id] ? `<button class="btn small" data-a="expand" data-id="${h.id}">Collapse</button>` : '';
    const reason = (x.rv && x.rv.attention.skip_reasons[h.id]) || 'No findings';
    const changed = h.lines.filter((l) => l.kind !== ' ').length;

    return `<article class="hunk" id="hunk-${h.id}" data-file="${esc(h.file)}">
      <div class="hunk-head">
        <span class="file">${esc(h.file)}</span><span class="range">${esc(h.header.replace(/^@@ (.*?) @@.*$/, '$1'))}</span>
        <span style="display:flex;flex-wrap:wrap;gap:6px">${tags}</span>
        <span class="hunk-actions">${freshChip}${mustChip}${risk}${collapseBtn}
        <button class="btn small ${rev ? 'ok' : ''}" data-a="reviewed" data-id="${h.id}" aria-pressed="${rev}">${rev ? '✓ Reviewed' : 'Mark reviewed'}</button></span>
      </div>
      ${collapsed
        ? `<div class="collapsed"><span style="color:var(--green)">✓</span><span class="why">Collapsed · ${esc(reason)}</span><button class="btn small" data-a="expand" data-id="${h.id}">Show ${changed} changed lines</button></div>`
        : `<div class="code">${lines}</div>`}
    </article>`;
  }

  function renderRight(x) {
    if (!x.d) return '';
    if (!S.ai) return '<div class="banner muted">AI layer off. This is a plain diff in file order, with nothing hidden or reordered.</div>' + renderImpact(x);
    if (!x.rv) {
      return (S.reviewing ? '<div class="section"><div class="label">Findings</div><div class="skeleton"></div><div class="skeleton" style="width:80%"></div></div>' : '') + renderImpact(x);
    }
    let html = x.sel ? renderDetail(x, x.sel) : renderOverview(x);
    html += renderImpact(x) + renderCheck(x);
    return html;
  }

  function renderOverview(x) {
    let fs = x.findings;
    if (S.aim === 'none') fs = fs.filter((f) => !f.aim);
    else if (S.aim !== 'all') fs = fs.filter((f) => f.aim === S.aim);
    const aimText = (id) => (x.aims.find((a) => a.id === id) || {}).text;
    return `<section class="section">
      <div class="label">Verdict</div>
      ${x.rv.summary ? `<p><strong>${esc(x.rv.summary)}</strong></p>` : ''}
      ${x.rv.verdict ? `<p style="color:var(--text-2)">${esc(x.rv.verdict)}</p>` : ''}
      <div class="small muted">${esc(S.out.model)}${S.out.speed === 'thorough' ? ' · thorough' : ''}${S.out.cached ? ' · cached' : ''}${S.out.elapsed_ms != null ? ' · ' + (S.out.elapsed_ms / 1000).toFixed(1) + ' s' : ''}${S.out.output_tokens != null ? ' · ' + (S.out.output_tokens >= 1000 ? (S.out.output_tokens / 1000).toFixed(1) + 'k' : S.out.output_tokens) + ' tokens written' : ''}${S.out.context_truncated ? ' · some context was truncated' : ''}</div>
    </section>
    <section class="section"><div class="label">Findings</div>
      ${handoffBar(x.findings.filter((f) => HANDOFF_KINDS.includes(f.kind) && !(S.fixes[f.id] && S.fixes[f.id].state === 'applied')), 'all')}
      ${fs.length ? fs.map((f) => `<button class="finding-btn" data-a="sel" data-id="${esc(f.id)}">
        <span class="sym" style="color:${kind(f.kind).color}">${kind(f.kind).sym}</span>
        <span class="body"><span style="font-weight:500">${esc(f.title)}</span>
        <span class="meta">${esc(kind(f.kind).label)}${f.aim && aimText(f.aim) ? ' · ' + esc(aimText(f.aim)) : ''}${f.fix ? (S.fixes[f.id] && S.fixes[f.id].state === 'applied' ? ' · <span style="color:var(--green)">✓ Fix applied</span>' : ' · Fix proposed') : ''}</span></span></button>`).join('') : '<div class="small muted">No findings for this aim.</div>'}
    </section>`;
  }

  function renderDetail(x, f) {
    const k = kind(f.kind);
    const basis = f.basis === 'cited' ? 'Shown by the cited lines' : 'Inferred from code · not run';
    const aim = f.aim ? x.aims.find((a) => a.id === f.aim) : null;
    const who = { user: 'You asked', agent: 'The agent said', commit: 'Commit message', ticket: 'Ticket' }[f.why.who] || 'Intent';
    return `<section class="section detail">
      <button class="link-btn" data-a="back">← All findings</button>
      <div style="display:flex;flex-wrap:wrap;gap:6px">
        <span class="chip" style="color:${k.color}">${k.sym} ${esc(k.label)}</span>
        <span class="small" style="padding:2px 8px;border-radius:999px;border:1px solid var(--line-2);color:var(--text-2)">${esc(basis)}${f.confidence ? ' · ' + esc(f.confidence) + ' confidence' : ''}</span>
      </div>
      <h2>${esc(f.title)}</h2>
      ${aim ? `<div class="small muted">Aim: ${esc(aim.text)}</div>` : ''}
      ${f.detail ? `<p style="color:var(--text-2);line-height:1.55">${esc(f.detail)}</p>` : ''}
      ${f.suggestion ? `<div class="box"><span class="muted">Suggested: </span>${esc(f.suggestion)}</div>` : ''}
      ${f.fix ? renderFix(f) : ''}
      ${f.kind !== 'achieved' ? handoffBar([f], f.id) : ''}
      <div class="label" style="margin-top:4px">Evidence (highlighted in the diff)</div>
      ${f.evidence.map((c, i) => `<button class="cite" data-a="open-cite" data-id="${esc(f.id)}" data-i="${i}">
        <span class="where">${esc(c.file)}:${c.line}${c.side === 'old' ? ' (old)' : ''} ↗</span><span class="q">${esc(c.quote)}</span></button>`).join('')}
      ${f.why.quote ? `<div class="label" style="margin-top:4px">Why it's like this</div>
        <div class="box"><div class="small muted" style="margin-bottom:4px">${esc(who)}</div><div>“${esc(f.why.quote)}”</div></div>` : ''}
    </section>`;
  }

  // ---------- Claude Code handoff ----------
  // Findings worth sending in bulk. Decisions stay with the user, so needs_decision is per finding only.
  const HANDOFF_KINDS = ['bug', 'security', 'test_gap'];

  function diffCommand() {
    const w = S.ignoreWs ? ' -w' : '';
    if (S.staged) return `git diff${w} --staged`;
    const r = revList();
    return `git diff${w} ` + (r.length ? r.join(' ') : 'HEAD');
  }

  function handoffPrompt(fs) {
    const out = [
      `I'm reviewing a change in indiff (${S.diff.comparison}). Run \`${diffCommand()}\` to see it.`,
      '',
      fs.length === 1 ? 'Please work on this finding from the review.' : `Please work through these ${fs.length} findings from the review, most serious first.`,
      'The review reasoned about the code without running it, so check each finding against the code before changing anything. If one is wrong, say so and leave that code alone. Where a finding needs a decision from me, ask instead of choosing. Run the tests afterwards if the project has them.',
    ];
    if (!S.staged && revList().some((r) => r.includes('..'))) {
      out.push('This compares commits, not the working tree, so check which branch is checked out before editing.');
    }
    for (const f of fs) {
      out.push('', `## ${f.id}: ${f.title} (${kind(f.kind).label})`);
      if (f.detail) out.push(f.detail);
      out.push('Evidence:', ...f.evidence.map((c) => `- ${c.file}:${c.line}${c.side === 'old' ? ' (old version)' : ''}: \`${c.quote}\``));
      if (f.suggestion) out.push(`Suggested: ${f.suggestion}`);
      if (f.fix) {
        out.push(`Proposed fix${f.fix.note ? ` (${f.fix.note})` : ''}, checked to apply cleanly but not run:`);
        for (const e of f.fix.edits) out.push(`In ${e.file} at line ${e.line}, replace`, '```', e.find.replace(/\n$/, ''), '```', 'with', '```', e.replace.replace(/\n$/, ''), '```');
      }
    }
    return out.join('\n') + '\n';
  }

  function handoffBar(fs, key) {
    if (!fs.length) return '';
    const h = S.handoff && S.handoff.key === key ? S.handoff : null;
    const label = key === 'all' ? `Work on ${fs.length} ${fs.length === 1 ? 'finding' : 'findings'} in Claude Code` : 'Work on this in Claude Code';
    const status = !h ? ''
      : h.state === 'opened' ? '<span class="small" style="color:var(--green)">✓ Opened in a terminal</span>'
      : h.state === 'copied' ? '<span class="small" style="color:var(--green)">✓ Prompt copied</span>'
      : h.state === 'error' ? `<span class="small" style="color:var(--red)">${esc(h.msg)}</span>` : '';
    const ids = esc(fs.map((f) => f.id).join(','));
    return `<div class="handoff">
      <button class="btn small" data-a="handoff" data-key="${esc(key)}" data-ids="${ids}" ${h && h.state === 'opening' ? 'disabled' : ''}>${h && h.state === 'opening' ? 'Opening…' : esc(label) + ' ↗'}</button>
      <button class="btn small" data-a="handoff-copy" data-key="${esc(key)}" data-ids="${ids}">Copy prompt</button>
      ${status}</div>`;
  }

  async function copyText(text) {
    try { await navigator.clipboard.writeText(text); return true; } catch { return false; }
  }

  async function handoff(key, ids, copyOnly) {
    const byId = derive().byId;
    const fs = ids.split(',').map((id) => byId[id]).filter(Boolean).sort((a, b) => RANK[a.kind] - RANK[b.kind]);
    if (!fs.length) return;
    const prompt = handoffPrompt(fs);
    if (copyOnly) {
      S.handoff = (await copyText(prompt)) ? { key, state: 'copied' } : { key, state: 'error', msg: 'Couldn’t copy to the clipboard.' };
      return render();
    }
    S.handoff = { key, state: 'opening' };
    render();
    try {
      await call('open_in_claude', { repo: S.repo, prompt });
      S.handoff = { key, state: 'opened' };
    } catch (e) {
      // Still leave the user with something they can paste.
      const copied = await copyText(prompt);
      S.handoff = { key, state: 'error', msg: String(e) + (copied ? ' The prompt was copied to the clipboard.' : '') };
    }
    render();
  }

  function renderFix(f) {
    const st = S.fixes[f.id] || {};
    const split = (s) => s.replace(/\n$/, '').split('\n');
    const hl = (path, lines) => (window.IndiffCode ? IndiffCode.highlight(path, lines) : lines.map(esc));
    const edits = f.fix.edits.map((e) => {
      const before = split(e.find);
      const after = e.replace === '' ? [] : split(e.replace);
      const delHtml = hl(e.file, before);
      const addHtml = hl(e.file, after);
      // Mark changed words when the edit swaps lines one for one.
      if (window.IndiffCode && before.length === after.length) {
        before.forEach((b, i) => {
          const w = IndiffCode.wordDiff(b, after[i]);
          if (w) { delHtml[i] = IndiffCode.overlay(delHtml[i], w[0]); addHtml[i] = IndiffCode.overlay(addHtml[i], w[1]); }
        });
      }
      const del = delHtml.map((t, i) => `<div class="ln del"><span class="n">${e.line + i}</span><span class="s">−</span><span class="x">${t || ' '}</span></div>`).join('');
      const add = addHtml.map((t) => `<div class="ln add"><span class="n"></span><span class="s">+</span><span class="x">${t || ' '}</span></div>`).join('');
      return `<div class="fix-edit">
        <button class="link-btn small" data-a="open-ref" data-file="${esc(e.file)}" data-line="${e.line}">${esc(e.file)}:${e.line} ↗</button>
        <div class="code">${del}${add}</div></div>`;
    }).join('');
    let action;
    if (st.state === 'applied') {
      action = `<div class="small" style="color:var(--green)">✓ Applied to ${esc(st.files.join(', '))}</div>`;
    } else if (f.fix.applicable) {
      action = `<button class="btn primary small" data-a="apply-fix" data-id="${esc(f.id)}" ${st.state === 'applying' ? 'disabled' : ''}>${st.state === 'applying' ? 'Applying…' : 'Apply fix'}</button>`;
    } else {
      action = '<div class="small muted">Fixes can be applied when comparing against the working tree (git indiff with no range).</div>';
    }
    return `<div class="label" style="margin-top:4px">Proposed fix</div>
      <div class="fix">
        ${f.fix.note ? `<div>${esc(f.fix.note)}</div>` : ''}
        ${edits}
        <div class="small muted">Written by the model and checked to apply cleanly, not run or tested.</div>
        ${action}
        ${st.state === 'error' ? `<div class="small" style="color:var(--red)">${esc(st.msg)}</div>` : ''}
      </div>`;
  }

  function renderImpact(x) {
    const refs = x.d.references || [];
    if (!refs.length) return '';
    const bySym = {};
    for (const r of refs) (bySym[r.symbol] = bySym[r.symbol] || []).push(r);
    const blocks = Object.entries(bySym).map(([sym, rs]) => {
      const callers = rs.filter((r) => !r.is_test);
      const tests = rs.filter((r) => r.is_test);
      const ref = (r) => `<button class="ref" data-a="open-ref" data-file="${esc(r.file)}" data-line="${r.line}">
        <span class="where">${esc(r.file)}:${r.line}</span><span class="q">${esc(r.text)}</span></button>`;
      const flagged = (r) => x.findings.some((f) => f.kind !== 'achieved' && f.evidence.some((c) => c.file === r.file && Math.abs(c.line - r.line) <= 1));
      const refWithFlag = (r) => flagged(r) ? ref(r).replace('class="ref"', 'class="ref" style="border-color:var(--orange)"') : ref(r);
      return `${callers.length ? `<div class="small muted">Used by</div>${callers.map(refWithFlag).join('')}<div class="arrow" aria-hidden="true">↓</div>` : ''}
        <div class="node">${esc(sym)}()<div class="small" style="color:var(--accent-text);font-family:var(--sans);margin-top:2px">changed</div></div>
        ${tests.length ? `<div class="arrow" aria-hidden="true">↓</div><div class="small muted">Tests that reach it</div>${tests.map(ref).join('')}` : `<div class="small" style="color:var(--orange)">○ No tests reference it</div>`}`;
    });
    return `<section class="section"><div class="label">Impact</div><div class="small muted">Found by searching the repo, no AI involved.</div>${blocks.join('<div style="height:8px"></div>')}</section>`;
  }

  function renderCheck(x) {
    const c = S.out.check;
    const removed = c.dropped_findings.length + c.dropped_behaviour_rows.length;
    const droppedFixes = c.dropped_fixes || [];
    if (!removed && !c.corrected_citations && !c.dropped_why && !droppedFixes.length) return '';
    const parts = [];
    if (c.dropped_findings.length) parts.push(`${c.dropped_findings.length} ${c.dropped_findings.length === 1 ? 'finding' : 'findings'}`);
    if (c.dropped_behaviour_rows.length) parts.push(`${c.dropped_behaviour_rows.length} behaviour ${c.dropped_behaviour_rows.length === 1 ? 'row' : 'rows'}`);
    return `<section class="section"><div class="label">Citation check</div>
      <div class="box small" style="line-height:1.5">
        ${removed ? `Removed ${parts.join(' and ')} whose quoted code doesn't exist.` : 'Every claim matched the code.'}
        ${c.corrected_citations ? ` Fixed ${c.corrected_citations} line ${c.corrected_citations === 1 ? 'number' : 'numbers'} that ${c.corrected_citations === 1 ? 'was' : 'were'} slightly out.` : ''}
        ${c.dropped_why ? ` Cleared ${c.dropped_why} “why” ${c.dropped_why === 1 ? 'quote' : 'quotes'} not found in the intent.` : ''}
        ${droppedFixes.length ? ` Removed ${droppedFixes.length} proposed ${droppedFixes.length === 1 ? 'fix' : 'fixes'} that didn't match the code.` : ''}
        ${removed || droppedFixes.length ? `<details style="margin-top:6px"><summary class="muted">What was removed</summary><ul style="margin:6px 0 0;padding-left:18px">${[...c.dropped_findings, ...c.dropped_behaviour_rows, ...droppedFixes.map((d) => 'Fix for ' + d)].map((d) => `<li>${esc(d)}</li>`).join('')}</ul></details>` : ''}
      </div></section>`;
  }

  function renderModal(x) {
    if (S.modal === 'file' && S.file) {
      const f = S.file;
      const body = f.text == null
        ? `<div class="empty">${f.loading ? 'Loading…' : 'This file isn’t available on that side of the comparison.'}</div>`
        : `<div class="code">${cached(`f|${f.side}|${f.path}|${f.text.length}`, () => {
          const ls = f.text.split('\n');
          return window.IndiffCode ? IndiffCode.highlight(f.path, ls) : ls.map(esc);
        }).map((t, i) => `<div class="ln ${i + 1 === f.line ? 'hl' : ''}"><span class="n">${i + 1}</span><span class="s"></span><span class="x">${t || ' '}</span></div>`).join('')}</div>`;
      return `<div class="scrim" data-a="close-modal"><div class="modal" role="dialog" aria-modal="true" aria-label="File ${esc(f.path)}" data-stop>
        <div class="modal-head"><span class="ttl">${esc(f.path)}${f.side === 'old' ? ' · old version' : ''} · line ${f.line}</span><button class="btn small" data-a="close-modal">Close</button></div>
        <div class="modal-body">${body}</div></div></div>`;
    }
    if (S.modal === 'settings' && S.settings) {
      const s = S.settings;
      return `<div class="scrim" data-a="close-modal"><form class="modal narrow" role="dialog" aria-modal="true" aria-label="Settings" data-a="settings-form" data-stop>
        <div class="modal-head"><span class="ttl" style="font-family:var(--sans);font-weight:600">Settings</span><button type="button" class="btn small" data-a="close-modal">Cancel</button></div>
        <div class="modal-body pad">
          <div class="field"><label for="provider">Model provider</label>
            <select id="provider" name="provider"><option value="anthropic" ${s.provider !== 'openai' ? 'selected' : ''}>Anthropic</option><option value="openai" ${s.provider === 'openai' ? 'selected' : ''}>OpenAI-compatible (OpenAI, OpenRouter, Ollama…)</option></select></div>
          <div class="field"><label for="model">Model</label><input id="model" name="model" value="${esc(s.model)}" placeholder="claude-sonnet-5-5" spellcheck="false"></div>
          <div class="field"><label for="speed">Review speed</label>
            <select id="speed" name="speed"><option value="fast" ${s.speed !== 'thorough' ? 'selected' : ''}>Fast: little or no thinking</option><option value="thorough" ${s.speed === 'thorough' ? 'selected' : ''}>Thorough: the model thinks first (slower)</option></select>
            <span class="hint">Applies to recent Claude models. Other models and providers use their own defaults.</span></div>
          <div class="field"><label for="api_key">API key</label><input id="api_key" name="api_key" type="password" placeholder="${s.has_key ? (s.key_from_env ? 'Using key from environment' : 'Saved · leave blank to keep') : 'Paste your key'}" autocomplete="off">
            <span class="hint">Stored in a file only you can read, in the app’s settings folder. Your code is sent to this provider when you review.</span></div>
          <div class="field"><label for="base_url">Base URL <span class="muted">(optional)</span></label><input id="base_url" name="base_url" value="${esc(s.base_url)}" placeholder="Provider default" spellcheck="false"></div>
          <div style="display:flex;gap:8px;justify-content:space-between;flex-wrap:wrap">
            <button type="button" class="btn small" data-a="show-prompt">Show what the model sees</button>
            <button type="submit" class="btn primary">Save</button></div>
        </div></form></div>`;
    }
    if (S.modal === 'prompt') {
      return `<div class="scrim" data-a="close-modal"><div class="modal" role="dialog" aria-modal="true" aria-label="Prompt" data-stop>
        <div class="modal-head"><span class="ttl" style="font-family:var(--sans)">Exactly what is sent to the model</span><button class="btn small" data-a="close-modal">Close</button></div>
        <div class="modal-body"><pre class="prompt">${esc(S.prompt ?? 'Loading…')}</pre></div></div></div>`;
    }
    return '';
  }

  // ---------- actions ----------
  async function openRepo() {
    if (!inApp) { S.repo = DEMO.diff.repo; return load(); }
    const dir = await T.dialog.open({ directory: true, multiple: false, title: 'Choose a git repository' });
    if (!dir) return;
    S.repo = Array.isArray(dir) ? dir[0] : dir;
    S.revs = '';
    S.transcript = '';
    return load();
  }

  // Bumped on every load, so a review still running for the previous diff
  // can't write its result over the new one.
  let loadGen = 0;

  async function load() {
    loadGen++;
    S.reviewing = false; S.reviewError = null;
    S.loading = true; S.loadError = null; S.diff = null; S.out = null; S.sel = null; S.aim = 'all'; S.expanded = {};
    S.fixes = {}; S.outStale = false; S.newSince = new Set(); S.handoff = null; S.ctx = {}; S.files = {};
    htmlCache.clear();
    render();
    try {
      const [d, branches, transcripts] = await Promise.all([
        call('load_diff', withWs()),
        call('list_branches', { repo: S.repo }),
        call('list_transcripts', { repo: S.repo }),
      ]);
      S.diff = d; S.repo = d.repo; S.branches = branches || []; S.transcripts = transcripts || [];
      // Default to the newest session from the last two days.
      if (!S.transcript && S.transcripts.length && Date.now() / 1000 - S.transcripts[0].modified < 172800) S.transcript = S.transcripts[0].path;
      store.load();
    } catch (e) {
      S.loadError = String(e);
    }
    S.loading = false;
    render();
    if (S.diff && S.diff.hunks.length && S.settings && S.settings.has_key) review(false);
  }

  async function review(force) {
    if (S.reviewing || !S.diff) return;
    const gen = loadGen;
    S.reviewing = true; S.reviewError = null;
    render();
    let out, err;
    try {
      out = await call('run_review', { ...withWs(), intent: S.intent || null, transcript: S.transcript || null, force: !!force });
    } catch (e) {
      err = String(e);
    }
    if (gen !== loadGen) return;
    S.reviewing = false;
    if (err) {
      S.reviewError = err;
      render();
      return;
    }
    S.out = out; S.sel = null;
    alignReview();
    render();
    // The files may have changed while the model ran. Catch the diff up now.
    if (out.diff_key !== S.diff.diff_key) refresh();
  }

  // Points the review's hunk ids at the diff on screen, matching hunks by content.
  // Always works from the review as it arrived, so repeated refreshes don't compound.
  // When the diff differs from the one reviewed, the review is stale and hunks it
  // never saw are listed in newSince.
  function alignReview() {
    const o = S.out;
    const d = S.diff;
    if (!o || !d || !o.hunks) return;
    if (!o.original) o.original = JSON.parse(JSON.stringify({ attention: o.review.attention, aims: o.review.aims }));
    const byHash = {};
    for (const h of d.hunks) byHash[hunkHash(h)] = h.id;
    const map = {};
    for (const h of o.hunks) { const n = byHash[hunkHash(h)]; if (n) map[h.id] = n; }
    const ids = (a) => a.map((k) => map[k]).filter(Boolean);
    const att = o.original.attention;
    o.review.attention = {
      ...att,
      must_read: ids(att.must_read),
      skip_reasons: Object.fromEntries(Object.entries(att.skip_reasons || {}).filter(([k]) => map[k]).map(([k, v]) => [map[k], v])),
    };
    o.review.aims = o.original.aims.map((a) => ({ ...a, hunks: ids(a.hunks) }));
    S.outStale = d.diff_key !== o.diff_key;
    const seen = new Set(Object.values(map));
    S.newSince = S.outStale ? new Set(d.hunks.map((h) => h.id).filter((id) => !seen.has(id))) : new Set();
  }

  // Reloads the diff after the files change. The review is kept: re-running it costs
  // money, so that stays the user's call. Per-hunk state follows hunks by content.
  async function refresh() {
    const gen = loadGen;
    let d;
    try { d = await call('load_diff', withWs()); } catch { return; }
    if (gen !== loadGen || !S.diff || d.diff_key === S.diff.diff_key) return;
    const byHash = {};
    for (const h of d.hunks) byHash[hunkHash(h)] = h.id;
    const map = {};
    for (const h of S.diff.hunks) { const n = byHash[hunkHash(h)]; if (n) map[h.id] = n; }
    const remap = (obj) => Object.fromEntries(Object.entries(obj).filter(([k]) => map[k]).map(([k, v]) => [map[k], v]));
    S.expanded = remap(S.expanded);
    S.ctx = remap(S.ctx);
    S.diff = d;
    S.files = {};
    htmlCache.clear();
    store.load();
    alignReview();
    render();
  }

  // Polls for file changes while the window is visible, so edits made elsewhere
  // (in an editor, or by Claude Code after a handoff) show up without a click.
  let polling = false;
  async function poll() {
    if (polling || !S.diff || S.loading || S.reviewing || document.hidden) return;
    polling = true;
    const gen = loadGen;
    try {
      const k = await call('diff_key', withWs());
      if (gen === loadGen && S.diff && k !== S.diff.diff_key && !S.reviewing) await refresh();
    } catch { /* the next poll will try again */ }
    polling = false;
  }
  setInterval(poll, 3000);
  window.addEventListener('focus', poll);
  document.addEventListener('visibilitychange', poll);

  // Writes a fix to disk. The diff isn't reloaded until the user asks, so the
  // review stays readable and applying a fix never triggers a model call.
  async function applyFix(id) {
    const f = derive().byId[id];
    if (!f || !f.fix || (S.fixes[id] && S.fixes[id].state === 'applying')) return;
    S.fixes[id] = { state: 'applying' };
    render();
    try {
      const files = await call('apply_fix', { ...base(), fix: f.fix });
      S.fixes[id] = { state: 'applied', files };
    } catch (e) {
      S.fixes[id] = { state: 'error', msg: String(e) };
    }
    render();
    poll();
  }

  async function openFile(path, side, line) {
    S.file = { path, side, line, text: null, loading: true };
    S.modal = 'file';
    render();
    try { S.file.text = await call('read_file', { ...base(), side, path }); } catch { S.file.text = null; }
    S.file.loading = false;
    render();
  }

  async function moreContext(id, dir) {
    const h = S.diff && S.diff.hunks.find((h) => h.id === id);
    if (!h) return;
    const side = ctxSide(h);
    const path = side === 'old' ? h.old_file : h.file;
    const k = side + '|' + path;
    if (!(k in S.files)) {
      try { S.files[k] = await call('read_file', { ...base(), side, path }); } catch { S.files[k] = null; }
    }
    const c = S.ctx[id] || (S.ctx[id] = { above: 0, below: 0 });
    c[dir] += 20;
    render();
  }

  function gotoFile(path) {
    const hs = S.diff.hunks.filter((h) => h.file === path);
    if (!hs.length) return;
    // Show it even if the current aim filters it out.
    if (S.aim !== 'all' && !hs.some((h) => document.getElementById('hunk-' + h.id))) { S.aim = 'all'; S.sel = null; render(); }
    const el = app.querySelector(`.hunk[data-file="${CSS.escape(path)}"]`);
    if (el) el.scrollIntoView({ block: 'start', behavior: 'smooth' });
  }

  function selectFinding(id) {
    S.sel = S.sel === id ? null : id;
    if (S.sel) {
      const f = derive().byId[id];
      if (f && S.aim !== 'all' && f.aim !== S.aim) S.aim = 'all';
      // Make sure the hunk holding the first citation is visible.
      const c = f && f.evidence[0];
      if (c && S.diff) {
        const h = S.diff.hunks.find((h) => (c.side === 'old' ? h.old_file : h.file) === c.file && h.lines.some((l) => (c.side === 'old' ? l.old : l.new) === c.line));
        if (h) S.expanded[h.id] = true;
        render();
        if (h) { const el = document.getElementById('hunk-' + h.id); if (el) el.scrollIntoView({ block: 'nearest', behavior: 'smooth' }); }
        return;
      }
    }
    render();
  }

  app.addEventListener('click', (e) => {
    const t = e.target.closest('[data-a]');
    if (!t) return;
    const a = t.dataset.a;
    if (a === 'close-modal') {
      if (t.classList.contains('scrim') && e.target.closest('[data-stop]')) return;
      S.modal = null; render(); return;
    }
    if (t.tagName === 'FORM') return;
    switch (a) {
      case 'open-repo': openRepo(); break;
      case 'review': review(t.dataset.force === '1'); break;
      case 'toggle-ai': S.ai = !S.ai; S.sel = null; render(); break;
      case 'aim': S.aim = t.dataset.id; S.sel = null; render(); break;
      case 'sel': selectFinding(t.dataset.id); break;
      case 'back': S.sel = null; render(); break;
      case 'apply-fix': applyFix(t.dataset.id); break;
      case 'handoff': handoff(t.dataset.key, t.dataset.ids, false); break;
      case 'handoff-copy': handoff(t.dataset.key, t.dataset.ids, true); break;
      case 'more-ctx': moreContext(t.dataset.id, t.dataset.dir); break;
      case 'goto-file': gotoFile(t.dataset.file); break;
      case 'expand': S.expanded[t.dataset.id] = !S.expanded[t.dataset.id]; render(); break;
      case 'reviewed': S.reviewed[t.dataset.id] = !S.reviewed[t.dataset.id]; store.save(); render(); break;
      case 'open-cite': {
        const f = derive().byId[t.dataset.id];
        const c = f && f.evidence[+t.dataset.i];
        if (c) openFile(c.file, c.side === 'old' ? 'old' : 'new', c.line);
        break;
      }
      case 'cite': {
        const r = S.out.review.behaviour[+t.dataset.i];
        const c = r && r.evidence[0];
        if (c) openFile(c.file, c.side === 'old' ? 'old' : 'new', c.line);
        break;
      }
      case 'open-ref': openFile(t.dataset.file, 'new', +t.dataset.line); break;
      case 'settings': call('get_settings').then((s) => { S.settings = s; S.modal = 'settings'; render(); }); break;
      case 'show-prompt':
        S.modal = 'prompt'; S.prompt = null; render();
        call('prompt_preview', { ...withWs(), intent: S.intent || null, transcript: S.transcript || null })
          .then((p) => { S.prompt = p; render(); }, (err) => { S.prompt = String(err); render(); });
        break;
    }
  });

  app.addEventListener('submit', async (e) => {
    e.preventDefault();
    const form = e.target;
    if (form.dataset.a === 'load-form') {
      S.revs = form.revs.value;
      load();
    } else if (form.dataset.a === 'settings-form') {
      const key = form.api_key.value;
      const update = { provider: form.provider.value, model: form.model.value.trim(), base_url: form.base_url.value.trim(), speed: form.speed.value, api_key: key ? key.trim() : null };
      try {
        S.settings = await call('save_settings', { update });
        S.modal = null;
        render();
        if (S.settings.has_key && S.diff && !S.out) review(false);
      } catch (err) { alertInline(String(err)); }
    }
  });

  function alertInline(msg) { S.reviewError = msg; S.modal = null; render(); }

  app.addEventListener('input', (e) => {
    if (e.target.id === 'intent') S.intent = e.target.value;
  });
  app.addEventListener('change', (e) => {
    if (e.target.id === 'transcript') S.transcript = e.target.value;
    if (e.target.id === 'ignore-ws') {
      S.ignoreWs = e.target.checked;
      try { localStorage.setItem('indiff:ignoreWs', S.ignoreWs ? '1' : '0'); } catch { /* storage unavailable */ }
      load();
    }
  });

  document.addEventListener('keydown', (e) => {
    if (e.target.matches('input, textarea, select')) return;
    if (e.key === 'Escape' && S.modal) { S.modal = null; render(); return; }
    if (e.key === 'Escape' && S.sel) { S.sel = null; render(); return; }
    // n / p: next and previous finding.
    if ((e.key === 'n' || e.key === 'p') && S.out && S.ai) {
      const fs = derive().findings.filter((f) => f.kind !== 'achieved');
      if (!fs.length) return;
      const i = fs.findIndex((f) => f.id === S.sel);
      const j = e.key === 'n' ? (i + 1) % fs.length : (i <= 0 ? fs.length - 1 : i - 1);
      S.sel = null;
      selectFinding(fs[j].id);
    }
  });

  // ---------- start ----------
  (async () => {
    try { S.settings = await call('get_settings'); } catch { S.settings = null; }
    const l = await call('launch_args');
    if (l && l.repo) {
      S.repo = l.repo; S.revs = (l.revs || []).join(' '); S.staged = !!l.staged;
      load();
    } else {
      render();
    }
  })();
})();
