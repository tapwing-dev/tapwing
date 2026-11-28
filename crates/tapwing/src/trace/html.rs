//! Self-contained offline `trace.html` viewer: inline CSS and vanilla JS, no
//! external URLs. The trace JSON is embedded in a `<script type=
//! "application/json">` block with `<`, `>`, `&`, U+2028, and U+2029 escaped,
//! and the script renders every value with `textContent`, so flow text,
//! URLs, and headers from the app can never inject markup.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::{Value, json};

use crate::report::html::escape_html;

const STYLES: &str = r#"
:root{
  --bg:#ffffff;--bg-subtle:#f6f7f8;--bg-hi:#eef1f4;--line:#e6e8eb;--line-strong:#d3d6db;
  --fg:#1b1d21;--fg-2:#4f545c;--fg-3:#868b94;
  --pass:#22774b;--fail:#c13a2f;--warn:#a8670a;--skip:#a4a9b1;--info:#2f5fb3;
  --sans:-apple-system,BlinkMacSystemFont,"Segoe UI",Roboto,"Helvetica Neue",Arial,sans-serif;
  --mono:ui-monospace,SFMono-Regular,"SF Mono",Menlo,Consolas,"Liberation Mono",monospace;
  color-scheme:light;
}
@media (prefers-color-scheme:dark){
  :root{
    --bg:#101113;--bg-subtle:#17191c;--bg-hi:#1e2125;--line:#24272c;--line-strong:#33373e;
    --fg:#e7e9ec;--fg-2:#adb2ba;--fg-3:#7b818b;
    --pass:#5db585;--fail:#e87a70;--warn:#dba24a;--skip:#5e646d;--info:#7fa6ea;
    color-scheme:dark;
  }
}
*{box-sizing:border-box}
html{-webkit-text-size-adjust:100%}
body{margin:0;background:var(--bg);color:var(--fg);font:14px/1.5 var(--sans);
  -webkit-font-smoothing:antialiased;font-variant-numeric:tabular-nums}
code,pre,.mono{font-family:var(--mono);font-size:12.5px}
.page{max-width:1040px;margin:0 auto;padding:40px 24px 64px}
h1{margin:0;font-size:20px;font-weight:600}
h2{margin:0;font-size:15px;font-weight:600;overflow-wrap:anywhere}
.muted{color:var(--fg-3)}
.pass{color:var(--pass)}.fail{color:var(--fail)}.warn{color:var(--warn)}.skip{color:var(--skip)}
.facts{display:flex;flex-wrap:wrap;gap:10px 36px;margin:20px 0 24px;padding:12px 0;
  border-top:1px solid var(--line);border-bottom:1px solid var(--line)}
.facts div{display:flex;flex-direction:column}
.facts dt{font-size:12px;color:var(--fg-3)}
.facts dd{margin:0;font-size:15px;font-weight:600;overflow-wrap:anywhere}
.toolbar{position:sticky;top:0;z-index:2;display:flex;flex-wrap:wrap;gap:8px;align-items:center;
  padding:10px 0;margin-bottom:12px;background:var(--bg);border-bottom:1px solid var(--line)}
.toolbar button{font:inherit;font-size:13px;padding:4px 10px;border-radius:999px;cursor:pointer;
  border:1px solid var(--line-strong);background:var(--bg);color:var(--fg-2)}
.toolbar button[aria-pressed=true]{background:var(--fg);color:var(--bg);border-color:var(--fg)}
.toolbar input{flex:1 1 180px;min-width:0;font:inherit;font-size:13px;padding:5px 10px;border-radius:6px;
  border:1px solid var(--line-strong);background:var(--bg);color:var(--fg)}
.flow{margin:28px 0 36px}
.flow-head{display:flex;flex-wrap:wrap;justify-content:space-between;align-items:baseline;gap:4px 16px;
  padding-bottom:8px;border-bottom:1px solid var(--line-strong)}
