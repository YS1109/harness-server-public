/* No server or third-party packages required. These are DOM/request contract tests;
 * visual layout and native browser focus/validation still need browser verification. */
'use strict';
const assert = require('node:assert/strict');
const { test } = require('node:test');
const { readFileSync } = require('node:fs');
const { resolve } = require('node:path');
const vm = require('node:vm');
const { randomUUID } = require('node:crypto');
const root = resolve(__dirname, '..');
const html = readFileSync(resolve(root, 'src/console.html'), 'utf8');
const script = readFileSync(resolve(root, 'src/console.js'), 'utf8');
const tick = () => new Promise((done) => setImmediate(done));

class Node {
  constructor(tag) {
    this.tag = tag; this.children = []; this.attrs = {}; this.dataset = {}; this.listeners = {};
    this._text = ''; this._value = undefined; this.hidden = false; this.disabled = false;
    this.classList = { toggle: (name, active) => {
      const names = new Set((this.className || '').split(' ').filter(Boolean));
      if (active) names.add(name); else names.delete(name);
      this.className = [...names].join(' ');
    } };
  }
  append(...children) { this.children.push(...children); }
  replaceChildren(...children) { this._text = ''; this.children = children; this._value = undefined; }
  set textContent(value) { this._text = String(value ?? ''); this.children = []; }
  get textContent() { return this._text + this.children.map((child) => child.textContent).join(''); }
  set value(value) { this._value = String(value); }
  get value() { return this._value ?? (this.tag === 'select' ? (this.options.find((o) => o.attrs.selected !== undefined) || this.options[0])?.value || '' : this.attrs.value || ''); }
  get options() { return this.querySelectorAll('option'); }
  setAttribute(name, value) {
    this.attrs[name] = String(value);
    if (name === 'id') this.id = value;
    if (name.startsWith('data-')) this.dataset[name.slice(5).replace(/-([a-z])/g, (_, c) => c.toUpperCase())] = value;
    if (name === 'hidden') this.hidden = true;
    if (name === 'disabled') this.disabled = true;
  }
  removeAttribute(name) { delete this.attrs[name]; }
  matches(selector) {
    const attr = selector.match(/^\[([^=\]]+)(?:="([^"]*)")?\]$/);
    if (!attr) return this.tag === selector;
    const key = attr[1].slice(5).replace(/-([a-z])/g, (_, c) => c.toUpperCase());
    const value = attr[1].startsWith('data-') ? this.dataset[key] : this.attrs[attr[1]];
    return value !== undefined && (attr[2] === undefined || value === attr[2]);
  }
  querySelectorAll(selector) {
    const selectors = selector.split(',').map((s) => s.trim()); const found = [];
    const visit = (parent) => parent.children.forEach((child) => {
      if (selectors.some((s) => child.matches(s))) found.push(child);
      visit(child);
    });
    visit(this); return found;
  }
  querySelector(selector) { return this.querySelectorAll(selector)[0] || null; }
  addEventListener(type, listener) { (this.listeners[type] ||= []).push(listener); }
  dispatch(type, fields = {}) {
    const event = { preventDefault() {}, ...fields };
    return Promise.all((this.listeners[type] || []).map((listener) => listener(event)));
  }
  click() { return this.disabled ? Promise.resolve() : this.dispatch('click'); }
  requestSubmit() { return this.dispatch('submit'); }
  reset() { this.querySelectorAll('input, textarea, select').forEach((control) => { control._value = undefined; }); }
  focus() { this.focused = true; }
  showModal() { this.open = true; }
  close() { this.open = false; }
}
function setup(session = new Map()) {
  const doc = new Node('document'); const stack = [doc]; const ids = new Map();
  const voids = new Set(['meta', 'link', 'input', 'br']);
  for (const token of html.matchAll(/<[^>]+>|[^<]+/g)) {
    const text = token[0];
    if (text.startsWith('</')) { stack.pop(); continue; }
    if (text.startsWith('<!')) continue;
    if (!text.startsWith('<')) { stack.at(-1)._text += text; continue; }
    const tag = text.match(/^<([a-z0-9-]+)/i)?.[1]; if (!tag) continue;
    const node = new Node(tag);
    for (const attr of text.slice(tag.length + 1, -1).matchAll(/([^\s=]+)(?:="([^"]*)")?/g)) node.setAttribute(attr[1], attr[2] ?? '');
    if (node.id) { assert(!ids.has(node.id), `duplicate ID: ${node.id}`); ids.set(node.id, node); }
    stack.at(-1).append(node); if (!voids.has(tag)) stack.push(node);
  }
  doc.getElementById = (id) => { assert(ids.has(id), `missing HTML element: ${id}`); return ids.get(id); };
  doc.createElement = (tag) => new Node(tag);
  const calls = []; const window = new Node('window'); window.confirm = () => true;
  window.sessionStorage = { getItem: (key) => session.get(key) ?? null, setItem: (key, value) => session.set(key, value), removeItem: (key) => session.delete(key) };
  const fetch = (path, options) => new Promise((resolveRequest, reject) => {
    // Deliberately allow replies after abort: epoch/sequence checks must protect UI too.
    calls.push({ path, options, reject, reply: (data, status = 200) => resolveRequest({ ok: status < 400, status, json: typeof data === 'function' ? data : async () => data }) });
  });
  const context = vm.createContext({ document: doc, window, fetch, AbortController, URLSearchParams, setTimeout, clearTimeout, crypto: { randomUUID } });
  vm.runInContext(script, context);
  return { $: doc.getElementById, calls, window, async fire(id, type = 'click', fields) { const result = doc.getElementById(id).dispatch(type, fields); await tick(); return result; } };
}
const overview = {
  namespaces: [{ id: 'shared', name: '公共', status: 'ACTIVE', sha256: 'namespace-sha' }, { id: 'device', name: '设备', status: 'ACTIVE' }],
  agents: [{ id: 'agent', name: 'Agent', sha256: 'agent-sha', read_namespaces: ['shared'], write_namespaces: ['shared'], default_read_namespaces: ['shared'], default_write_namespace: 'shared' }],
};
const runtime = { routing_evaluation: { latest: null }, sample_at: '2026-10-15T01:00:00Z', cache_age_seconds: 4, cache_hit: true, version: 'test', uptime_seconds: 42, window_days: 30, storage: { documents: 0, document_limit: 4096, disk_bytes: null }, metrics: { observed_calls: 0, handler_ms_p50: null, returned_characters: null }, metrics_scope: '仅业务处理器', net_benefit: '尚未测量', namespaces: [] };
async function login(ui) {
  ui.$('admin-token').value = 'test-admin-token'; const pending = ui.fire('login-form', 'submit');
  await tick(); ui.calls.at(-1).reply(overview); await tick();
  assert.equal(ui.calls.at(-1).path, '/admin/api/runtime?days=30');
  ui.calls.at(-1).reply(runtime); await pending;
}
function last(ui, fragment) { const call = ui.calls.at(-1); assert(call.path.includes(fragment), call.path); return call; }

// Async event callbacks that intentionally do not return their read promise are flushed with tick.
test('console: scoped reads, stale/failed filters, readonly detail and auth lifecycle', async () => {
  const ui = setup(); await login(ui);
  assert.equal(ui.$('admin-token').value, ''); assert.equal(ui.$('runtime-view').hidden, false);
  assert.match(ui.$('runtime-content').textContent, /未观测/); assert.match(ui.$('runtime-content').textContent, /尚未测量/);
  assert.match(ui.$('runtime-content').textContent, /仅业务处理器/); assert.match(ui.$('runtime-content').textContent, /观测调用0/);
  const before = ui.calls.length; await tick(); assert.equal(ui.calls.length, before, 'no polling');
  ui.$('runtime-days').value = '7'; ui.$('runtime-namespace').value = 'device';
  await ui.fire('runtime-filters', 'submit'); const old = last(ui, 'days=7&namespace=device');
  assert.match(ui.$('runtime-content').textContent, /观测调用0/, 'retain the prior scope while querying a new scope');
  assert.match(ui.$('runtime-state').textContent, /当前展示上次快照（全部空间 · 30 天）/);
  ui.$('runtime-days').value = '90'; await ui.fire('runtime-filters', 'submit'); const current = last(ui, 'days=90');
  assert(old.options.signal.aborted); current.reply({ ...runtime, version: 'newest' }); await tick();
  old.reply({ ...runtime, version: 'obsolete' }); await tick();
  assert.match(ui.$('runtime-content').textContent, /newest/); assert.doesNotMatch(ui.$('runtime-content').textContent, /obsolete/);
  await ui.fire('runtime-filters', 'submit'); last(ui, '/runtime').reply({ error: 'unavailable' }, 503); await tick();
  assert.match(ui.$('runtime-content').textContent, /newest/); assert.match(ui.$('runtime-state').textContent, /503.*保留上次快照/);
  const handoffs = ui.fire('nav-handoffs'); await tick(); const handoff = last(ui, 'kind=workstream&sort=updated_desc');
  handoff.reply({ documents: [{ path: 'workstreams/one.md', title: '<img src=x>', kind: 'workstream', status: 'ACTIVE' }], total: 21, remaining: 1, offset: 0 }); await handoffs;
  assert.match(ui.$('browse-body').textContent, /<img src=x>/); assert.equal(ui.$('browse-next').disabled, false);
  await ui.fire('browse-next'); const nextPage = last(ui, 'offset=20');
  await ui.fire('browse-previous'); assert.equal(ui.calls.at(-1), nextPage, 'busy page does not duplicate request');
  nextPage.reply({ documents: [], total: 21, remaining: 0, offset: 20 }); await tick(); assert.equal(ui.$('browse-previous').disabled, false);
  ui.$('browse-namespace').value = 'device'; ui.$('browse-kind').value = 'session'; ui.$('browse-query').value = 'hello & 世界'; ui.$('browse-status').value = 'COMPLETED';
  await ui.fire('browse-filters', 'submit'); const filtered = last(ui, 'namespace=device'); const params = new URLSearchParams(filtered.path.split('?')[1]);
  assert.equal(params.get('offset'), '0'); assert.equal(params.get('q'), 'hello & 世界'); assert.equal(params.get('status'), 'COMPLETED'); assert.equal(params.get('kind'), 'session');
  filtered.reply({ error: 'bad filter' }, 400); await tick(); assert.doesNotMatch(ui.$('browse-body').textContent, /<img/);
  const executions = ui.fire('nav-executions'); await tick(); const exec = last(ui, '/executions?namespace=device');
  exec.reply({ tasks: [{ task_id: 'task-one', title: '执行', status: 'RUNNING', reply_status: 'PENDING' }], total: 1, remaining: 0 }); await executions;
  const taskButton = ui.$('browse-body').querySelector('button'); const detailPending = taskButton.click(); await tick();
  const detail = last(ui, '/execution?namespace=device&task_id=task-one');
  detail.reply({ task: { title: '</pre><script>bad()</script>' }, attempts: { attempts: [] } }); await detailPending;
  assert.equal(ui.$('preview-dialog').open, true); assert.match(ui.$('preview-content').textContent, /<script>/);
  const oldDetailPending = taskButton.click(); await tick(); const oldDetail = ui.calls.at(-1);
  const switched = ui.fire('nav-runtime'); await tick(); assert(oldDetail.options.signal.aborted);
  last(ui, '/runtime?').reply(runtime); await switched; oldDetail.reply({ task: { title: 'late detail' } }); await oldDetailPending;
  assert.equal(ui.$('preview-dialog').open, false); assert.equal(ui.$('preview-content').textContent, '');
  const namespace = ui.$('namespace-list').querySelector('button'); const content = namespace.click(); await tick();
  const documents = last(ui, '/documents?namespace=shared'); assert.equal(new URLSearchParams(documents.path.split('?')[1]).get('sort'), 'path');
  documents.reply({ documents: [], total: 0, remaining: 0 }); await content;
  ui.$('query-filter').value = '目标'; ui.$('status-filter').value = 'IDLE'; ui.$('sort-filter').value = 'updated_desc'; ui.$('project-filter').value = 'demo';
  await ui.fire('filters', 'submit'); const contentParams = new URLSearchParams(last(ui, '/documents?').path.split('?')[1]);
  assert.equal(contentParams.get('q'), '目标'); assert.equal(contentParams.get('project'), 'demo'); assert.equal(contentParams.get('sort'), 'updated_desc'); assert.equal(contentParams.get('status'), 'IDLE');
  const contentRead = ui.calls.at(-1); await ui.fire('logout'); assert(contentRead.options.signal.aborted);
  contentRead.reply({ documents: [{ path: 'late', title: 'late content' }], total: 1 }); await tick();
  assert.equal(ui.$('documents-body').textContent, ''); assert.equal(ui.$('workspace').hidden, true); assert.equal(ui.$('runtime-content').textContent, '');
  for (const call of ui.calls) {
    assert.equal(call.options.mode, 'same-origin'); assert.equal(call.options.redirect, 'error'); assert.equal(call.options.credentials, 'omit');
    assert.equal(call.options.headers.Authorization, 'Bearer test-admin-token'); assert(!call.path.includes('test-admin-token'));
  }
  assert(!/innerHTML|localStorage|setInterval/.test(script));
});

test('console: pending mutation retains exact body/key; CAS conflict closes editor', async () => {
  const ui = setup(); await login(ui);
  const namespacePending = ui.$('namespace-list').querySelector('button').click(); await tick();
  last(ui, '/documents?').reply({ documents: [], total: 0, remaining: 0 }); await namespacePending;
  await ui.fire('edit-namespace'); ui.$('namespace-name').value = '更新名称';
  const saving = ui.fire('namespace-form', 'submit'); await tick(); const write = last(ui, '/namespaces/shared');
  assert.equal(JSON.parse(write.options.body).expected_sha256, 'namespace-sha');
  write.reject(new Error('lost response')); await saving;
  assert.equal(ui.$('pending-banner').hidden, false); assert.equal(ui.$('create-namespace').disabled, true);
  const retrying = ui.fire('retry-mutation'); await tick(); const retry = last(ui, '/namespaces/shared');
  assert.equal(retry.options.body, write.options.body); assert.equal(retry.options.headers['Idempotency-Key'], write.options.headers['Idempotency-Key']);
  retry.reply({ error: 'conflict' }, 409); await tick(); last(ui, '/overview').reply(overview); await tick();
  last(ui, '/documents?').reply({ documents: [], total: 0, remaining: 0 }); await retrying;
  assert.equal(ui.$('pending-banner').hidden, true); assert.equal(ui.$('namespace-dialog').open, false); assert.match(ui.$('notice').textContent, /版本冲突/);
  await ui.fire('global-agents'); const editAgent = ui.$('agents-list').querySelector('button'); await editAgent.click();
  const agentSaving = ui.fire('agent-form', 'submit'); await tick(); const agentWrite = last(ui, '/agents/agent');
  assert.equal(JSON.parse(agentWrite.options.body).expected_sha256, 'agent-sha');
  agentWrite.reply({}, 200); await tick(); last(ui, '/overview').reply(overview); await agentSaving;
  assert.equal(ui.$('agent-dialog').open, false); assert.equal(ui.$('pending-banner').hidden, true);
});

test('console: abort during JSON decoding, reconnect, offline and keyboard navigation', async () => {
  const ui = setup(); await login(ui);
  for (const id of ['project-filter', 'browse-project']) {
    // HTML pattern uses Unicode Sets mode in current browsers; a bare hyphen is invalid.
    const pattern = new RegExp(`^(?:${ui.$(id).attrs.pattern})$`, 'v');
    assert(pattern.test('demo-01')); assert(!pattern.test('../demo')); assert(!pattern.test('Demo'));
  }
  await ui.fire('runtime-filters', 'submit'); const decoding = last(ui, '/runtime?');
  let finishDecode; decoding.reply(() => new Promise((done) => { finishDecode = done; })); await tick();
  await ui.fire('logout'); assert(decoding.options.signal.aborted);
  await login(ui); finishDecode({ ...runtime, version: 'previous-login' }); await tick();
  assert.doesNotMatch(ui.$('runtime-content').textContent, /previous-login/);
  const callsBeforeOffline = ui.calls.length;
  await ui.window.dispatch('offline'); assert.match(ui.$('notice').textContent, /网络已断开/);
  await ui.window.dispatch('online'); assert.match(ui.$('notice').textContent, /手动刷新|点击刷新/);
  assert.equal(ui.calls.length, callsBeforeOffline, 'network recovery does not poll');
  const content = ui.$('namespace-list').querySelector('button').click(); await tick();
  const contentRead = last(ui, '/documents?');
  await ui.fire('tab-content', 'keydown', { key: 'ArrowRight' });
  assert.equal(ui.$('tab-members').focused, true); assert.equal(ui.$('panel-members').hidden, false);
  assert(contentRead.options.signal.aborted); contentRead.reply({ documents: [{ title: 'stale tab', path: 'stale.md' }], total: 1 }); await content;
  assert.equal(ui.$('documents-body').textContent, '');
  const opening = ui.fire('nav-runtime'); await tick();
  last(ui, '/runtime?').reply({ error: 'test-admin-token rejected' }, 401); await opening;
  assert.match(ui.$('runtime-state').textContent, /重新连接/); assert(!ui.$('runtime-state').textContent.includes('test-admin-token'));
  await ui.fire('logout');
});

test('console: independent browse filters and offsets survive navigation and overview refresh', async () => {
  const ui = setup(); await login(ui);
  let opening = ui.fire('nav-handoffs'); await tick();
  last(ui, 'namespace=shared').reply({ documents: [], total: 0 }); await opening;
  ui.$('browse-namespace').value = 'device'; ui.$('browse-kind').value = 'session';
  ui.$('browse-query').value = '交接'; ui.$('browse-project').value = 'project-a'; ui.$('browse-status').value = 'COMPLETED';
  await ui.fire('browse-filters', 'submit'); last(ui, 'namespace=device').reply({ documents: [{ path: 'session.md' }], total: 41, remaining: 40 }); await tick();
  await ui.fire('browse-next'); last(ui, 'offset=20').reply({ documents: [{ path: 'next.md' }], total: 41, remaining: 20 }); await tick();
  opening = ui.fire('nav-executions'); await tick();
  let params = new URLSearchParams(last(ui, '/executions?').path.split('?')[1]);
  assert.equal(params.get('namespace'), 'device', 'first entry inherits the current selection');
  assert.equal(params.get('q'), null); assert.equal(params.get('offset'), '0');
  ui.calls.at(-1).reply({ tasks: [], total: 0 }); await opening;
  ui.$('browse-namespace').value = 'shared'; ui.$('browse-query').value = '执行';
  ui.$('browse-project').value = 'project-b'; ui.$('browse-status').value = 'RUNNING';
  await ui.fire('browse-filters', 'submit'); ui.calls.at(-1).reply({ tasks: [{ task_id: 'one' }], total: 61, remaining: 60 }); await tick();
  await ui.fire('browse-next'); ui.calls.at(-1).reply({ tasks: [{ task_id: 'two' }], total: 61, remaining: 40 }); await tick();
  await ui.fire('browse-next'); ui.calls.at(-1).reply({ tasks: [{ task_id: 'three' }], total: 61, remaining: 20 }); await tick();
  for (const [view, namespace, q, project, status, offset, kind] of [
    ['handoffs', 'device', '交接', 'project-a', 'COMPLETED', '20', 'session'],
    ['executions', 'shared', '执行', 'project-b', 'RUNNING', '40', null],
  ]) {
    opening = ui.fire(`nav-${view}`); await tick();
    params = new URLSearchParams(ui.calls.at(-1).path.split('?')[1]);
    for (const [key, value] of Object.entries({ namespace, q, project, status, offset, kind })) assert.equal(params.get(key), value, `${view} ${key}`);
    assert.equal(ui.$('browse-namespace').value, namespace); assert.equal(ui.$('browse-project').value, project);
    ui.calls.at(-1).reply(view === 'handoffs' ? { documents: [], total: 41 } : { tasks: [], total: 61 }); await opening;
    const refreshing = ui.fire('refresh'); await tick(); last(ui, '/overview').reply(overview); await tick();
    assert.equal(new URLSearchParams(ui.calls.at(-1).path.split('?')[1]).get('offset'), offset);
    assert.equal(ui.$('browse-namespace').value, namespace);
    ui.calls.at(-1).reply(view === 'handoffs' ? { documents: [], total: 41 } : { tasks: [], total: 61 }); await refreshing;
  }
  // Unsubmitted search text is retained too, with its page reset when the query changes.
  ui.$('browse-query').value = '未提交搜索';
  opening = ui.fire('nav-runtime'); await tick(); ui.calls.at(-1).reply(runtime); await opening;
  opening = ui.fire('nav-executions'); await tick(); params = new URLSearchParams(ui.calls.at(-1).path.split('?')[1]);
  assert.equal(params.get('q'), '未提交搜索'); assert.equal(params.get('offset'), '0');
  ui.calls.at(-1).reply({ tasks: [], total: 0 }); await opening;
  await ui.fire('logout'); await login(ui);
  opening = ui.fire('nav-executions'); await tick();
  params = new URLSearchParams(ui.calls.at(-1).path.split('?')[1]);
  assert.equal(params.get('namespace'), 'shared'); assert.equal(params.get('q'), null);
  ui.calls.at(-1).reply({ tasks: [], total: 0 }); await opening; await ui.fire('logout');
});

test('console: readable execution checkpoints, bounded attempts and collapsed readonly JSON', async () => {
  const ui = setup(); await login(ui);
  const opening = ui.fire('nav-executions'); await tick();
  ui.calls.at(-1).reply({ tasks: [{ task_id: 'task' }], total: 1 }); await opening;
  const detail = ui.$('browse-body').querySelector('button').click(); await tick();
  const attempts = Array.from({ length: 22 }, (_, index) => ({
    attempt_id: `attempt-${index}`, sequence: 22 - index, status: index ? 'FAILED' : 'RUNNING',
    agent: 'pi', native_session_id: `native-${index}`, created_at: '2026-10-15T01:00:00Z',
    started_at: '2026-10-15T01:01:00Z', updated_at: '2026-10-15T01:02:00Z',
    checkpoint: { current: index ? '历史进度' : '正在验证 <script>literal</script>', completed_steps: ['已定位'], next_steps: ['执行测试'], blockers: ['待服务确认'], verification: ['模拟通过'], artifacts: ['branch:demo'] },
  }));
  ui.calls.at(-1).reply({ task: { task: { task_id: 'task', title: '修复', goal: '完成交付', acceptance: '回归通过', status: 'RUNNING', current_attempt_id: 'attempt-0' } }, attempts: { attempts, remaining: 15 } }); await detail;
  const content = ui.$('execution-detail');
  for (const text of ['运行中', '完成交付', '回归通过', '正在验证 <script>literal</script>', '已定位', '执行测试', '待服务确认', '模拟通过', 'branch:demo', 'native-0', '剩余 15 次']) assert(content.textContent.includes(text), text);
  assert.equal(content.querySelector('ol').children.length, 20);
  assert.equal(ui.$('preview-raw').open, false); assert.match(ui.$('preview-content').textContent, /"checkpoint"/);
  assert.equal(content.querySelectorAll('input, textarea, button').length, 0, 'execution detail has no write controls');
  const before = ui.calls.length;
  await ui.fire('preview-dialog', 'cancel'); assert.equal(ui.calls.length, before);
  assert.equal(content.textContent, ''); assert.equal(ui.$('preview-content').textContent, '');
  const docOpening = ui.fire('nav-handoffs'); await tick(); ui.calls.at(-1).reply({ documents: [{ path: 'one.md' }] }); await docOpening;
  const doc = ui.$('browse-body').querySelector('button').click(); await tick(); ui.calls.at(-1).reply({ content: '# Markdown', sha256: 'abc' }); await doc;
  assert.equal(ui.$('preview-raw').open, true); assert.equal(content.hidden, true); assert.equal(ui.$('preview-content').textContent, '# Markdown');
  await ui.fire('logout');
});

test('console: runtime command summaries, actual samples and opt-in cached disk capacity', async () => {
  const ui = setup(); await login(ui);
  assert(!ui.calls.some((call) => call.path.includes('/storage')), 'runtime never requests a disk scan');
  assert.match(ui.$('routing-content').textContent, /暂无评测/);
  await ui.fire('runtime-filters', 'submit');
  ui.calls.at(-1).reply({ ...runtime, metrics: { observed_calls: 17, grouped_calls: 9, execution_groups: 3, command_metrics: {
    route: { observed_calls: 12, handler_ms_p50: 1.25, handler_ms_p90: 8, lock_wait_ms_p50: 0, lock_wait_ms_p90: null, operation_ms_p50: 2, operation_ms_p90: 3, commit_ms_p50: null, commit_ms_p90: null },
    finish: { observed_calls: 5, handler_ms_p90: 19 },
  }, protocols: { v1: 10, single_request: 7 }, service_versions: { '0.4.0': 17 } } }); await tick();
  assert.match(ui.$('runtime-content').textContent, /分组调用9.*执行组3/);
  const tables = ui.$('runtime-content').querySelectorAll('table');
  const commands = tables.find((table) => table.textContent.startsWith('各命令耗时'));
  assert(commands); assert.match(commands.textContent, /route12P50 1.25 ms \/ P90 8 ms/);
  assert.match(commands.textContent, /P50 0 ms \/ P90 未观测/); assert.match(commands.textContent, /finish5P50 未观测 \/ P90 19 ms/);
  assert.match(ui.$('runtime-content').textContent, /single_request7/); assert.match(ui.$('runtime-content').textContent, /0.4.017/);
  assert.match(ui.$('runtime-state').textContent, /缓存命中 是.*缓存年龄 4 秒/);
  const formattedSample = new Date(runtime.sample_at).toLocaleString('zh-CN', { hour12: false });
  assert(ui.$('runtime-state').textContent.includes(formattedSample));
  const costs = ui.$('runtime-content').textContent;
  const storage = ui.fire('read-storage'); await tick(); last(ui, '/admin/api/storage').reply({ storage: { disk_bytes: 1048576, revisions: 3 }, sample_at: '2026-10-15T01:03:00Z', cache_hit: true, cache_age_seconds: 12 }); await storage;
  assert.match(ui.$('storage-content').textContent, /1 MiB.*保留修订3/); assert.match(ui.$('storage-state').textContent, /缓存年龄 12 秒/);
  let refreshing = ui.fire('read-storage'); await tick(); last(ui, '/storage?fresh=true').reply({ error: 'unavailable' }, 503); await refreshing;
  assert.match(ui.$('storage-state').textContent, /503.*保留上次容量/); assert.match(ui.$('storage-content').textContent, /1 MiB/);
  assert.equal(ui.$('runtime-content').textContent, costs);
  await ui.fire('runtime-filters', 'submit'); last(ui, '/runtime?').reject(new Error('offline')); await tick();
  assert.equal(ui.$('runtime-content').textContent, costs); assert.match(ui.$('runtime-state').textContent, /保留上次快照/);
  ui.$('runtime-namespace').value = 'device'; await ui.fire('runtime-filters', 'submit');
  last(ui, 'namespace=device').reply({ error: 'failed scope' }, 503); await tick();
  assert.equal(ui.$('runtime-content').textContent, costs); assert.match(ui.$('runtime-state').textContent, /保留上次快照（全部空间 · 30 天）/);
  refreshing = ui.fire('read-storage'); await tick(); const stale = last(ui, '/storage?fresh=true');
  const switching = ui.fire('nav-handoffs'); await tick(); assert(stale.options.signal.aborted);
  ui.calls.at(-1).reply({ documents: [] }); await switching;
  stale.reply({ disk_bytes: 999, revisions: 999 }); await refreshing;
  assert.doesNotMatch(ui.$('storage-content').textContent, /999/);
  assert.equal(ui.$('read-storage').disabled, false);
  await ui.fire('logout'); assert.equal(ui.$('storage-content').textContent, '');
});

test('console: routing evaluation uses embedded reports or scoped GET; stale replies cannot cross scopes', async () => {
  const ui = setup(); await login(ui);
  const report = { evaluated_at: '2026-10-15T02:00:00Z', suite_version: 'routing-v2', engine_version: '0.4.0', namespace: 'device', read_namespaces: ['device', 'shared'], suite_fingerprint: 'suite-hash', snapshot_fingerprint: 'fingerprint', scope: { namespace: 'device' }, summary: { positive_correct: 7, positive_evaluated: 8, positive_accuracy: 0.875, missed: 1, negative_evaluated: 2, false_selections: 0, unavailable: 3, total: 13 } };
  ui.$('runtime-namespace').value = 'device'; await ui.fire('runtime-filters', 'submit');
  const before = ui.calls.length; ui.calls.at(-1).reply({ ...runtime, routing_evaluation: { latest: report } }); await tick();
  assert.equal(ui.calls.length, before, 'embedded report avoids a second HTTP request');
  assert.match(ui.$('routing-content').textContent, /routing-v2.*0.4.0.*device.*shared.*suite-hash.*fingerprint/);
  assert.match(ui.$('routing-content').textContent, /7 \/ 8.*87.5%.*误选0.*不可用3.*总数13/);
  const withoutEmbedded = { ...runtime }; delete withoutEmbedded.routing_evaluation;
  await ui.fire('runtime-filters', 'submit'); ui.calls.at(-1).reply(withoutEmbedded); await tick();
  const old = last(ui, '/routing-evaluation?namespace=device');
  ui.$('runtime-namespace').value = ''; await ui.fire('runtime-filters', 'submit'); assert(old.options.signal.aborted);
  ui.calls.at(-1).reply(withoutEmbedded); await tick(); last(ui, '/routing-evaluation').reply({ latest: null }); await tick();
  old.reply({ latest: report }); await tick(); assert.equal(ui.$('routing-content').textContent, '暂无评测');
  await ui.fire('runtime-filters', 'submit'); ui.calls.at(-1).reply(withoutEmbedded); await tick();
  last(ui, '/routing-evaluation').reply({ error: 'offline' }, 503); await tick();
  assert.match(ui.$('routing-state').textContent, /503.*保留上次结果/); assert.match(ui.$('runtime-content').textContent, /观测调用0/);
  assert(ui.calls.every((call) => (call.options.method || 'GET') === 'GET'));
  await ui.fire('logout');
});

test('console: browser reload restores only session filters and never credentials', async () => {
  const session = new Map(); const first = setup(session); await login(first);
  const opening = first.fire('nav-handoffs'); await tick(); first.calls.at(-1).reply({ documents: [] }); await opening;
  first.$('browse-namespace').value = 'device'; first.$('browse-kind').value = 'session';
  first.$('browse-query').value = '需恢复'; first.$('browse-project').value = 'demo'; first.$('browse-status').value = 'ACTIVE';
  await first.fire('browse-filters', 'submit'); first.calls.at(-1).reply({ documents: [{ path: 'one' }], total: 40, remaining: 39 }); await tick();
  await first.fire('browse-next'); first.calls.at(-1).reply({ documents: [{ path: 'two' }], total: 40, remaining: 19 }); await tick();
  await first.window.dispatch('beforeunload');
  assert.equal(session.size, 1); assert(![...session.values()].join('').includes('test-admin-token'));
  const reloaded = setup(session);
  assert.equal(reloaded.calls.length, 0, 'reloading never restores authentication');
  assert.equal(reloaded.$('workspace').hidden, true); assert.equal(reloaded.$('admin-token').value, '');
  await login(reloaded);
  const restored = reloaded.fire('nav-handoffs'); await tick();
  const params = new URLSearchParams(reloaded.calls.at(-1).path.split('?')[1]);
  for (const [key, value] of Object.entries({ namespace: 'device', kind: 'session', q: '需恢复', project: 'demo', status: 'ACTIVE', offset: '20' })) assert.equal(params.get(key), value, key);
  assert.equal(reloaded.$('browse-namespace').value, 'device');
  reloaded.calls.at(-1).reply({ documents: [] }); await restored;
  await reloaded.fire('logout'); assert.equal(session.size, 0, 'explicit logout clears session filters');
  await first.fire('logout');
  // A storage-restricted browser keeps the in-memory behavior and can still authenticate.
  const restricted = setup(); restricted.window.sessionStorage = { getItem() { throw new Error('disabled'); }, setItem() { throw new Error('disabled'); }, removeItem() { throw new Error('disabled'); } };
  await login(restricted);
  const memoryOnly = restricted.fire('nav-executions'); await tick(); restricted.calls.at(-1).reply({ tasks: [] }); await memoryOnly;
  const switchBack = restricted.fire('nav-handoffs'); await tick(); restricted.calls.at(-1).reply({ documents: [] }); await switchBack;
  await restricted.fire('logout');
});

test('console: corrupt and unreadable evaluation reports are distinct from absent reports', async () => {
  const ui = setup(); await login(ui);
  for (const [reason, label] of [['report_corrupt', '报告文件损坏'], ['report_unreadable', '报告文件不可读']]) {
    await ui.fire('runtime-filters', 'submit'); const count = ui.calls.length;
    ui.calls.at(-1).reply({ ...runtime, routing_evaluation: { status: 'unavailable', reason } }); await tick();
    assert.equal(ui.calls.length, count, 'an unavailable embedded report does not trigger another request');
    assert.match(ui.$('routing-content').textContent, /评测结果不可读取/);
    assert(ui.$('routing-content').textContent.includes(label)); assert.doesNotMatch(ui.$('routing-content').textContent, /暂无评测/);
    assert.match(ui.$('runtime-content').textContent, /观测调用0/);
  }
  await ui.fire('runtime-filters', 'submit'); ui.calls.at(-1).reply({ ...runtime, routing_evaluation: null }); await tick();
  assert.equal(ui.$('routing-content').textContent, '暂无评测'); await ui.fire('logout');
});
