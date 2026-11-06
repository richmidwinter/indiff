// Syntax highlighting and word-level change marks for diff lines.
// Uses the vendored highlight.js when it loaded; plain escaped text otherwise.
(() => {
  'use strict';

  const H = window.hljs;
  const esc = (s) => String(s ?? '').replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));

  // Past these sizes, highlighting costs more than it helps (generated or minified code).
  const MAX_CHARS = 200000;
  const MAX_LINE = 2000;

  const BY_NAME = { makefile: 'makefile', gnumakefile: 'makefile', dockerfile: 'bash', gemfile: 'ruby', rakefile: 'ruby', 'cargo.lock': 'ini' };
  const BY_EXT = { vue: 'xml', svelte: 'xml', htm: 'xml', svg: 'xml', toml: 'ini', jsonc: 'json', json5: 'json', mdx: 'markdown', zsh: 'bash', fish: 'bash', mts: 'typescript', cts: 'typescript' };

  /** highlight.js language for a path, or null to leave it plain. */
  function langFor(path) {
    if (!H || !path) return null;
    const base = path.split('/').pop().toLowerCase();
    if (BY_NAME[base]) return BY_NAME[base];
    const ext = base.includes('.') ? base.split('.').pop() : '';
    const name = BY_EXT[ext] || ext;
    // getLanguage resolves aliases, which cover most extensions (ts, rs, py, yml, h…).
    return name && H.getLanguage(name) ? name : null;
  }

  /** Splits highlighted HTML into lines, closing and reopening spans that cross a line break. */
  function splitLines(html) {
    const out = [];
    const stack = [];
    let cur = '';
    const re = /(<span[^>]*>)|(<\/span>)|(\n)|([^<\n]+)/g;
    let m;
    while ((m = re.exec(html))) {
      if (m[1]) { stack.push(m[1]); cur += m[1]; }
      else if (m[2]) { stack.pop(); cur += m[2]; }
      else if (m[3]) { cur += '</span>'.repeat(stack.length); out.push(cur); cur = stack.join(''); }
      else cur += m[4];
    }
    out.push(cur);
    return out;
  }

  /**
   * Highlights consecutive lines of one file as a single piece of text, so
   * constructs spanning lines (block comments, multi-line strings) come out right.
   */
  function highlight(path, lines) {
    const lang = langFor(path);
    if (!lang || !lines.length) return lines.map(esc);
    let total = 0;
    for (const l of lines) {
      if (l.length > MAX_LINE) return lines.map(esc);
      total += l.length + 1;
    }
    if (total > MAX_CHARS) return lines.map(esc);
    try {
      const out = splitLines(H.highlight(lines.join('\n'), { language: lang, ignoreIllegals: true }).value);
      return out.length === lines.length ? out : lines.map(esc);
    } catch {
      return lines.map(esc);
    }
  }

  // ---------- word-level changes ----------
  const TOKEN = /\w+|\s+|[^\w\s]/g;

  /** Character ranges [start, end) of tokens not kept. Ranges split only by whitespace are merged. */
  function ranges(text, toks, kept) {
    const r = [];
    let pos = 0;
    toks.forEach((t, i) => {
      if (!kept[i]) {
        const last = r[r.length - 1];
        if (last && /^\s*$/.test(text.slice(last[1], pos))) last[1] = pos + t.length;
        else r.push([pos, pos + t.length]);
      }
      pos += t.length;
    });
    return r;
  }

  /**
   * The parts of an old and a new line that differ, as [oldRanges, newRanges].
   * Null when the lines share too little for marks to help (the whole line changed).
   */
  function wordDiff(a, b) {
    if (a === b || a.length > 1000 || b.length > 1000) return null;
    const ta = a.match(TOKEN) || [];
    const tb = b.match(TOKEN) || [];
    const n = ta.length;
    const m = tb.length;
    if (!n || !m || n * m > 250000) return null;
    const dp = Array.from({ length: n + 1 }, () => new Uint16Array(m + 1));
    for (let i = n - 1; i >= 0; i--) {
      for (let j = m - 1; j >= 0; j--) {
        dp[i][j] = ta[i] === tb[j] ? dp[i + 1][j + 1] + 1 : Math.max(dp[i + 1][j], dp[i][j + 1]);
      }
    }
    const ka = new Array(n).fill(false);
    const kb = new Array(m).fill(false);
    let i = 0;
    let j = 0;
    while (i < n && j < m) {
      if (ta[i] === tb[j]) { ka[i] = kb[j] = true; i++; j++; }
      else if (dp[i + 1][j] >= dp[i][j + 1]) i++;
      else j++;
    }
    // Shared whitespace doesn't make two lines alike.
    const solid = (toks, kept) => toks.reduce((s, t, k) => s + (kept[k] ? t.trim().length : 0), 0);
    const shared = solid(ta, ka);
    const size = Math.max(a.replace(/\s/g, '').length, b.replace(/\s/g, '').length);
    if (shared * 2 < size) return null;
    return [ranges(a, ta, ka), ranges(b, tb, kb)];
  }

  /** Wraps the characters in `rs` (offsets into the line's text) in <mark class="wd">, keeping tags intact. */
  function overlay(html, rs) {
    if (!rs || !rs.length) return html;
    let out = '';
    let pos = 0;
    let ri = 0;
    let open = false;
    const re = /(<[^>]+>)|(&(?:[a-z]+|#\d+|#x[0-9a-f]+);)|([^<&])/gi;
    let m;
    while ((m = re.exec(html))) {
      if (m[1]) { out += open ? `</mark>${m[1]}<mark class="wd">` : m[1]; continue; }
      while (ri < rs.length && pos >= rs[ri][1]) ri++;
      const inside = ri < rs.length && pos >= rs[ri][0];
      if (inside && !open) { out += '<mark class="wd">'; open = true; }
      if (!inside && open) { out += '</mark>'; open = false; }
      out += m[2] || m[3];
      pos++;
    }
    if (open) out += '</mark>';
    return out;
  }

  /**
   * HTML for every line of a hunk: each side highlighted as a whole, and the
   * changed words marked where a removed line is paired with an added one.
   */
  function hunkLines(h) {
    const oldIdx = [];
    const newIdx = [];
    h.lines.forEach((l, i) => {
      if (l.kind !== '+') oldIdx.push(i);
      if (l.kind !== '-') newIdx.push(i);
    });
    const oldHtml = highlight(h.old_file, oldIdx.map((i) => h.lines[i].text));
    const newHtml = highlight(h.file, newIdx.map((i) => h.lines[i].text));
    const html = new Array(h.lines.length);
    oldIdx.forEach((i, k) => { if (h.lines[i].kind === '-') html[i] = oldHtml[k]; });
    newIdx.forEach((i, k) => { html[i] = newHtml[k]; });

    // Pair each run of removed lines with the run of added lines straight after it.
    for (let i = 0; i < h.lines.length;) {
      if (h.lines[i].kind !== '-') { i++; continue; }
      const dStart = i;
      while (i < h.lines.length && h.lines[i].kind === '-') i++;
      const aStart = i;
      while (i < h.lines.length && h.lines[i].kind === '+') i++;
      const pairs = Math.min(aStart - dStart, i - aStart);
      for (let k = 0; k < pairs; k++) {
        const d = dStart + k;
        const a = aStart + k;
        const w = wordDiff(h.lines[d].text, h.lines[a].text);
        if (w) { html[d] = overlay(html[d], w[0]); html[a] = overlay(html[a], w[1]); }
      }
    }
    return html;
  }

  window.IndiffCode = { highlight, hunkLines, wordDiff, overlay, langFor };
})();