.flow-meta{color:var(--fg-2);font-size:13px}
.flow-err{margin:10px 0 0;padding:10px 12px;background:var(--bg-subtle);border:1px solid var(--line);
  border-radius:4px;white-space:pre-wrap;overflow-wrap:anywhere;color:var(--fail)}
.timeline{display:flex;height:12px;margin:12px 0 6px;border-radius:3px;overflow:hidden;background:var(--bg-subtle)}
.timeline span{display:block;min-width:2px;height:100%;cursor:pointer;border-right:1px solid var(--bg)}
.timeline .s-passed{background:var(--pass)}.timeline .s-failed{background:var(--fail)}
.timeline .s-warned{background:var(--warn)}.timeline .s-skipped{background:var(--skip)}
.timeline span:hover{opacity:.75}
details.step{border-bottom:1px solid var(--line)}
details.step>summary{list-style:none;cursor:pointer;display:grid;
  grid-template-columns:16px 34px minmax(0,1fr) auto;gap:8px;align-items:baseline;padding:7px 8px}
details.step>summary::-webkit-details-marker{display:none}
details.step>summary:hover{background:var(--bg-subtle)}
details.step[open]>summary{background:var(--bg-hi)}
.st{font-weight:700;text-align:center}
.idx{color:var(--fg-3);text-align:right}
.desc{overflow-wrap:anywhere}
.dur{color:var(--fg-2);white-space:nowrap;text-align:right}
.badges{display:inline-flex;flex-wrap:wrap;gap:4px;margin-left:6px;vertical-align:1px}
.badge{font-size:11px;line-height:16px;padding:0 6px;border-radius:999px;border:1px solid var(--line-strong);color:var(--fg-2)}
.badge.b-fail{color:var(--fail);border-color:currentColor}
.badge.b-warn{color:var(--warn);border-color:currentColor}
.badge.b-info{color:var(--info);border-color:currentColor}
.step-body{padding:6px 8px 16px 58px;display:flex;flex-direction:column;gap:12px}
.step-body h3{margin:0 0 4px;font-size:12px;font-weight:600;color:var(--fg-3);text-transform:uppercase;letter-spacing:.04em}
pre.box{margin:0;padding:10px 12px;background:var(--bg-subtle);border:1px solid var(--line);border-radius:4px;
  white-space:pre-wrap;overflow-wrap:anywhere;line-height:1.5}
pre.box.err{color:var(--fail)}
ul.plain{margin:0;padding-left:18px}
ul.plain li{margin:2px 0;overflow-wrap:anywhere}
.table-wrap{overflow-x:auto}
table.http{width:100%;border-collapse:collapse;font-size:12.5px}
table.http th{text-align:left;font-weight:500;color:var(--fg-3);padding:0 8px 4px;border-bottom:1px solid var(--line)}
table.http td{padding:4px 8px;border-bottom:1px solid var(--line);vertical-align:top}
table.http td.url{font-family:var(--mono);overflow-wrap:anywhere;min-width:200px}
table.http td.num{text-align:right;white-space:nowrap}
table.http tr.bad td{color:var(--fail)}
details.sub>summary{cursor:pointer;font-size:12.5px;color:var(--fg-2)}
details.sub dl{display:grid;grid-template-columns:max-content minmax(0,1fr);gap:2px 14px;margin:4px 0 0;font-size:12px}
details.sub dt{color:var(--fg-3);font-family:var(--mono)}
details.sub dd{margin:0;font-family:var(--mono);overflow-wrap:anywhere}
figure.shot{margin:0}
figure.shot img{display:block;max-width:100%;max-height:520px;border:1px solid var(--line);border-radius:4px;background:var(--bg-subtle)}
figure.shot figcaption{margin-top:4px;font-size:12px;color:var(--fg-3);overflow-wrap:anywhere}
details.tree{border:1px solid var(--line);border-radius:4px;background:var(--bg-subtle)}
details.tree>summary{cursor:pointer;padding:6px 10px;font-size:13px;color:var(--fg-2)}
details.tree>pre{margin:0;padding:8px 10px;border-top:1px solid var(--line);overflow-x:auto;max-height:360px;line-height:1.45}
.empty{padding:14px 8px;color:var(--fg-3)}
[hidden]{display:none!important}
@media (max-width:640px){
  .page{padding:24px 16px 48px}
  .facts{gap:10px 24px}
  details.step>summary{grid-template-columns:14px 26px minmax(0,1fr);}
  details.step>summary .dur{grid-column:3;text-align:left}
  .step-body{padding-left:8px}
}
@media print{.toolbar{position:static}}
"#;

