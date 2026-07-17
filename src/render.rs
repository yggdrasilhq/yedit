//! Markdown rendering and the viewport page.
//!
//! yedit OWNS its viewport content (the one libyggterm rule), so the top bar
//! (markdown toggle, save floppy) lives INSIDE this page, not in yggterm
//! chrome. The page is served from yedit's own loopback server and talks back
//! to it same-origin.

use pulldown_cmark::{html, Options, Parser};

/// Markdown → HTML with tables (the triage-board acceptance test is a wide
/// table), strikethrough and task lists enabled.
pub fn markdown_to_html(markdown: &str) -> String {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TASKLISTS);
    options.insert(Options::ENABLE_FOOTNOTES);
    let parser = Parser::new_ext(markdown, options);
    let mut out = String::with_capacity(markdown.len() * 2);
    html::push_html(&mut out, parser);
    out
}

/// The single-page app. Self-contained: inline CSS/JS, no external fetches.
/// Theme-aware via `prefers-color-scheme`; wide content (tables, code) scrolls
/// inside its own container so the page never scrolls horizontally.
pub fn page_html() -> &'static str {
    r#"<!doctype html>
<html>
<head>
<meta charset="utf-8">
<title>yedit</title>
<style>
:root {
  --bg: #ffffff; --fg: #1c2024; --muted: #6a737d; --chrome: #f2f3f5;
  --border: #d8dce1; --accent: #3b82d9; --dirty: #e0a030; --danger: #c0504a;
}
@media (prefers-color-scheme: dark) {
  :root {
    --bg: #1e2227; --fg: #d6dade; --muted: #8a929c; --chrome: #262b31;
    --border: #343a42; --accent: #5ba3e8; --dirty: #e8b458; --danger: #d97a75;
  }
}
* { box-sizing: border-box; }
html, body { margin: 0; height: 100%; }
body {
  background: var(--bg); color: var(--fg);
  font-family: system-ui, -apple-system, "Segoe UI", sans-serif;
  font-size: 14px; display: flex; flex-direction: column;
}
#topbar {
  display: flex; align-items: center; gap: 8px; padding: 6px 12px;
  background: var(--chrome); border-bottom: 1px solid var(--border);
  flex: 0 0 auto; min-height: 38px;
}
#filename { font-weight: 600; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
#dirty { color: var(--dirty); visibility: hidden; font-size: 16px; line-height: 1; }
#dirty.on { visibility: visible; }
#filepath { color: var(--muted); font-size: 11px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; flex: 1 1 auto; }
.tbtn {
  background: transparent; color: var(--fg); border: 1px solid var(--border);
  border-radius: 5px; padding: 4px 10px; cursor: pointer; font-size: 13px; line-height: 1.2;
}
.tbtn:hover { border-color: var(--accent); }
.tbtn.active { background: var(--accent); color: #fff; border-color: var(--accent); }
#content { flex: 1 1 auto; overflow: auto; }
#rendered { padding: 18px 26px 40px; max-width: 980px; margin: 0 auto; line-height: 1.55; }
#rendered h1, #rendered h2 { border-bottom: 1px solid var(--border); padding-bottom: 4px; }
#rendered code {
  background: var(--chrome); border: 1px solid var(--border); border-radius: 4px;
  padding: 1px 5px; font-family: ui-monospace, "SF Mono", monospace; font-size: 12.5px;
}
#rendered pre { background: var(--chrome); border: 1px solid var(--border); border-radius: 6px; padding: 10px 14px; overflow-x: auto; }
#rendered pre code { background: none; border: none; padding: 0; }
#rendered blockquote { border-left: 3px solid var(--accent); margin-left: 0; padding-left: 14px; color: var(--muted); }
#rendered a { color: var(--accent); }
.tablewrap { overflow-x: auto; }
#rendered table { border-collapse: collapse; margin: 12px 0; font-size: 13px; }
#rendered th, #rendered td { border: 1px solid var(--border); padding: 5px 10px; text-align: left; vertical-align: top; }
#rendered th { background: var(--chrome); position: sticky; top: 0; }
#rendered tr:nth-child(even) td { background: color-mix(in srgb, var(--chrome) 45%, var(--bg)); }
#editor {
  display: none; width: 100%; height: 100%; border: none; outline: none; resize: none;
  background: var(--bg); color: var(--fg); padding: 16px 20px;
  font-family: ui-monospace, "SF Mono", monospace; font-size: 13px; line-height: 1.5;
  tab-size: 4;
}
#empty { padding: 40px; color: var(--muted); max-width: 640px; margin: 0 auto; }
#empty h2 { color: var(--fg); }
#empty ul { padding-left: 18px; }
#empty a { color: var(--accent); cursor: pointer; }
#toast {
  position: fixed; bottom: 18px; right: 18px; background: var(--chrome); color: var(--fg);
  border: 1px solid var(--border); border-radius: 6px; padding: 8px 14px; font-size: 13px;
  opacity: 0; transition: opacity .18s; pointer-events: none; max-width: 60ch;
}
#toast.show { opacity: 1; }
#conflict {
  display: none; position: fixed; inset: 0; background: rgba(0,0,0,.45);
  align-items: center; justify-content: center;
}
#conflict .box {
  background: var(--bg); border: 1px solid var(--border); border-radius: 8px;
  padding: 20px 24px; max-width: 460px;
}
#conflict .box .row { display: flex; gap: 10px; margin-top: 16px; justify-content: flex-end; }
.tbtn.danger { border-color: var(--danger); color: var(--danger); }
.tbtn.danger:hover { background: var(--danger); color: #fff; }
</style>
</head>
<body>
<div id="topbar">
  <span id="dirty" title="Unsaved changes">●</span>
  <span id="filename">yedit</span>
  <span id="filepath"></span>
  <button id="modebtn" class="tbtn" title="Toggle rendered markdown / plain editor (Ctrl+E)">Markdown</button>
  <button id="savebtn" class="tbtn" title="Save (Ctrl+S)">💾&#xFE0E; Save</button>
</div>
<div id="content">
  <div id="rendered"></div>
  <textarea id="editor" spellcheck="false"></textarea>
  <div id="empty" style="display:none">
    <h2>yedit</h2>
    <p>No note is open. Open one from the 🗒&#xFE0E; <b>Notes</b> pane in the right rail
    (path box or a recent file), or run <code>yedit &lt;file&gt;</code>.</p>
    <div id="recent"></div>
  </div>
</div>
<div id="toast"></div>
<div id="conflict">
  <div class="box">
    <b>File changed on disk</b>
    <p class="muted">This note's file was modified outside yedit since it was
    loaded. Saving now would overwrite those changes.</p>
    <div class="row">
      <button id="conflict-reload" class="tbtn">Reload from disk</button>
      <button id="conflict-overwrite" class="tbtn danger">Overwrite</button>
    </div>
  </div>
</div>
<script>
(() => {
  const el = (id) => document.getElementById(id);
  let state = { active_id: null, markdown_mode: true, epoch: 0 };
  let doc = null;            // { id, name, path, content, html, dirty }
  let editTimer = null;
  let pendingSave = null;    // content awaiting a conflict decision

  const toast = (text) => {
    const t = el('toast');
    t.textContent = text;
    t.classList.add('show');
    clearTimeout(t._h);
    t._h = setTimeout(() => t.classList.remove('show'), 2400);
  };

  const api = async (path, body) => {
    const opts = body === undefined ? {} :
      { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(body) };
    const res = await fetch(path, opts);
    const value = await res.json().catch(() => ({}));
    return { status: res.status, value };
  };

  const renderTopbar = () => {
    el('dirty').classList.toggle('on', !!(doc && doc.dirty));
    el('filename').textContent = doc ? doc.name : 'yedit';
    el('filepath').textContent = doc ? doc.path : '';
    el('modebtn').classList.toggle('active', state.markdown_mode);
    el('modebtn').textContent = state.markdown_mode ? 'Markdown' : 'Plain';
    document.title = (doc && doc.dirty ? '● ' : '') + (doc ? doc.name : 'yedit') + ' — yedit';
  };

  const renderBody = () => {
    const hasDoc = !!doc;
    el('empty').style.display = hasDoc ? 'none' : 'block';
    el('rendered').style.display = hasDoc && state.markdown_mode ? 'block' : 'none';
    el('editor').style.display = hasDoc && !state.markdown_mode ? 'block' : 'none';
    if (!hasDoc) { renderTopbar(); return; }
    if (state.markdown_mode) {
      el('rendered').innerHTML = doc.html;
      // Wide tables scroll inside their own container, never the page.
      for (const table of el('rendered').querySelectorAll('table')) {
        if (table.parentElement.classList.contains('tablewrap')) continue;
        const wrap = document.createElement('div');
        wrap.className = 'tablewrap';
        table.replaceWith(wrap);
        wrap.appendChild(table);
      }
    } else if (el('editor').value !== doc.content) {
      el('editor').value = doc.content;
    }
    renderTopbar();
  };

  const loadDoc = async (id, opts) => {
    if (!id) { doc = null; renderBody(); return; }
    const reload = opts && opts.reload ? '&reload=1' : '';
    const { value } = await api(`/api/doc?id=${id}${reload}`);
    doc = value && value.id ? value : null;
    renderBody();
  };

  const refreshState = async () => {
    const { value } = await api('/api/state');
    const switched = value.active_id !== state.active_id;
    const moved = value.epoch !== state.epoch;
    state = value;
    if (switched) await loadDoc(state.active_id);
    else if (moved && doc && !doc.dirty) await loadDoc(state.active_id);
    else renderTopbar();
    if (!state.active_id) {
      const rec = (state.recent || []).map(p =>
        `<li><a data-path="${p.replaceAll('"','&quot;')}">${p}</a></li>`).join('');
      el('recent').innerHTML = rec ? `<p>Recent:</p><ul>${rec}</ul>` : '';
      renderBody();
    }
  };
  window.yeditPoll = refreshState;

  el('recent').addEventListener('click', async (ev) => {
    const path = ev.target && ev.target.dataset && ev.target.dataset.path;
    if (path) { await api('/api/open', { path }); refreshState(); }
  });

  const currentContent = () =>
    state.markdown_mode ? (doc ? doc.content : '') : el('editor').value;

  const save = async (force) => {
    if (!doc) return;
    const content = pendingSave !== null && force ? pendingSave : currentContent();
    const { status, value } = await api('/api/save', { id: doc.id, content, force: !!force });
    if (status === 409) {
      pendingSave = content;
      el('conflict').style.display = 'flex';
      return;
    }
    pendingSave = null;
    if (value.ok) { toast('Saved'); await loadDoc(doc.id); }
    else toast('Save failed: ' + (value.error || status));
  };

  el('savebtn').addEventListener('click', () => save(false));
  el('conflict-overwrite').addEventListener('click', async () => {
    el('conflict').style.display = 'none';
    await save(true);
  });
  el('conflict-reload').addEventListener('click', async () => {
    el('conflict').style.display = 'none';
    pendingSave = null;
    await api('/api/reload', { id: doc.id });
    await loadDoc(doc.id, { reload: true });
    toast('Reloaded from disk');
  });

  el('modebtn').addEventListener('click', async () => {
    // Leaving the editor flushes the draft so the rendered view shows it.
    if (!state.markdown_mode && doc) {
      doc.content = el('editor').value;
      await api('/api/edit', { id: doc.id, content: doc.content });
      await loadDoc(doc.id);
    }
    const { value } = await api('/api/mode', { markdown: !state.markdown_mode });
    state.markdown_mode = value.markdown_mode;
    renderBody();
  });

  el('editor').addEventListener('input', () => {
    if (!doc) return;
    doc.dirty = true;
    renderTopbar();
    clearTimeout(editTimer);
    editTimer = setTimeout(async () => {
      doc.content = el('editor').value;
      await api('/api/edit', { id: doc.id, content: doc.content });
    }, 500);
  });

  document.addEventListener('keydown', (ev) => {
    if ((ev.ctrlKey || ev.metaKey) && ev.key === 's') { ev.preventDefault(); save(false); }
    if ((ev.ctrlKey || ev.metaKey) && ev.key === 'e') { ev.preventDefault(); el('modebtn').click(); }
  });

  refreshState();
  setInterval(refreshState, 1500);
})();
</script>
</body>
</html>
"#
}