const SCRIPT: &str = r#"
(function () {
  'use strict';
  var data = JSON.parse(document.getElementById('trace-data').textContent);
  var trace = data.trace || {};
  var images = data.images || {};
  var root = document.getElementById('app');

  function el(tag, cls, text) {
    var e = document.createElement(tag);
    if (cls) e.className = cls;
    if (text !== undefined && text !== null) e.textContent = String(text);
    return e;
  }
  function add(parent) {
    for (var i = 1; i < arguments.length; i++) if (arguments[i]) parent.appendChild(arguments[i]);
    return parent;
  }
  function fmtMs(ms) {
    if (typeof ms !== 'number' || !isFinite(ms) || ms < 0) return '';
    if (ms < 10) return ms.toFixed(1) + ' ms';
    if (ms < 1000) return Math.round(ms) + ' ms';
    if (ms < 60000) return (ms / 1000).toFixed(2) + ' s';
    var s = Math.round(ms / 1000);
    return Math.floor(s / 60) + ' min ' + ('0' + (s % 60)).slice(-2) + ' s';
  }
  function fmtBytes(b) {
    if (typeof b !== 'number') return '';
    if (b < 1024) return b + ' B';
    if (b < 1048576) return (b / 1024).toFixed(1) + ' KB';
    return (b / 1048576).toFixed(1) + ' MB';
  }
  function httpBad(h) {
    return (typeof h.status === 'number' && h.status >= 400) || (h.state && h.state !== 'completed');
  }
  function statusMark(status) {
    switch (status) {
      case 'passed': return ['✓', 'pass', 'Passed'];
      case 'failed': return ['✗', 'fail', 'Failed'];
      case 'warned': return ['~', 'warn', 'Optional, failed'];
      case 'skipped': return ['–', 'skip', 'Skipped'];
      default: return ['·', 'skip', String(status || '')];
    }
  }
  function imageFor(path) {
    var src = path && images[path];
    if (typeof src !== 'string' || src.indexOf('data:image/') !== 0) return null;
    var img = el('img');
    img.src = src;
    img.alt = 'Screenshot';
    img.loading = 'lazy';
    return img;
  }
  function badge(text, kind, title) {
    var b = el('span', 'badge' + (kind ? ' b-' + kind : ''), text);
    if (title) b.title = title;
    return b;
  }
  function headerList(title, headers) {
    if (!headers || typeof headers !== 'object') return null;
    var keys = Object.keys(headers);
    if (!keys.length) return null;
    var d = el('details', 'sub');
    add(d, el('summary', null, title + ' (' + keys.length + ')'));
    var dl = el('dl');
    keys.forEach(function (k) { add(dl, el('dt', null, k), el('dd', null, headers[k])); });
    return add(d, dl);
  }

  var flows = Array.isArray(trace.flows) ? trace.flows : [];
  var stepRows = [];

  function renderHttp(list) {
    var wrap = el('div', 'table-wrap');
    var table = el('table', 'http');
    var head = el('tr');
    ['Method', 'URL', 'Status', 'Time', 'Size'].forEach(function (h) { add(head, el('th', null, h)); });
    add(table, add(el('thead'), head));
    var body = el('tbody');
    list.forEach(function (h) {
      var tr = el('tr', httpBad(h) ? 'bad' : null);
      var status = typeof h.status === 'number' ? String(h.status) : '';
      if (h.state && h.state !== 'completed') status = (status ? status + ' ' : '') + h.state;
      var urlCell = el('td', 'url');
      add(urlCell, el('div', null, h.url || ''));
      if (h.error) add(urlCell, el('div', 'fail', h.error));
      var extra = el('div');
      add(extra, headerList('Request headers', h.requestHeaders), headerList('Response headers', h.responseHeaders));
      if (extra.childNodes.length) add(urlCell, extra);
      var size = [];
      if (typeof h.requestBytes === 'number') size.push('↑ ' + fmtBytes(h.requestBytes));
      if (typeof h.responseBytes === 'number') size.push('↓ ' + fmtBytes(h.responseBytes));
      var time = fmtMs(h.durationMs);
      if (typeof h.headersMs === 'number') time += ' (headers ' + fmtMs(h.headersMs) + ')';
      add(tr, el('td', 'mono', h.method || ''), urlCell, el('td', 'num', status), el('td', 'num', time),
        el('td', 'num', size.join(' ')));
      add(body, tr);
    });
    return add(wrap, add(table, body));
  }

  // Same wording as `settle_timeout_note` in `orchestra/describe.rs`.
  function busySource(e) {
    if (!e || typeof e.source !== 'string') return null;
    function counted(n, one, many) { return n + ' ' + (n === 1 ? one : many); }
    var count = typeof e.count === 'number' ? e.count : 1;
    if (typeof e.more === 'number') {
      if (e.source === 'channel') return counted(e.more, 'more platform channel', 'more platform channels');
      if (e.source === 'animation') return counted(e.more, 'more animation', 'more animations');
      return e.more + ' more ' + e.source;
    }
    switch (e.source) {
      case 'channel':
        var details = [];
        if (typeof e.pendingMs === 'number') details.push('pending ' + e.pendingMs + ' ms');
        if (typeof e.calls === 'number') details.push(e.calls + ' calls');
        return 'platform channel ' + (e.channel || '?') + (details.length ? ' (' + details.join(', ') + ')' : '');
      case 'http': return counted(count, 'HTTP request', 'HTTP requests');
      case 'images': return counted(count, 'image load', 'image loads');
      case 'keyboard': return 'the soft keyboard';
      case 'animation':
        return ['animation', e.widget || '?', Math.round((e.fraction || 0) * 100) + '%', e.status || ''].join(' ').trim();
      case 'frames': return 'a UI that keeps changing';
      default: return e.source;
    }
  }

  function settleTimeoutNote(driver) {
    if (!driver || typeof driver !== 'object') return null;
    var reply = [driver, driver.idle].filter(function (r) {
      return r && typeof r === 'object' && (r.settle === 'timeout' || r.state === 'timeout');
    })[0];
    if (!reply || !Array.isArray(reply.settleBusy)) return null;
    var waitedFor = reply.settleBusy.map(busySource).filter(Boolean);
    return waitedFor.length ? 'settle timed out, still waiting for ' + waitedFor.join(', ') : null;
  }

  function renderStep(flowIdx, s) {
    var mark = statusMark(s.status);
    var det = el('details', 'step');
    det.id = 'f' + flowIdx + '-s' + s.index;
    var sum = el('summary');
    var st = el('span', 'st ' + mark[1], mark[0]);
    st.title = mark[2];
    var desc = el('span', 'desc', s.description || s.command || '');
    if (typeof s.depth === 'number' && s.depth > 0) desc.style.paddingLeft = (Math.min(s.depth, 8) * 14) + 'px';
    var badges = el('span', 'badges');
    var http = Array.isArray(s.http) ? s.http : [];
    var issues = Array.isArray(s.appIssues) ? s.appIssues : [];
    var httpErrors = http.filter(httpBad).length;
    if (s.lane && s.lane !== 'flutter') add(badges, badge(s.lane, s.lane === 'native' || s.lane === 'hybrid' ? 'info' : null, 'Lane'));
    if (typeof s.attempt === 'number') add(badges, badge('attempt ' + s.attempt, s.attempt > 1 ? 'warn' : null, 'retry attempt'));
    if (s.optional) add(badges, badge('optional'));
    if (issues.length) add(badges, badge('⚠ ' + issues.length + ' app issue' + (issues.length === 1 ? '' : 's'), 'warn'));
    if (http.length) add(badges, badge(http.length + ' HTTP' + (httpErrors ? ' · ' + httpErrors + ' error' + (httpErrors === 1 ? '' : 's') : ''), httpErrors ? 'fail' : null));
    if (badges.childNodes.length) add(desc, badges);
    add(sum, st, el('span', 'idx mono', '#' + (s.index + 1)), desc, el('span', 'dur', fmtMs(s.ms)));
    add(det, sum);

    var body = el('div', 'step-body');
    var meta = [];
    if (s.command) meta.push(s.command);
    if (typeof s.startMs === 'number') meta.push('at +' + fmtMs(s.startMs));
    if (s.lane) meta.push('lane ' + s.lane);
    add(body, el('div', 'muted mono', meta.join(' · ')));
    if (s.error) add(body, el('pre', 'box err', s.error));
    if (s.warning) add(body, el('div', 'warn', '⚠ ' + s.warning));
    var settleNote = settleTimeoutNote(s.driver);
    if (settleNote) add(body, el('div', 'warn', '⚠ ' + settleNote));
    var failure = s.failure && typeof s.failure === 'object' ? s.failure : null;
    if (failure && Array.isArray(failure.suggestions) && failure.suggestions.length) {
      var sec = el('div');
      add(sec, el('h3', null, 'Did you mean'));
      var ul = el('ul', 'plain');
      failure.suggestions.forEach(function (g) {
        var li = el('li');
        add(li, el('code', null, (g.field || '') + '="' + (g.value || '') + '"'), document.createTextNode(' — ' + (g.reason || '')));
        add(ul, li);
      });
      add(body, add(sec, ul));
    }
    if (issues.length) {
      var isec = el('div');
      add(isec, el('h3', null, 'App health'));
      var iul = el('ul', 'plain');
      issues.forEach(function (i) {
        var text = (i.kind || 'issue') + ': ' + (i.message || '');
        if (typeof i.count === 'number' && i.count > 1) text += ' (×' + i.count + ')';
        if (i.ignored) text += ' (ignored)';
        var li = el('li', i.ignored ? 'muted' : 'warn', text);
        if (i.summary && i.summary !== i.message) {
          var d = el('details', 'sub');
          add(d, el('summary', null, 'details'), el('pre', 'box', i.summary));
          add(li, d);
        }
        add(iul, li);
      });
      add(body, add(isec, iul));
    }
    if (http.length) {
      var hsec = el('div');
      add(hsec, el('h3', null, 'HTTP requests'), renderHttp(http));
      add(body, hsec);
    }
    var shotPath = (s.screenshot && s.screenshot.path) || (failure && failure.screenshot);
    var img = imageFor(shotPath);
    if (img) {
      var fig = el('figure', 'shot');
      add(fig, img, el('figcaption', 'mono', shotPath));
      add(body, fig);
    } else if (shotPath) {
      add(body, el('div', 'muted mono', 'Screenshot: ' + shotPath));
    }
    var tree = s.tree || (failure && failure.hierarchy);
    if (tree) {
      var td = el('details', 'tree');
      var label = s.tree ? 'UI tree after step' : 'UI tree at failure';
      var src = s.treeSource || (failure && failure.hierarchySource);
      var n = typeof s.treeElements === 'number' ? s.treeElements : failure && failure.totalElements;
      var bits = [];
      if (src) bits.push(src);
      if (typeof n === 'number') bits.push(n + ' element' + (n === 1 ? '' : 's'));
      add(td, el('summary', null, label + (bits.length ? ' (' + bits.join(', ') + ')' : '')), el('pre', 'mono', tree));
      add(body, td);
    }
    add(det, body);
    stepRows.push({
      el: det,
      failed: s.status === 'failed',
      health: issues.length > 0,
      httpErr: httpErrors > 0,
      text: [s.description, s.command, s.error, s.warning, settleNote].concat(http.map(function (h) { return h.url; }))
        .concat(issues.map(function (i) { return i.message; })).join('\n').toLowerCase()
    });
    return det;
  }

  function renderFlow(flow, flowIdx) {
    var section = el('section', 'flow');
    var head = el('div', 'flow-head');
    var mark = statusMark(flow.status);
    var title = el('h2');
    add(title, el('span', mark[1], mark[0] + ' '), document.createTextNode(flow.name || 'flow'));
    var meta = [fmtMs(flow.totalMs)];
    if (flow.lane) meta.push('lane ' + flow.lane);
    if (flow.http === 'enabled') meta.push('HTTP log on');
    else if (flow.http === 'unsupported') meta.push('HTTP log unavailable (no Tapwing driver connection: --input native, black-box app, or an older driver)');
    if (typeof flow.httpDropped === 'number' && flow.httpDropped > 0) meta.push(flow.httpDropped + ' HTTP entries dropped');
    if (flow.blackbox) meta.push(flow.blackboxReason ? 'black-box (' + flow.blackboxReason + ')' : 'black-box');
    if (flow.graybox) meta.push(Array.isArray(flow.grayboxReasons) && flow.grayboxReasons.length ? 'gray-box (' + flow.grayboxReasons.join('; ') + ')' : 'gray-box');
    add(head, title, el('span', 'flow-meta', meta.join(' · ')));
    add(section, head);
    if (flow.file || flow.path) add(section, el('div', 'muted mono', flow.file || flow.path));
    if (flow.error) add(section, el('pre', 'flow-err', flow.error));

    var steps = Array.isArray(flow.steps) ? flow.steps : [];
    var top = steps.filter(function (s) { return !s.depth; });
    var total = top.reduce(function (a, s) { return a + (typeof s.ms === 'number' ? s.ms : 0); }, 0);
    if (top.length && total > 0) {
      var bar = el('div', 'timeline');
      bar.setAttribute('role', 'img');
      bar.setAttribute('aria-label', 'Step timeline');
      top.forEach(function (s) {
        var seg = el('span', 's-' + (s.status || 'passed'));
        seg.style.flexGrow = String(Math.max(typeof s.ms === 'number' ? s.ms : 0, total / 400));
        seg.title = '#' + (s.index + 1) + ' ' + (s.description || '') + ' · ' + fmtMs(s.ms);
        seg.addEventListener('click', function () {
          var target = document.getElementById('f' + flowIdx + '-s' + s.index);
          if (target) { target.open = true; target.scrollIntoView({ block: 'center' }); }
        });
        add(bar, seg);
      });
      add(section, bar);
    }
    var list = el('div', 'steps');
    var firstRow = stepRows.length;
    steps.forEach(function (s) { add(list, renderStep(flowIdx, s)); });
    var empty = el('div', 'empty', steps.length ? 'No steps match the filter.' : 'No steps recorded.');
    empty.hidden = steps.length > 0;
    add(section, list, empty);
    flowSections.push({ el: section, rows: stepRows.slice(firstRow), empty: empty, failed: flow.status === 'failed' });
    return section;
  }

  var flowSections = [];
  var header = el('header');
  add(header, el('h1', null, 'Tapwing trace'));
  var passed = flows.filter(function (f) { return f.status === 'passed'; }).length;
  var stepCount = 0, httpCount = 0, issueCount = 0;
  flows.forEach(function (f) {
    (f.steps || []).forEach(function (s) {
      stepCount++;
      httpCount += (s.http || []).length;
      issueCount += (s.appIssues || []).length;
    });
  });
  var shard = trace.shard || {};
  var facts = el('dl', 'facts');
  [['Flows', passed + ' / ' + flows.length + ' passed'], ['Steps', stepCount], ['App issues', issueCount],
   ['HTTP requests', httpCount], ['Device', (shard.device || '–') + (shard.platform ? ' · ' + shard.platform : '')],
   ['Created', trace.createdEpochMs ? new Date(trace.createdEpochMs).toLocaleString() : '–']].forEach(function (f) {
    add(facts, add(el('div'), el('dt', null, f[0]), el('dd', null, f[1])));
  });
  add(header, facts);
  add(root, header);

  var toolbar = el('div', 'toolbar');
  var filters = [['all', 'All'], ['failed', 'Failed'], ['health', 'App issues'], ['http', 'HTTP errors']];
  var active = 'all';
  var buttons = filters.map(function (f) {
    var b = el('button', null, f[1]);
    b.type = 'button';
    b.setAttribute('aria-pressed', f[0] === active ? 'true' : 'false');
    b.addEventListener('click', function () {
      active = f[0];
      buttons.forEach(function (x, i) { x.setAttribute('aria-pressed', filters[i][0] === active ? 'true' : 'false'); });
      apply();
    });
    return b;
  });
  var search = el('input');
  search.type = 'search';
  search.placeholder = 'Search steps, URLs, errors';
  search.setAttribute('aria-label', 'Search steps');
  search.addEventListener('input', apply);
  buttons.forEach(function (b) { add(toolbar, b); });
  add(toolbar, search);
  add(root, toolbar);

  flows.forEach(function (f, i) { add(root, renderFlow(f, i)); });
  if (!flows.length) add(root, el('div', 'empty', 'No flows ran.'));

  function apply() {
    var q = search.value.trim().toLowerCase();
    flowSections.forEach(function (fs) {
      var visible = 0;
      fs.rows.forEach(function (r) {
        var ok = active === 'all' || (active === 'failed' && r.failed) || (active === 'health' && r.health) ||
          (active === 'http' && r.httpErr);
        if (ok && q) ok = r.text.indexOf(q) >= 0;
        r.el.hidden = !ok;
        if (ok) visible++;
      });
      var filtering = active !== 'all' || q;
      fs.el.hidden = !!filtering && visible === 0 && !(active === 'failed' && fs.failed && !q);
      fs.empty.hidden = visible > 0 || !fs.rows.length;
    });
  }
  // Failing steps open by default so the first thing on screen is the failure.
  stepRows.forEach(function (r) { if (r.failed) r.el.open = true; });
}());
"#;

/// Escapes JSON for embedding inside a `<script>` element.
fn script_safe_json(value: &Value) -> String {
    let raw = serde_json::to_string(value).unwrap_or_else(|_| "{}".into());
    let mut out = String::with_capacity(raw.len() + 64);
    for ch in raw.chars() {
        match ch {
            '<' => out.push_str("\\u003c"),
            '>' => out.push_str("\\u003e"),
            '&' => out.push_str("\\u0026"),
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            other => out.push(other),
        }
    }
    out
}

/// Screenshot paths referenced by the trace (per-step and failure shots).
fn screenshot_paths(trace: &Value) -> Vec<String> {
    let mut out = Vec::new();
    for flow in trace["flows"].as_array().into_iter().flatten() {
        for step in flow["steps"].as_array().into_iter().flatten() {
            for p in [step["screenshot"]["path"].as_str(), step["failure"]["screenshot"].as_str()].into_iter().flatten()
            {
                out.push(p.to_string());
            }
        }
    }
    out
}

/// Renders the self-contained viewer for a `tapwing.trace/1` document.
/// Screenshots on disk are inlined as downscaled JPEG `data:` URIs.
pub fn render_trace_html(trace: &Value) -> String {
    let mut images: BTreeMap<String, String> = BTreeMap::new();
    for path in screenshot_paths(trace) {
        if images.contains_key(&path) {
            continue;
        }
        if let Some(uri) = super::thumbnail_data_uri(Path::new(&path)) {
            images.insert(path, uri);
        }
    }
    let payload = json!({ "trace": trace, "images": images });
    let title = match trace["flows"].as_array().map(Vec::as_slice) {
        Some([one]) => format!("{} · Tapwing trace", one["name"].as_str().unwrap_or("flow")),
        _ => "Tapwing trace".to_string(),
    };
    format!(
        "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n<title>{}</title>\n<style>{STYLES}</style>\n</head>\n<body>\n<main class=\"page\" id=\"app\"><noscript>The trace viewer needs JavaScript. The same data is in the trace JSON next to this file.</noscript></main>\n<script id=\"trace-data\" type=\"application/json\">{}</script>\n<script>{SCRIPT}</script>\n</body>\n</html>\n",
        escape_html(&title),
        script_safe_json(&payload),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hostile_trace() -> Value {
        json!({
            "schema": super::super::SCHEMA,
            "flows": [{
                "name": "</script><script>alert('name')</script>",
                "status": "failed",
                "error": "<img src=x onerror=alert(1)>",
                "steps": [{
                    "index": 0,
                    "command": "tapOn",
                    "description": "tapOn \"</script><script>alert(1)</script>\"",
                    "status": "failed",
                    "ms": 12.0,
                    "http": [{
                        "method": "GET",
                        "url": "https://evil.test/<script>?q=</script>",
                        "state": "completed",
                        "status": 500,
                        "responseHeaders": { "x-evil": "<!-- --><script>alert(2)</script>" }
                    }]
                }]
            }]
        })
    }

    #[test]
    fn embedded_data_cannot_break_out_of_the_script_block() {
        let html = render_trace_html(&hostile_trace());
        // Exactly the two script elements the template has.
        assert_eq!(html.matches("<script").count(), 2, "{html}");
        assert_eq!(html.matches("</script>").count(), 2);
        assert!(!html.contains("<img src=x"));
        assert!(!html.contains("<!--"));
        // The title is HTML-escaped.
        assert!(html.contains(
            "<title>&lt;/script&gt;&lt;script&gt;alert(&#39;name&#39;)&lt;/script&gt; · Tapwing trace</title>"
        ));
        // The payload round-trips.
        let start = html.find("type=\"application/json\">").unwrap() + "type=\"application/json\">".len();
        let end = start + html[start..].find("</script>").unwrap();
        let parsed: Value = serde_json::from_str(&html[start..end]).unwrap();
        assert_eq!(parsed["trace"], hostile_trace());
    }

    #[test]
    fn the_viewer_words_a_settle_timeout_like_the_console_does() {
        let reply = json!({ "settle": "timeout", "settleBusy": [{ "source": "keyboard" }] });
        let console = crate::orchestra::settle_timeout_note(&reply).unwrap();
        let lead = console.strip_suffix("the soft keyboard").unwrap();
        assert!(SCRIPT.contains(&format!("'{lead}'")), "viewer lead text differs from {lead:?}");
        assert!(SCRIPT.contains("'the soft keyboard'"));
        assert!(SCRIPT.contains("settleTimeoutNote(s.driver)"));
    }

    #[test]
    fn template_has_zero_external_urls_and_renders_with_text_content_only() {
        let html = render_trace_html(&json!({ "flows": [] }));
        assert!(!html.contains("http://") && !html.contains("https://"), "external URL in template");
        assert!(!html.contains("//cdn") && !html.contains("@import"));
        assert!(
            !SCRIPT.contains("innerHTML") && !SCRIPT.contains("outerHTML") && !SCRIPT.contains("insertAdjacentHTML")
        );
        assert!(!SCRIPT.contains("document.write") && !SCRIPT.contains("eval("));
        assert!(SCRIPT.contains("indexOf('data:image/') !== 0"), "image sources restricted to data URIs");
        // With hostile data, URLs only ever appear inside the JSON payload.
        let html = render_trace_html(&hostile_trace());
        let data_start = html.find("<script id=\"trace-data\"").unwrap();
        let data_end = data_start + html[data_start..].find("</script>").unwrap();
        let outside = format!("{}{}", &html[..data_start], &html[data_end..]);
        assert!(!outside.contains("evil.test"));
        assert!(STYLES.contains("prefers-color-scheme:dark"));
        assert!(STYLES.contains("max-width:640px"));
    }
}
