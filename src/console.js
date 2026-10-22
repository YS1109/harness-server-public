/* Standalone admin UI. Authentication and unresolved writes live only in this page. */
(() => {
  'use strict';
  const $ = (id) => document.getElementById(id);
  const kinds = { project: '项目', project_topic: '专题', process: '流程', template: '模板', session: '会话', workstream: '工作流', execution_task: '执行任务', execution_attempt: '执行尝试', execution_context: '执行上下文', log: '日志' };
  const statuses = { ACTIVE: '使用中', ARCHIVED: '已归档', IDLE: '空闲', COMPLETED: '已完成', WAITING_USER: '等待用户', QUEUED: '排队中', DISPATCHING: '分派中', RUNNING: '运行中', RECONCILING: '核对中', SUCCEEDED: '已成功', FAILED: '失败', CANCELLED: '已取消', PENDING: '待回复', SENT: '已回复', DELIVERED: '已送达', NONE: '尚无回复', UNKNOWN: '结果未知' };
  const documentStatuses = ['ACTIVE', 'ARCHIVED', 'IDLE', 'RUNNING', 'WAITING_USER', 'COMPLETED'];
  const state = { token: '', epoch: 0, overview: null, namespace: null, view: 'runtime', tab: 'content', offset: 0, kind: '', project: '', query: '', status: '', sort: 'path', total: 0, remaining: 0, documentsBusy: false, documentSequence: 0, previewSequence: 0, overviewSequence: 0, runtimeSequence: 0, browseSequence: 0, browseBusy: false, browseOffset: 0, browseRemaining: 0, browseFilters: null, browsePages: {}, runtimeCache: new Map(), runtimeDisplayed: null, storageReport: null, storageSequence: 0, routingCache: new Map(), pending: null, mutationBusy: false, fresh: false, editor: null, dirty: false };
  const controllers = new Set();
  const readControllers = new Map();

  function element(tag, text, className) {
    const node = document.createElement(tag);
    if (text !== undefined) node.textContent = String(text ?? '');
    if (className) node.className = className;
    return node;
  }
  function button(text, handler, className) {
    const node = element('button', text, className);
    node.type = 'button';
    node.addEventListener('click', handler);
    return node;
  }
  function namespaces() { return state.overview?.namespaces ?? []; }
  function agents() { return state.overview?.agents ?? []; }
  function currentNamespace() { return namespaces().find((n) => n.id === state.namespace); }
  function list(value) { return Array.isArray(value) ? value : []; }
  function names(ids) { return list(ids).map((id) => namespaces().find((n) => n.id === id)?.name || id).join('、') || '未设置'; }
  function notice(message, error = false) {
    $('notice').textContent = message;
    $('notice').className = error ? 'notice error' : 'notice';
    $('notice').hidden = !message;
  }
  function connection(text, status = '') {
    $('connection').textContent = text;
    $('connection').className = `connection ${status}`;
  }
  function errorText(error) {
    if (error.status === 401 || error.status === 403) return '管理员令牌无效或没有管理权限，请退出并重新连接。';
    if (error.status === 409) return '版本冲突，请刷新并重新审阅最新数据。';
    if (error.status) return `请求失败（HTTP ${error.status}）。${error.message || '请检查输入或服务状态。'}`;
    return '连接中断或请求超时，请检查服务连接后重试。';
  }
  function cancelRead(group) {
    readControllers.get(group)?.abort();
    readControllers.delete(group);
  }
  async function request(path, options = {}) {
    // Only fixed, same-origin API paths reach fetch; token never enters a URL or storage.
    if (!path.startsWith('/admin/api/') || path.startsWith('//')) throw new Error('无效请求路径');
    if (options.group) cancelRead(options.group);
    const controller = new AbortController();
    const epoch = state.epoch;
    controllers.add(controller);
    if (options.group) readControllers.set(options.group, controller);
    const timeout = setTimeout(() => controller.abort(), 20000);
    try {
      const response = await fetch(path, {
        method: options.method || 'GET', mode: 'same-origin', credentials: 'omit',
        redirect: 'error', cache: 'no-store', signal: controller.signal,
        headers: { Authorization: `Bearer ${state.token}`, Accept: 'application/json', ...(options.body !== undefined ? { 'Content-Type': 'application/json', 'Idempotency-Key': options.key } : {}) },
        ...(options.body !== undefined ? { body: options.body } : {}),
      });
      if (epoch !== state.epoch) throw new Error('已退出');
      if (!response.ok) {
        const error = new Error('');
        error.status = response.status;
        try {
          const data = await response.json();
          const message = typeof data.error === 'string' ? data.error : data.error?.message || data.message;
          if (typeof message === 'string') error.message = message.replaceAll(state.token, '[已隐藏]').slice(0, 500);
        } catch { /* The HTTP status still gives a definite failure. */ }
        throw error;
      }
      // A successful mutation needs no response body. Refresh is a separate read.
      if (options.body !== undefined) return null;
      const data = await response.json();
      if (epoch !== state.epoch || controller.signal.aborted) throw new Error('请求已取消');
      return data;
    } finally {
      clearTimeout(timeout);
      controllers.delete(controller);
      if (options.group && readControllers.get(options.group) === controller) readControllers.delete(options.group);
    }
  }
  function updateControls() {
    const blocked = !state.fresh || state.mutationBusy || !!state.pending;
    $('create-namespace').disabled = blocked;
    $('edit-namespace').disabled = blocked;
    $('refresh').disabled = state.mutationBusy;
    $('namespace-save').disabled = blocked;
    $('agent-save').disabled = blocked;
    document.querySelectorAll('[data-edit-agent]').forEach((node) => { node.disabled = blocked; });
    ['namespace-form', 'agent-form'].forEach((id) => {
      $(id).querySelectorAll('input, textarea, select').forEach((node) => {
        node.disabled = blocked || node.dataset.locked === 'true';
      });
    });
    $('pending-banner').hidden = !state.pending;
    $('retry-mutation').disabled = state.mutationBusy;
    $('abandon-mutation').disabled = state.mutationBusy;
    $('pending-description').textContent = state.pending ? state.pending.label : '';
  }
  function dirty(value = true) {
    state.dirty = value;
    $('namespace-unsaved').hidden = !value || state.editor?.type !== 'namespace';
    $('agent-unsaved').hidden = !value || state.editor?.type !== 'agent';
  }
  function closeEditor(force = false) {
    if (!force && state.mutationBusy) return false;
    if (!force && state.dirty && !window.confirm('有未保存的修改。确定放弃这些修改？')) return false;
    ['namespace-dialog', 'agent-dialog'].forEach((id) => { if ($(id).open) $(id).close(); });
    state.editor = null;
    dirty(false);
    $('namespace-form').reset();
    $('agent-form').reset();
    $('agent-grants').replaceChildren();
    $('agent-default-write').replaceChildren();
    $('namespace-form-error').textContent = '';
    $('agent-form-error').textContent = '';
    $('agent-identity').textContent = '';
    return true;
  }
  function emptyRow(body, columns, message) {
    const cell = element('td', message, 'empty-row');
    cell.colSpan = columns;
    const row = element('tr'); row.append(cell); body.append(row);
  }
  function renderNavigation() {
    const query = $('namespace-search').value.trim().toLocaleLowerCase();
    const nav = $('namespace-list'); nav.replaceChildren();
    const matches = namespaces().filter((n) => `${n.name} ${n.id}`.toLocaleLowerCase().includes(query));
    for (const namespace of matches) {
      const active = state.view === 'namespace' && namespace.id === state.namespace;
      const node = button('', () => selectNamespace(namespace.id), `namespace-item${active ? ' active' : ''}`);
      if (active) node.setAttribute('aria-current', 'page');
      node.append(element('span', namespace.id === 'shared' ? '共' : '空', 'namespace-icon'));
      const label = element('span');
      label.append(element('strong', namespace.name), element('small', `${namespace.id}${namespace.status === 'ARCHIVED' ? ' · 已归档' : ''}`));
      node.append(label); nav.append(node);
    }
    if (!matches.length) nav.append(element('p', query ? '没有匹配的空间' : '暂无空间', 'hint'));
    for (const [id, view] of [['global-agents', 'agents'], ['nav-runtime', 'runtime'], ['nav-handoffs', 'handoffs'], ['nav-executions', 'executions']]) {
      $(id).classList.toggle('active', state.view === view);
      if (state.view === view) $(id).setAttribute('aria-current', 'page');
      else $(id).removeAttribute('aria-current');
    }
  }
  function renderNamespace() {
    const namespace = currentNamespace();
    $('namespace-view').hidden = !namespace || state.view !== 'namespace';
    $('agents-view').hidden = state.view !== 'agents';
    $('runtime-view').hidden = state.view !== 'runtime';
    $('browse-view').hidden = !['handoffs', 'executions'].includes(state.view);
    $('empty-view').hidden = !!namespace || state.view !== 'namespace';
    if (!namespace) return;
    $('namespace-title').textContent = namespace.name;
    $('namespace-description').textContent = namespace.description || '暂无描述';
    $('namespace-id').textContent = namespace.id;
    $('namespace-status').textContent = namespace.status === 'ARCHIVED' ? '已归档' : '使用中';
    $('namespace-status').className = `badge${namespace.status === 'ARCHIVED' ? ' archived' : ''}`;
    $('counts').replaceChildren();
    Object.entries(kinds).forEach(([kind, label]) => {
      const card = element('div', undefined, 'count');
      card.append(element('strong', namespace.counts?.[kind] ?? 0), element('span', label));
      $('counts').append(card);
    });
    renderMembers(namespace);
    renderAudit(namespace);
    setTabVisibility();
  }
  function agentEditButton(agent) {
    const node = button('编辑权限', () => openAgent(agent.id), 'text-link');
    node.dataset.editAgent = agent.id;
    return node;
  }
  function renderMembers(namespace) {
    const body = $('members-body'); body.replaceChildren();
    for (const member of list(namespace.members)) {
      const row = element('tr');
      const identity = element('td'); identity.append(element('span', member.name || member.id), element('span', member.id, 'hint mono'));
      const action = element('td');
      const agent = agents().find((a) => a.id === member.id);
      action.append(agent ? agentEditButton(agent) : element('span', '由部署配置管理', 'hint'));
      row.append(identity, element('td', member.read ? '允许' : '—'), element('td', member.write ? '允许' : '—'), action);
      body.append(row);
    }
    if (!body.children.length) emptyRow(body, 4, '该空间暂无授权成员，请在 Agent 管理中配置。');
  }
  function renderAudit(namespace) {
    const wrap = element('div', undefined, 'table-wrap');
    const table = element('table'); table.append(element('caption', '最近管理操作记录', 'sr-only'));
    const head = element('thead'); const header = element('tr');
    ['时间', '操作', '操作者', '目标'].forEach((label) => { const cell = element('th', label); cell.scope = 'col'; header.append(cell); });
    head.append(header); table.append(head);
    const body = element('tbody');
    const relevant = (record) => typeof record.target === 'string' && (record.target === namespace.id || record.target === `namespace:${namespace.id}` || record.target === `namespaces/${namespace.id}`);
    const records = list(state.overview?.audit).slice().sort((a, b) => Number(relevant(b)) - Number(relevant(a)));
    records.forEach((record) => {
      const row = element('tr');
      ['time', 'action', 'actor', 'target'].forEach((key) => row.append(element('td', typeof record[key] === 'object' ? JSON.stringify(record[key]) : record[key] || '—')));
      body.append(row);
    });
    if (!records.length) emptyRow(body, 4, '暂无操作记录');
    table.append(body); wrap.append(table); $('audit-table').replaceChildren(wrap);
  }
  function renderAgents() {
    $('agent-count').textContent = `${agents().length} 个已注册 Agent`;
    const grid = $('agents-list'); grid.replaceChildren();
    for (const agent of agents()) {
      const card = element('article', undefined, 'agent-card');
      card.append(element('h2', agent.name || agent.id), element('code', agent.id));
      const details = element('dl');
      [['读取空间', names(agent.read_namespaces)], ['写入空间', names(agent.write_namespaces)], ['默认读取', names(agent.default_read_namespaces)], ['默认写入', agent.default_write_namespace ? names([agent.default_write_namespace]) : '未设置']].forEach(([label, value]) => {
        details.append(element('dt', label), element('dd', value));
      });
      card.append(details, agentEditButton(agent)); grid.append(card);
    }
    if (!agents().length) grid.append(element('p', '尚无凭据绑定的 Agent，请通过部署配置注册。', 'empty-state'));
    $('limits-text').textContent = JSON.stringify(state.overview?.limits ?? {}, null, 2);
  }
  async function loadOverview() {
    const epoch = state.epoch;
    const sequence = ++state.overviewSequence;
    connection('正在同步');
    try {
      const data = await request('/admin/api/overview', { group: 'overview' });
      if (epoch !== state.epoch || sequence !== state.overviewSequence) return false;
      if (!Array.isArray(data.namespaces) || !Array.isArray(data.agents)) throw new Error('无效响应');
      state.overview = data; state.fresh = true;
      if (!currentNamespace()) state.namespace = namespaces()[0]?.id ?? null;
      renderNavigation(); renderNamespace(); renderAgents(); populateSpaces(); updateControls();
      connection('已连接', 'online');
      return true;
    } catch (error) {
      if (epoch !== state.epoch || sequence !== state.overviewSequence) return false;
      state.fresh = false; updateControls(); connection('连接异常', 'offline');
      notice(errorText(error), true);
      return false;
    }
  }
  function setTabVisibility() {
    ['content', 'members', 'audit'].forEach((tab) => {
      $(`panel-${tab}`).hidden = state.tab !== tab;
      $(`tab-${tab}`).setAttribute('aria-selected', String(state.tab === tab));
      $(`tab-${tab}`).tabIndex = state.tab === tab ? 0 : -1;
    });
  }
  function clearDocuments() {
    cancelRead('documents');
    state.documentSequence++; state.previewSequence++; state.documentsBusy = false;
    $('documents-body').replaceChildren();
    $('documents-state').textContent = ''; $('page-label').textContent = '';
    $('previous-page').disabled = true; $('next-page').disabled = true;
    $('documents-body').setAttribute('aria-busy', 'false');
    clearPreview();
  }
  function clearPreview() {
    cancelRead('preview');
    state.previewSequence++;
    $('execution-detail').replaceChildren(); $('execution-detail').hidden = true;
    $('preview-raw').open = false;
    if ($('preview-dialog').open) $('preview-dialog').close();
    ['preview-title', 'preview-path', 'preview-sha', 'preview-content', 'preview-note', 'preview-state'].forEach((id) => { $(id).textContent = ''; });
  }
  async function selectNamespace(id) {
    if (!closeEditor()) return;
    rememberBrowse(); clearViewReads();
    state.namespace = id; persistBrowse(); state.view = 'namespace'; state.offset = 0;
    clearDocuments(); renderNavigation(); renderNamespace(); updateControls();
    if (state.tab === 'content') await loadDocuments();
  }
  async function loadDocuments() {
    if (!state.token || !currentNamespace()) return;
    const epoch = state.epoch; const sequence = ++state.documentSequence;
    state.documentsBusy = true;
    $('documents-body').setAttribute('aria-busy', 'true');
    $('documents-body').replaceChildren(); $('documents-state').textContent = '正在加载文档…';
    $('page-label').textContent = ''; $('previous-page').disabled = true; $('next-page').disabled = true;
    const query = new URLSearchParams({ namespace: state.namespace, limit: '20', offset: String(state.offset) });
    if (state.kind) query.set('kind', state.kind);
    if (state.project) query.set('project', state.project);
    if (state.query) query.set('q', state.query);
    if (state.status) query.set('status', state.status);
    query.set('sort', state.sort);
    try {
      const data = await request(`/admin/api/documents?${query}`, { group: 'documents' });
      if (epoch !== state.epoch || sequence !== state.documentSequence) return;
      if (!Array.isArray(data.documents ?? data.items)) throw new Error('无效响应');
      const rows = list(data.items ?? data.documents).slice(0, 20);
      state.total = Number.isFinite(data.total) ? Math.max(0, data.total) : rows.length;
      state.remaining = Number.isFinite(data.remaining) ? Math.max(0, data.remaining) : Math.max(0, state.total - state.offset - rows.length);
      for (const item of rows) {
        const field = (key) => item[key] ?? item.metadata?.[key] ?? '';
        const row = element('tr'); const identity = element('td');
        identity.append(button(field('title') || item.path, () => preview(item.path), 'text-link'), element('span', item.path, 'hint mono'));
        const owner = element('td'); owner.append(element('span', field('project') || field('project_id') || '—'), element('span', statusLabel(field('status')), 'hint'));
        row.append(identity, element('td', kinds[field('kind')] || field('kind') || '—'), owner, timeCell(field('updated_at')));
        $('documents-body').append(row);
      }
      if (!rows.length) emptyRow($('documents-body'), 4, state.offset ? '本页已无文档，请返回上一页。' : '暂无匹配文档，可清除筛选条件后查询。');
      connection('已连接', 'online');
      $('documents-state').textContent = '每页最多读取 20 条摘要，点击文档查看原文。';
      $('page-label').textContent = rows.length ? `${state.offset + 1}–${state.offset + rows.length} / ${state.total} 条` : `共 ${state.total} 条`;
      $('previous-page').disabled = state.offset === 0;
      $('next-page').disabled = state.remaining === 0 || rows.length === 0;
    } catch (error) {
      if (epoch !== state.epoch || sequence !== state.documentSequence) return;
      $('documents-body').replaceChildren();
      $('documents-state').textContent = errorText(error);
      emptyRow($('documents-body'), 4, '文档加载失败，可点击查询或刷新重试。');
      $('previous-page').disabled = state.offset === 0;
      if (!error.status) connection('连接异常', 'offline');
    } finally {
      if (epoch === state.epoch && sequence === state.documentSequence) {
        state.documentsBusy = false; $('documents-body').setAttribute('aria-busy', 'false');
      }
    }
  }
  async function preview(path, namespace = state.namespace, execution = false) {
    clearPreview();
    const epoch = state.epoch; const sequence = ++state.previewSequence;
    $('preview-title').textContent = execution ? '执行任务详情' : '文档预览'; $('preview-path').textContent = `${namespace} · ${path}`;
    $('preview-note').textContent = execution ? '只读任务与最近 20 次尝试；不执行或修改任务。' : '只读 Markdown 原文';
    $('preview-raw').classList.toggle('document-raw', !execution);
    $('preview-raw').open = !execution;
    $('preview-state').textContent = '正在读取…';
    if (!$('preview-dialog').open) $('preview-dialog').showModal();
    const query = new URLSearchParams({ namespace, [execution ? 'task_id' : 'path']: path });
    try {
      const data = await request(`/admin/api/${execution ? 'execution' : 'document'}?${query}`, { group: 'preview' });
      if (epoch !== state.epoch || sequence !== state.previewSequence) return;
      $('preview-sha').textContent = execution ? '' : `SHA-256 · ${data.sha256 || '—'}`;
      $('preview-state').textContent = '';
      $('preview-content').textContent = execution ? JSON.stringify({ task: data.task, attempts: data.attempts }, null, 2) : data.content ?? '';
      if (execution) renderExecution(data);
    } catch (error) {
      if (epoch === state.epoch && sequence === state.previewSequence) $('preview-state').textContent = errorText(error);
    }
  }
  function readable(value) {
    if (value === null || value === undefined || value === '') return '未提供';
    return typeof value === 'object' ? JSON.stringify(value) : String(value);
  }
  function detailFields(entries) {
    const details = element('dl', undefined, 'execution-fields');
    entries.forEach(([label, value]) => details.append(element('dt', label), element('dd', readable(value))));
    return details;
  }
  function checkpointView(checkpoint) {
    const section = element('section', undefined, 'checkpoint');
    section.append(element('h3', '检查点'), detailFields([['当前进度', checkpoint?.current]]));
    for (const [key, label] of [['completed_steps', '已完成'], ['next_steps', '下一步'], ['blockers', '阻塞'], ['verification', '验证'], ['artifacts', '产物']]) {
      section.append(element('h3', label));
      const values = list(checkpoint?.[key]);
      if (!values.length) section.append(element('p', '未提供', 'hint'));
      else { const items = element('ul'); values.forEach((value) => items.append(element('li', readable(value)))); section.append(items); }
    }
    return section;
  }
  function renderExecution(data) {
    const envelope = data.task || {}; const task = envelope.task || envelope;
    const attempts = list(data.attempts?.attempts).slice(0, 20).map((entry) => entry.attempt || entry);
    const latest = attempts.find((attempt) => attempt.attempt_id === task.current_attempt_id) || envelope.latest_attempt || data.latest_attempt;
    const content = $('execution-detail'); content.replaceChildren(); content.hidden = false;
    content.append(element('h2', task.title || task.task_id || '执行任务'), detailFields([
      ['状态', statusLabel(task.status)], ['目标', task.goal], ['验收条件', task.acceptance],
      ['当前尝试', task.current_attempt_id], ['回复状态', statusLabel(task.reply_status ?? task.delivery_status)],
      ['创建时间', dateLabel(task.created_at)], ['更新时间', dateLabel(task.updated_at)],
    ]), element('h2', '当前进展'), checkpointView(latest?.checkpoint || task.checkpoint));
    content.append(element('h2', '尝试时间线（最近 20 次）'));
    const timeline = element('ol', undefined, 'attempt-timeline');
    attempts.forEach((attempt) => {
      const item = element('li');
      item.append(element('h3', `${attempt.sequence !== undefined ? `第 ${attempt.sequence} 次尝试` : '执行尝试'} · ${statusLabel(attempt.status)}`), detailFields([
        ['尝试 ID', attempt.attempt_id], ['执行器', attempt.agent], ['原生会话', attempt.native_session_id],
        ['创建时间', dateLabel(attempt.created_at)], ['开始时间', dateLabel(attempt.started_at)],
        ['更新时间', dateLabel(attempt.updated_at)], ['结束时间', dateLabel(attempt.finished_at ?? attempt.completed_at)],
      ]));
      const checkpoint = element('details'); checkpoint.append(element('summary', '查看尝试检查点'), checkpointView(attempt.checkpoint));
      item.append(checkpoint); timeline.append(item);
    });
    if (!attempts.length) content.append(element('p', '暂无尝试记录', 'hint'));
    else content.append(timeline);
    const remaining = data.attempts?.remaining;
    content.append(element('p', `已展示 ${attempts.length} 次尝试${Number.isFinite(remaining) ? `，剩余 ${number(remaining)} 次` : ''}。服务当前只返回最近 20 次；更早记录未在此加载。`, 'hint'));
  }
  function statusLabel(value) { return statuses[value] || value || '—'; }
  function number(value, unit = '') {
    return typeof value === 'number' && Number.isFinite(value) ? `${value.toLocaleString('zh-CN', { maximumFractionDigits: 2 })}${unit}` : '未观测';
  }
  function dateLabel(value) {
    const date = new Date(value);
    return value && !Number.isNaN(date.getTime()) ? date.toLocaleString('zh-CN', { hour12: false }) : value || '—';
  }
  function timeCell(value) {
    const cell = element('td', dateLabel(value), 'time-cell');
    if (value) cell.title = value;
    return cell;
  }
  function setStatuses(select, values) {
    select.replaceChildren();
    const all = element('option', '全部状态'); all.value = ''; select.append(all);
    values.forEach((value) => { const option = element('option', statusLabel(value)); option.value = value; select.append(option); });
  }
  function populateSpaces() {
    for (const id of ['runtime-namespace', 'browse-namespace']) {
      const select = $(id); const previous = select.value;
      select.replaceChildren();
      if (id === 'runtime-namespace') { const all = element('option', '全部空间（全局）'); all.value = ''; select.append(all); }
      namespaces().forEach((space) => {
        const option = element('option', `${space.name} · ${space.id}${space.status === 'ARCHIVED' ? '（已归档）' : ''}`);
        option.value = space.id; select.append(option);
      });
      select.value = [...select.options].some((option) => option.value === previous) ? previous : (id === 'runtime-namespace' ? '' : state.namespace || '');
    }
  }
  function clearRuntime() {
    state.runtimeSequence++; cancelRead('runtime'); cancelRead('routing');
    state.storageSequence++; cancelRead('storage');
    if ($('read-storage').disabled) $('storage-state').textContent = state.storageReport ? `读取已取消，保留上次容量：${sampleLabel(state.storageReport)}` : '读取已取消，可重新读取磁盘容量。';
    $('read-storage').disabled = false;
    ['runtime-content', 'storage-content', 'routing-content'].forEach((id) => $(id).setAttribute('aria-busy', 'false'));
  }
  function clearBrowse() {
    state.browseSequence++; cancelRead('browse'); state.browseBusy = false; state.browseRemaining = 0;
    $('browse-body').replaceChildren(); $('browse-body').setAttribute('aria-busy', 'false');
    $('browse-state').textContent = ''; $('browse-page-label').textContent = '';
    $('browse-previous').disabled = true; $('browse-next').disabled = true;
  }
  function clearViewReads() { clearRuntime(); clearBrowse(); clearDocuments(); }
  function configureBrowse() {
    const execution = state.view === 'executions';
    $('browse-title').textContent = execution ? '执行任务' : '最近交接';
    $('browse-caption').textContent = $('browse-title').textContent;
    $('browse-eyebrow').textContent = execution ? 'EXECUTIONS' : 'HANDOFFS';
    $('browse-description').textContent = execution ? '按空间浏览任务摘要与执行尝试，仅提供只读查看。按最近更新时间排序。' : '浏览最近更新的会话与工作流，点击标题查看交接原文。按最近更新时间排序。';
    $('browse-type-heading').textContent = execution ? '回复状态' : '类型';
    $('browse-kind-field').hidden = execution;
    $('browse-query').placeholder = execution ? '搜索任务' : '标题或路径';
    populateSpaces();
    setStatuses($('browse-status'), execution ? ['QUEUED', 'DISPATCHING', 'RUNNING', 'WAITING_USER', 'RECONCILING', 'SUCCEEDED', 'FAILED', 'CANCELLED'] : documentStatuses);
    const saved = state.browsePages[state.view] || { namespace: state.namespace || '', kind: 'workstream', q: '', project: '', status: '', offset: 0 };
    state.browseFilters = { ...saved }; state.browseOffset = saved.offset;
    $('browse-namespace').value = saved.namespace;
    $('browse-kind').value = saved.kind; $('browse-query').value = saved.q;
    $('browse-project').value = saved.project; $('browse-status').value = saved.status;
    if (saved.namespace) state.namespace = saved.namespace;
  }
  function rememberBrowse() {
    if (!['handoffs', 'executions'].includes(state.view) || !state.browseFilters) return;
    const previous = state.browseFilters;
    captureBrowseFilters();
    const changed = ['namespace', 'kind', 'q', 'project', 'status'].some((key) => previous[key] !== state.browseFilters[key]);
    if (changed) state.browseOffset = 0;
    state.browsePages[state.view] = { ...state.browseFilters, offset: state.browseOffset };
    persistBrowse();
  }
  const browseSessionKey = 'harness.console.browse.v1';
  function persistBrowse() {
    if (['handoffs', 'executions'].includes(state.view) && state.browseFilters) {
      state.browsePages[state.view] = { ...state.browseFilters, offset: state.browseOffset };
    }
    try {
      // Only list filters enter session storage. Credentials and reports stay in memory.
      window.sessionStorage.setItem(browseSessionKey, JSON.stringify({ namespace: state.namespace, pages: state.browsePages }));
    } catch { /* Disabled browser storage still permits in-page navigation. */ }
  }
  function restoreBrowse() {
    try {
      const saved = JSON.parse(window.sessionStorage.getItem(browseSessionKey) || 'null');
      if (!saved || typeof saved !== 'object') return;
      if (typeof saved.namespace === 'string') state.namespace = saved.namespace;
      for (const view of ['handoffs', 'executions']) {
        const page = saved.pages?.[view];
        if (!page || !['namespace', 'q', 'project', 'status', 'kind'].every((key) => typeof page[key] === 'string') || !Number.isSafeInteger(page.offset) || page.offset < 0 || page.offset % 20) continue;
        state.browsePages[view] = { namespace: page.namespace, q: page.q, project: page.project, status: page.status, kind: page.kind, offset: page.offset };
      }
    } catch { /* Corrupt session filters have no effect on authentication. */ }
  }
  function captureBrowseFilters() {
    state.browseFilters = {
      namespace: $('browse-namespace').value, kind: $('browse-kind').value,
      q: $('browse-query').value.trim(), project: $('browse-project').value.trim(), status: $('browse-status').value,
    };
    if (state.browseFilters.namespace) state.namespace = state.browseFilters.namespace;
  }
  async function selectView(view) {
    if (!closeEditor()) return;
    rememberBrowse(); clearViewReads(); state.view = view;
    if (view === 'handoffs' || view === 'executions') configureBrowse();
    renderNavigation(); renderNamespace(); updateControls();
    await loadCurrentView();
  }
  async function loadCurrentView() {
    if (state.view === 'runtime') await loadRuntime();
    else if (['handoffs', 'executions'].includes(state.view)) await loadBrowse();
    else if (state.view === 'namespace' && state.tab === 'content') await loadDocuments();
  }
  function metricCard(label, value, hint) {
    const card = element('article', undefined, 'metric-card');
    card.append(element('h3', label), element('strong', value));
    if (hint) card.append(element('p', hint, 'hint'));
    return card;
  }
  function simpleTable(caption, headers, rows) {
    const wrap = element('div', undefined, 'table-wrap'); wrap.tabIndex = 0;
    wrap.setAttribute('role', 'region'); wrap.setAttribute('aria-label', `${caption}，可横向滚动`);
    const table = element('table'); table.append(element('caption', caption, 'sr-only'));
    const head = element('thead'); const header = element('tr');
    headers.forEach((title) => { const cell = element('th', title); cell.scope = 'col'; header.append(cell); });
    head.append(header); table.append(head);
    const body = element('tbody');
    rows.forEach((values) => { const row = element('tr'); values.forEach((value) => row.append(element('td', value))); body.append(row); });
    if (!rows.length) emptyRow(body, headers.length, '当前范围暂无观测记录。');
    table.append(body); wrap.append(table); return wrap;
  }
  function renderRuntime(data) {
    const content = $('runtime-content'); content.replaceChildren(); const storage = data.storage || {}; const metrics = data.metrics || {};
    const scope = element('div', undefined, 'info-box');
    scope.append(element('strong', `净收益：${data.net_benefit || '尚未测量'}`), element('p', '调用量、耗时与字符量用于观察资源成本；字符数不等于 token，统计不代表收益或货币成本。'));
    scope.append(element('p', typeof data.metrics_scope === 'string' ? data.metrics_scope : JSON.stringify(data.metrics_scope ?? '采集边界尚未返回。'), 'scope-note'));
    const coverage = data.coverage || {};
    scope.append(element('p', `采集起点：${dateLabel(data.observed_since)}；全局采集健康：损坏事件 ${number(coverage.malformed_events)}，不可读文件 ${number(coverage.unreadable_files)}，本进程丢弃事件 ${number(coverage.dropped_events_this_process)}。`, 'scope-note'));
    if (metrics.observed_calls === 0) scope.append(element('p', '当前范围暂无采集数据；历史调用不会从旧日志补算。', 'hint'));
    content.append(scope);
    const summary = element('div', undefined, 'runtime-meta');
    const uptime = typeof data.uptime_seconds === 'number' ? `${number(Math.floor(data.uptime_seconds / 3600))} 小时 ${Math.floor(data.uptime_seconds % 3600 / 60)} 分钟` : '未观测';
    summary.append(element('span', sampleLabel(data)), element('span', `版本 ${data.version ?? '未知'}`), element('span', `已运行 ${uptime}`), element('span', `统计窗口 ${number(data.window_days, ' 天')}`));
    content.append(summary, element('h2', '调用与耗时'));
    const calls = element('div', undefined, 'metric-grid');
    [
      ['观测调用', number(metrics.observed_calls)], ['失败调用', number(metrics.failures)], ['重试调用', number(metrics.retries)], ['分组调用', number(metrics.grouped_calls)], ['执行组', number(metrics.execution_groups)],
      ['处理总耗时', number(metrics.handler_ms_total, ' ms')], ['处理耗时 P50', number(metrics.handler_ms_p50, ' ms')], ['处理耗时 P90', number(metrics.handler_ms_p90, ' ms')],
      ['返回字符', number(metrics.returned_characters), `字符观测覆盖 ${number(metrics.returned_character_observed_calls)} 次调用`],
      ['交接字符', number(metrics.handoff_characters), '返回的交接字符合计，含幂等重放；不代表新增写入量'],
    ].forEach((args) => calls.append(metricCard(...args)));
    content.append(calls, element('h2', '内存存储计数（全局）'));
    const resources = element('div', undefined, 'metric-grid');
    const bytes = (value) => typeof value === 'number' ? `${number(value / 1024 / 1024)} MiB` : '未观测';
    [
      ['文档数量', number(storage.documents), `上限 ${number(storage.document_limit)} 篇`],
      ['文档体积', bytes(storage.document_bytes), `上限 ${bytes(storage.document_bytes_limit)}`],
      ['幂等回执', number(storage.receipts)],
    ].forEach((args) => resources.append(metricCard(...args)));
    content.append(resources, element('p', `当前修订：${storage.revision ?? '未观测'}。${data.storage_scope || '存储容量为全局，空间筛选仅影响调用统计'}。存储不随统计窗口变化。`, 'hint revision-note'));
    const breakdowns = element('div', undefined, 'runtime-breakdowns');
    for (const [label, key, name] of [['命令分布', 'commands', '命令'], ['错误分布', 'error_codes', '错误码']]) {
      const section = element('section'); section.append(element('h2', label));
      const values = metrics[key] && typeof metrics[key] === 'object' ? Object.entries(metrics[key]) : [];
      section.append(simpleTable(label, [name, '次数'], values.sort((a, b) => (b[1] || 0) - (a[1] || 0)).map(([key, value]) => [key, number(value)])));
      breakdowns.append(section);
    }
    content.append(breakdowns, element('h2', '各命令耗时'));
    const commands = metrics.command_metrics || data.command_metrics || {};
    const pair = (values, key) => {
      const count = key === 'handler_ms' ? values?.observed_calls : values?.[`${key.replace(/_ms$/, '')}_observed_calls`];
      return `P50 ${number(values?.[`${key}_p50`], ' ms')} / P90 ${number(values?.[`${key}_p90`], ' ms')}（样本 ${number(count)}）`;
    };
    content.append(simpleTable('各命令耗时', ['命令', '调用', '处理 P50 / P90', '锁等待 P50 / P90', '操作 P50 / P90', '提交 P50 / P90'], Object.entries(commands).map(([command, values]) => [
      command, number(values?.observed_calls), pair(values, 'handler_ms'), pair(values, 'lock_wait_ms'), pair(values, 'operation_ms'), pair(values, 'commit_ms'),
    ])));
    content.append(element('p', '阶段分位数各自计算，不能相加。操作耗时含逻辑和事务准备，提交耗时仅覆盖实际提交尝试；幂等重放不产生提交样本。执行组不是完整用户任务数。', 'hint'));
    const versions = element('div', undefined, 'runtime-breakdowns');
    for (const [label, key] of [['协议摘要', 'protocols'], ['服务版本摘要', 'service_versions']]) {
      const section = element('section'); section.append(element('h2', label));
      const entries = Object.entries(metrics[key] || data[key] || {}).sort((a, b) => (b[1] || 0) - (a[1] || 0));
      section.append(simpleTable(label, ['标识', '调用'], entries.slice(0, 8).map(([name, value]) => [name, number(value)])));
      if (entries.length > 8) section.append(element('p', `另有 ${entries.length - 8} 项未展开`, 'hint'));
      versions.append(section);
    }
    content.append(versions, element('h2', '各空间观测'));
    content.append(simpleTable('各空间观测', ['空间', '调用', '失败', '重试', '处理总耗时', '返回字符'], list(data.namespaces).map((space) => {
      const m = space.metrics || {};
      return [`${space.name || space.id} · ${space.id}`, number(m.observed_calls), number(m.failures), number(m.retries), number(m.handler_ms_total, ' ms'), number(m.returned_characters)];
    })));
  }
  function sampleLabel(report) {
    const cache = report?.cache_hit;
    return `实际采样时间 ${dateLabel(report?.sample_at ?? report?.sampled_at)} · 缓存命中 ${typeof cache === 'boolean' ? (cache ? '是' : '否') : '未提供'} · 缓存年龄 ${number(report?.cache_age_seconds ?? report?.age_seconds, ' 秒')}`;
  }
  async function loadRuntime() {
    if (!state.token || state.view !== 'runtime') return;
    clearRuntime();
    const epoch = state.epoch; const sequence = state.runtimeSequence;
    const query = new URLSearchParams({ days: $('runtime-days').value });
    const namespace = $('runtime-namespace').value;
    if (namespace) query.set('namespace', namespace);
    const key = query.toString(); const cached = state.runtimeCache.get(key);
    const scope = `${namespace || '全部空间'} · ${$('runtime-days').value} 天`;
    const retained = cached ? { key, data: cached, scope } : state.runtimeDisplayed;
    if (retained) renderRuntime(retained.data); else $('runtime-content').replaceChildren();
    renderRoutingCached(namespace);
    $('runtime-state').textContent = `正在读取运行快照…${retained ? ` 当前展示上次快照（${retained.scope}）：${sampleLabel(retained.data)}` : ''}`;
    $('runtime-content').setAttribute('aria-busy', 'true');
    try {
      const data = await request(`/admin/api/runtime?${query}`, { group: 'runtime' });
      if (epoch !== state.epoch || sequence !== state.runtimeSequence) return;
      if (!data.metrics || !data.storage) throw new Error('无效响应');
      state.runtimeCache.set(key, data); state.runtimeDisplayed = { key, data, scope }; renderRuntime(data);
      $('runtime-state').textContent = `当前展示范围 ${scope} · ${sampleLabel(data)} · 仅手动刷新`;
      connection('已连接', 'online');
      if (Object.prototype.hasOwnProperty.call(data, 'routing_evaluation')) {
        showRouting(namespace, data.routing_evaluation);
      } else await loadRouting(namespace, epoch, sequence);
    } catch (error) {
      if (epoch !== state.epoch || sequence !== state.runtimeSequence) return;
      $('runtime-state').textContent = `${errorText(error)}${retained ? ` 保留上次快照（${retained.scope}），可能过期：${sampleLabel(retained.data)}` : ' 尚无成功快照。'}`;
      $('routing-state').textContent = state.routingCache.has(namespace) ? '运行快照读取失败，保留上次评测结果，可能过期。' : '运行快照读取失败，尚未取得评测结果。';
      if (!error.status) connection('连接异常', 'offline');
    } finally {
      if (epoch === state.epoch && sequence === state.runtimeSequence) $('runtime-content').setAttribute('aria-busy', 'false');
    }
  }
  function renderStorage(report) {
    const storage = report.storage || report;
    const content = $('storage-content'); content.replaceChildren();
    const resources = element('div', undefined, 'metric-grid');
    resources.append(metricCard('磁盘用量', number(typeof storage.disk_bytes === 'number' ? storage.disk_bytes / 1024 / 1024 : null, ' MiB')), metricCard('保留修订', number(storage.revisions)));
    content.append(resources, element('p', `${sampleLabel({ ...storage, ...report })}。${storage.disk_scope || ''}`, 'hint'));
  }
  async function loadStorage() {
    if (!state.token || state.view !== 'runtime' || $('read-storage').disabled) return;
    const epoch = state.epoch; const sequence = ++state.storageSequence;
    const cached = state.storageReport;
    $('read-storage').disabled = true; $('storage-content').setAttribute('aria-busy', 'true');
    $('storage-state').textContent = '正在读取磁盘容量…';
    try {
      const data = await request(`/admin/api/storage${cached ? '?fresh=true' : ''}`, { group: 'storage' });
      if (epoch !== state.epoch || sequence !== state.storageSequence) return;
      const storage = data.storage || data;
      if (!Object.prototype.hasOwnProperty.call(storage, 'disk_bytes') && !Object.prototype.hasOwnProperty.call(storage, 'revisions')) throw new Error('无效响应');
      state.storageReport = data; renderStorage(data);
      $('storage-state').textContent = sampleLabel({ ...storage, ...data });
      $('read-storage').textContent = '刷新磁盘容量';
    } catch (error) {
      if (epoch !== state.epoch || sequence !== state.storageSequence) return;
      $('storage-state').textContent = `${errorText(error)}${cached ? ` 保留上次容量，可能过期：${sampleLabel({ ...(cached.storage || {}), ...cached })}` : ''}`;
    } finally {
      if (epoch === state.epoch && sequence === state.storageSequence) {
        $('read-storage').disabled = false; $('storage-content').setAttribute('aria-busy', 'false');
      }
    }
  }
  function renderRoutingCached(namespace) {
    $('routing-content').replaceChildren();
    const cached = state.routingCache.get(namespace);
    $('routing-state').textContent = cached === undefined ? '等待读取最近评测结果…' : '保留上次读取的评测结果，等待刷新。';
    if (cached !== undefined) renderRouting(cached);
  }
  function showRouting(namespace, response) {
    // Runtime may embed the endpoint envelope or its latest report directly.
    const latest = response && Object.prototype.hasOwnProperty.call(response, 'latest') ? response.latest : response;
    state.routingCache.set(namespace, latest ?? null); renderRouting(latest);
    $('routing-state').textContent = '只读最近结果；不会触发评测或保存。';
  }
  function renderRouting(report) {
    const content = $('routing-content'); content.replaceChildren();
    if (!report) { content.append(element('p', '暂无评测', 'hint')); return; }
    if (report.status === 'unavailable') {
      const reasons = { report_corrupt: '报告文件损坏', report_unreadable: '报告文件不可读' };
      content.append(element('p', `评测结果不可读取：${reasons[report.reason] || report.reason || '原因未提供'}`, 'error-text'));
      return;
    }
    const summary = report.summary || {};
    content.append(detailFields([
      ['评测时间', dateLabel(report.evaluated_at)], ['套件版本', report.suite_version],
      ['引擎版本', report.engine_version], ['报告空间', report.namespace], ['读取空间', report.read_namespaces],
      ['套件指纹', report.suite_fingerprint],
      ['快照指纹', report.snapshot_fingerprint], ['评测范围', report.scope],
    ]));
    content.append(simpleTable('最近路由评测结果', ['指标', '结果'], [
      ['正例正确 / 已评测', `${number(summary.positive_correct)} / ${number(summary.positive_evaluated)}`],
      ['正例准确率', number(typeof summary.positive_accuracy === 'number' ? summary.positive_accuracy * 100 : null, '%')],
      ['漏选', number(summary.missed)], ['反例已评测', number(summary.negative_evaluated)],
      ['误选', number(summary.false_selections)], ['不可用', number(summary.unavailable)], ['总数', number(summary.total)],
    ]));
  }
  async function loadRouting(namespace, epoch, sequence) {
    $('routing-content').setAttribute('aria-busy', 'true');
    try {
      const query = new URLSearchParams(); if (namespace) query.set('namespace', namespace);
      const data = await request(`/admin/api/routing-evaluation${namespace ? `?${query}` : ''}`, { group: 'routing' });
      if (epoch !== state.epoch || sequence !== state.runtimeSequence) return;
      if (!Object.prototype.hasOwnProperty.call(data, 'latest')) throw new Error('无效响应');
      showRouting(namespace, data);
    } catch (error) {
      if (epoch !== state.epoch || sequence !== state.runtimeSequence) return;
      $('routing-state').textContent = `${errorText(error)}${state.routingCache.has(namespace) ? ' 保留上次结果，可能过期。' : ' 暂无可用评测快照。'}`;
    } finally {
      if (epoch === state.epoch && sequence === state.runtimeSequence) $('routing-content').setAttribute('aria-busy', 'false');
    }
  }
  async function loadBrowse() {
    if (!state.token || !['handoffs', 'executions'].includes(state.view)) return;
    clearBrowse(); clearPreview();
    const filter = state.browseFilters; const execution = state.view === 'executions';
    if (!filter?.namespace) {
      $('browse-state').textContent = '暂无可浏览的空间。创建空间后可在这里查看任务。'; return;
    }
    const epoch = state.epoch; const sequence = state.browseSequence;
    state.browseBusy = true; $('browse-body').setAttribute('aria-busy', 'true');
    $('browse-state').textContent = '正在加载…';
    const query = new URLSearchParams({ namespace: filter.namespace, limit: '20', offset: String(state.browseOffset) });
    for (const key of ['project', 'q', 'status']) if (filter[key]) query.set(key, filter[key]);
    if (!execution) { query.set('kind', filter.kind); query.set('sort', 'updated_desc'); }
    try {
      const data = await request(`/admin/api/${execution ? 'executions' : 'documents'}?${query}`, { group: 'browse' });
      if (epoch !== state.epoch || sequence !== state.browseSequence) return;
      const source = execution ? data.tasks : data.documents ?? data.items;
      if (!Array.isArray(source)) throw new Error('无效响应');
      const rows = source.slice(0, 20); const offset = state.browseOffset;
      const total = Number.isFinite(data.total) ? Math.max(0, data.total) : rows.length;
      state.browseRemaining = Number.isFinite(data.remaining) ? Math.max(0, data.remaining) : Math.max(0, total - offset - rows.length);
      rows.forEach((item) => {
        const field = (key) => item[key] ?? item.metadata?.[key] ?? '';
        const id = execution ? item.task_id : item.path;
        const identity = element('td');
        identity.append(button(field('title') || id, () => preview(id, filter.namespace, execution), 'text-link'), element('span', id, 'hint mono'));
        if (execution && item.goal) identity.append(element('span', item.goal, 'hint task-goal'));
        const owner = element('td'); owner.append(element('span', field('project') || field('project_id') || '—'), element('span', statusLabel(field('status')), 'hint'));
        const row = element('tr');
        row.append(identity, element('td', execution ? statusLabel(item.reply_status ?? item.delivery_status) : kinds[field('kind')] || kinds[filter.kind]), owner, timeCell(field('updated_at')));
        $('browse-body').append(row);
      });
      if (!rows.length) emptyRow($('browse-body'), 4, offset ? '本页已无记录，请返回上一页。' : '暂无匹配记录，可切换空间或清除筛选后查询。');
      $('browse-page-label').textContent = rows.length ? `${offset + 1}–${offset + rows.length} / ${total} 条` : `共 ${total} 条`;
      $('browse-previous').disabled = offset === 0; $('browse-next').disabled = !state.browseRemaining || !rows.length;
      $('browse-state').textContent = `最近更新优先 · 每页最多 20 条 · ${execution ? '点击标题查看任务进展与尝试时间线' : '点击标题查看交接原文'}`;
      connection('已连接', 'online');
    } catch (error) {
      if (epoch !== state.epoch || sequence !== state.browseSequence) return;
      $('browse-body').replaceChildren(); emptyRow($('browse-body'), 4, '加载失败，请重新查询或刷新。');
      $('browse-state').textContent = errorText(error); $('browse-previous').disabled = state.browseOffset === 0;
      if (!error.status) connection('连接异常', 'offline');
    } finally {
      if (epoch === state.epoch && sequence === state.browseSequence) { state.browseBusy = false; $('browse-body').setAttribute('aria-busy', 'false'); }
    }
  }
  function openNamespace(create) {
    if (!state.fresh || state.pending || state.mutationBusy || !closeEditor()) return;
    const namespace = create ? null : currentNamespace();
    if (!create && !namespace) return;
    state.editor = { type: 'namespace', id: namespace?.id ?? null, sha256: namespace?.sha256 };
    $('namespace-dialog-title').textContent = create ? '新建空间' : '编辑空间';
    $('namespace-form-note').textContent = create ? '填写名称与描述，空间 ID 由服务自动生成。' : `空间 ID：${namespace.id}`;
    $('namespace-name').value = namespace?.name ?? '';
    $('namespace-description-input').value = namespace?.description ?? '';
    $('namespace-status-field').hidden = create;
    $('namespace-status-input').value = namespace?.status ?? 'ACTIVE';
    $('namespace-status-input').dataset.locked = String(namespace?.id === 'shared');
    $('namespace-save').textContent = create ? '创建空间' : '保存修改';
    updateControls(); $('namespace-dialog').showModal(); $('namespace-name').focus();
  }
  function openAgent(id) {
    if (!state.fresh || state.pending || state.mutationBusy || !closeEditor()) return;
    const agent = agents().find((a) => a.id === id);
    if (!agent) return;
    state.editor = { type: 'agent', id, sha256: agent.sha256 };
    $('agent-dialog-title').textContent = '编辑 Agent 权限';
    $('agent-identity').textContent = `${agent.id} · 凭据由部署配置注册`;
    $('agent-name').value = agent.name || agent.id;
    const grants = $('agent-grants'); grants.replaceChildren();
    const referenced = [...list(agent.read_namespaces), ...list(agent.write_namespaces), ...list(agent.default_read_namespaces), ...(agent.default_write_namespace ? [agent.default_write_namespace] : [])];
    const spaces = [...namespaces()];
    new Set(referenced).forEach((namespaceId) => {
      if (namespaceId !== '*' && !spaces.some((n) => n.id === namespaceId)) spaces.push({ id: namespaceId, name: `${namespaceId}（未返回的空间）` });
    });
    for (const namespace of spaces) {
      if (namespace.id === '*') continue;
      const row = element('tr'); row.dataset.namespace = namespace.id;
      const identity = element('td'); identity.append(element('span', namespace.name), element('span', `${namespace.id}${namespace.status === 'ARCHIVED' ? ' · 已归档' : ''}`, 'hint mono'));
      row.append(identity);
      for (const [role, label, ids] of [['read', '读取', agent.read_namespaces], ['write', '写入', agent.write_namespaces], ['defaultRead', '默认读取', agent.default_read_namespaces]]) {
        const cell = element('td'); const control = element('input');
        control.type = 'checkbox'; control.dataset.role = role; control.checked = list(ids).includes(namespace.id);
        control.setAttribute('aria-label', `${namespace.name}：${label}`);
        const labelNode = element('label'); labelNode.append(control); cell.append(labelNode); row.append(cell);
        control.addEventListener('change', () => {
          if (role === 'read' && !control.checked) { row.querySelector('[data-role="defaultRead"]').checked = false; row.querySelector('[data-role="write"]').checked = false; rebuildDefaultWrite(); }
          if (role === 'defaultRead' && control.checked) row.querySelector('[data-role="read"]').checked = true;
          if (role === 'write') { if (control.checked) row.querySelector('[data-role="read"]').checked = true; rebuildDefaultWrite(); }
        });
      }
      grants.append(row);
    }
    rebuildDefaultWrite(agent.default_write_namespace);
    updateControls(); $('agent-dialog').showModal(); $('agent-name').focus();
  }
  function rebuildDefaultWrite(initial) {
    const select = $('agent-default-write'); const previous = initial === undefined ? select.value : initial;
    select.replaceChildren();
    const none = element('option', '未设置'); none.value = ''; select.append(none);
    $('agent-grants').querySelectorAll('tr').forEach((row) => {
      if (!row.querySelector('[data-role="write"]').checked) return;
      const id = row.dataset.namespace;
      const option = element('option', namespaces().find((n) => n.id === id)?.name || id); option.value = id; select.append(option);
    });
    select.value = [...select.options].some((option) => option.value === previous) ? previous : '';
  }
  async function submitMutation(method, path, payload, label) {
    if (state.pending || state.mutationBusy || !state.fresh) return;
    if (typeof crypto.randomUUID !== 'function') {
      const target = state.editor?.type === 'agent' ? $('agent-form-error') : $('namespace-form-error');
      target.textContent = '浏览器不支持安全 UUID，请使用 HTTPS 或 localhost 上的现代浏览器。';
      return;
    }
    state.pending = { method, path, body: JSON.stringify(payload), key: crypto.randomUUID(), label };
    await sendPending();
  }
  async function sendPending() {
    if (!state.pending || state.mutationBusy) return;
    const operation = state.pending; const epoch = state.epoch;
    state.mutationBusy = true; notice('正在提交…'); updateControls();
    const formError = state.editor?.type === 'agent' ? $('agent-form-error') : $('namespace-form-error');
    formError.textContent = '';
    try {
      await request(operation.path, operation);
      if (epoch !== state.epoch) return;
      state.pending = null; state.fresh = false;
      closeEditor(true); notice('已保存，正在刷新最新数据…');
      const refreshed = await loadOverview();
      if (epoch !== state.epoch) return;
      notice(refreshed ? `${operation.label}已完成。` : '写入已成功，但最新数据读取失败。请刷新后再编辑；无需重复提交。', !refreshed);
      if (refreshed) await loadCurrentView();
    } catch (error) {
      if (epoch !== state.epoch) return;
      if (error.status === 409) {
        state.pending = null; state.fresh = false;
        closeEditor(true); clearViewReads();
        await loadOverview();
        if (epoch !== state.epoch) return;
        notice(state.fresh ? '版本冲突：已加载最新数据，旧表单已关闭。请重新打开编辑，审阅后再保存。' : '版本冲突：旧表单已关闭，最新数据尚未加载。请刷新后重新审阅并编辑。', true);
        if (state.fresh) await loadCurrentView();
      } else if (error.status && error.status < 500) {
        state.pending = null;
        formError.textContent = errorText(error); notice(errorText(error), true);
      } else {
        state.fresh = false; connection('结果未确认', 'offline');
        closeEditor(true);
        notice('请求结果未知，原始请求与幂等键已保留。请显式重试确认结果。', true);
      }
    } finally {
      if (epoch === state.epoch) { state.mutationBusy = false; updateControls(); }
    }
  }
  async function refresh() {
    if (state.mutationBusy || !closeEditor()) return;
    notice('');
    rememberBrowse(); clearViewReads();
    const refreshed = await loadOverview();
    if (refreshed) await loadCurrentView();
  }
  function logout(preserveBrowse = false) {
    if (!preserveBrowse) {
      try { window.sessionStorage.removeItem(browseSessionKey); } catch { /* Storage may be disabled. */ }
    }
    state.epoch++;
    controllers.forEach((controller) => controller.abort()); controllers.clear();
    readControllers.clear();
    state.token = ''; state.overview = null; state.namespace = null; state.pending = null;
    state.fresh = false; state.mutationBusy = false; state.view = 'runtime'; state.tab = 'content';
    state.offset = 0; state.kind = ''; state.project = ''; state.query = ''; state.status = ''; state.sort = 'path'; state.total = 0; state.remaining = 0;
    state.browseOffset = 0; state.browseFilters = null; state.browsePages = {};
    state.runtimeCache.clear(); state.runtimeDisplayed = null; state.routingCache.clear(); state.storageReport = null;
    ['runtime-content', 'storage-content', 'routing-content'].forEach((id) => $(id).replaceChildren());
    ['runtime-state', 'storage-state', 'routing-state'].forEach((id) => { $(id).textContent = ''; });
    $('read-storage').textContent = '读取磁盘容量';
    closeEditor(true); clearViewReads(); notice('');
    ['namespace-list', 'agents-list', 'members-body', 'audit-table', 'counts'].forEach((id) => $(id).replaceChildren());
    ['namespace-title', 'namespace-description', 'namespace-id', 'namespace-status', 'limits-text', 'agent-count', 'pending-description', 'namespace-form-note', 'login-error'].forEach((id) => { $(id).textContent = ''; });
    $('namespace-search').value = ''; $('filters').reset(); $('login-form').reset();
    $('runtime-filters').reset(); $('browse-filters').reset(); populateSpaces();
    renderNavigation(); renderNamespace();
    $('workspace').hidden = true; $('login-view').hidden = false;
    $('refresh').hidden = true; $('logout').hidden = true;
    $('login-submit').disabled = false; $('admin-token').disabled = false;
    connection('未连接'); updateControls(); $('admin-token').focus();
  }

  $('login-form').addEventListener('submit', async (event) => {
    event.preventDefault();
    if ($('login-submit').disabled) return;
    state.token = $('admin-token').value.trim(); $('admin-token').value = '';
    if (!state.token) { $('login-error').textContent = '请输入管理员令牌。'; return; }
    const epoch = state.epoch;
    $('login-submit').disabled = true; $('admin-token').disabled = true; $('login-error').textContent = '';
    const success = await loadOverview();
    if (epoch !== state.epoch) return;
    if (!success) {
      const message = $('notice').textContent;
      logout(); $('login-error').textContent = message; return;
    }
    $('login-view').hidden = true; $('workspace').hidden = false;
    $('refresh').hidden = false; $('logout').hidden = false; $('main').focus();
    await loadCurrentView();
  });
  $('logout').addEventListener('click', () => {
    if ((state.dirty || state.pending || state.mutationBusy) && !window.confirm('退出将清除未保存表单和待重试请求。已发送的写入仍可能完成，确定退出？')) return;
    logout();
  });
  $('refresh').addEventListener('click', refresh);
  $('namespace-search').addEventListener('input', renderNavigation);
  $('create-namespace').addEventListener('click', () => openNamespace(true));
  $('edit-namespace').addEventListener('click', () => openNamespace(false));
  $('global-agents').addEventListener('click', () => selectView('agents'));
  for (const view of ['runtime', 'handoffs', 'executions']) $(`nav-${view}`).addEventListener('click', () => selectView(view));
  $('read-storage').addEventListener('click', loadStorage);
  $('runtime-filters').addEventListener('submit', (event) => { event.preventDefault(); loadRuntime(); });
  ['runtime-namespace', 'runtime-days'].forEach((id) => $(id).addEventListener('change', () => $('runtime-filters').requestSubmit()));
  $('browse-filters').addEventListener('submit', (event) => {
    event.preventDefault(); captureBrowseFilters(); state.browseOffset = 0; persistBrowse(); loadBrowse();
  });
  ['browse-namespace', 'browse-kind', 'browse-status'].forEach((id) => $(id).addEventListener('change', () => $('browse-filters').requestSubmit()));
  $('browse-previous').addEventListener('click', () => {
    if (state.browseBusy || !state.browseOffset) return;
    state.browseOffset = Math.max(0, state.browseOffset - 20); persistBrowse(); loadBrowse();
  });
  $('browse-next').addEventListener('click', () => {
    if (state.browseBusy || !state.browseRemaining) return;
    state.browseOffset += 20; persistBrowse(); loadBrowse();
  });
  document.querySelectorAll('[data-tab]').forEach((node) => {
    const activate = () => {
      state.tab = node.dataset.tab; setTabVisibility();
      clearDocuments();
      if (state.tab === 'content') loadDocuments();
    };
    node.addEventListener('click', activate);
    node.addEventListener('keydown', (event) => {
      const tabs = [...document.querySelectorAll('[data-tab]')]; const index = tabs.indexOf(node);
      let next;
      if (event.key === 'ArrowRight') next = (index + 1) % tabs.length;
      if (event.key === 'ArrowLeft') next = (index + tabs.length - 1) % tabs.length;
      if (event.key === 'Home') next = 0;
      if (event.key === 'End') next = tabs.length - 1;
      if (next !== undefined) { event.preventDefault(); tabs[next].focus(); tabs[next].click(); }
    });
  });
  $('filters').addEventListener('submit', (event) => {
    event.preventDefault(); state.kind = $('kind-filter').value; state.project = $('project-filter').value.trim(); state.offset = 0;
    state.query = $('query-filter').value.trim(); state.status = $('status-filter').value; state.sort = $('sort-filter').value;
    clearPreview(); loadDocuments();
  });
  $('previous-page').addEventListener('click', () => {
    if (state.documentsBusy || !state.offset) return;
    state.offset = Math.max(0, state.offset - 20); clearPreview(); loadDocuments();
  });
  $('next-page').addEventListener('click', () => {
    if (state.documentsBusy || !state.remaining) return;
    state.offset += 20; clearPreview(); loadDocuments();
  });
  ['namespace-form', 'agent-form'].forEach((id) => {
    $(id).addEventListener('input', () => dirty());
    $(id).addEventListener('change', () => dirty());
  });
  $('namespace-form').addEventListener('submit', async (event) => {
    event.preventDefault();
    if (state.editor?.type !== 'namespace') return;
    const name = $('namespace-name').value.trim();
    if (!name) { $('namespace-form-error').textContent = '空间名称不能为空。'; return; }
    const payload = { name, description: $('namespace-description-input').value.trim() };
    const id = state.editor.id;
    if (id) {
      if (!state.editor.sha256) { $('namespace-form-error').textContent = '缺少版本信息，请刷新后重新编辑。'; return; }
      payload.status = id === 'shared' ? 'ACTIVE' : $('namespace-status-input').value;
      payload.expected_sha256 = state.editor.sha256;
    }
    await submitMutation(id ? 'PUT' : 'POST', id ? `/admin/api/namespaces/${encodeURIComponent(id)}` : '/admin/api/namespaces', payload, id ? '空间更新' : '空间创建');
  });
  $('agent-form').addEventListener('submit', async (event) => {
    event.preventDefault();
    if (state.editor?.type !== 'agent') return;
    const name = $('agent-name').value.trim();
    if (!name) { $('agent-form-error').textContent = 'Agent 名称不能为空。'; return; }
    if (!state.editor.sha256) { $('agent-form-error').textContent = '缺少版本信息，请刷新后重新编辑。'; return; }
    const selected = (role) => [...$('agent-grants').querySelectorAll('tr')].filter((row) => row.querySelector(`[data-role="${role}"]`).checked).map((row) => row.dataset.namespace);
    const payload = { name, read_namespaces: selected('read'), write_namespaces: selected('write'), default_read_namespaces: selected('defaultRead'), default_write_namespace: $('agent-default-write').value || null, expected_sha256: state.editor.sha256 };
    if (payload.write_namespaces.some((id) => !payload.read_namespaces.includes(id)) || (payload.read_namespaces.length && !payload.default_read_namespaces.length) || payload.read_namespaces.length > 16 || payload.default_read_namespaces.length > 8) { $('agent-form-error').textContent = '写入必须同时授权读取；有读取权限时至少选一个默认读取，最多授权 16 个空间、默认读取 8 个。'; return; }
    if (payload.default_read_namespaces.some((id) => !payload.read_namespaces.includes(id)) || (payload.default_write_namespace && !payload.write_namespaces.includes(payload.default_write_namespace))) {
      $('agent-form-error').textContent = '默认空间必须属于对应的授权范围。'; return;
    }
    await submitMutation('PUT', `/admin/api/agents/${encodeURIComponent(state.editor.id)}`, payload, 'Agent 权限更新');
  });
  document.querySelectorAll('[data-close]').forEach((node) => node.addEventListener('click', () => {
    if (node.dataset.close === 'preview-dialog') clearPreview(); else closeEditor();
  }));
  ['namespace-dialog', 'agent-dialog'].forEach((id) => $(id).addEventListener('cancel', (event) => { event.preventDefault(); closeEditor(); }));
  $('preview-dialog').addEventListener('cancel', (event) => { event.preventDefault(); clearPreview(); });
  $('retry-mutation').addEventListener('click', sendPending);
  $('abandon-mutation').addEventListener('click', async () => {
    if (state.mutationBusy || !state.pending) return;
    if (!window.confirm('原请求可能已完成。放弃会清除重试信息，请先核对刷新后的数据，避免重复操作。确定放弃重试？')) return;
    state.pending = null; closeEditor(true); updateControls(); await refresh();
  });
  window.addEventListener('beforeunload', (event) => {
    rememberBrowse();
    if (state.dirty || state.pending || state.mutationBusy) { event.preventDefault(); event.returnValue = ''; }
  });
  window.addEventListener('offline', () => { if (state.token) { connection('网络已断开', 'offline'); notice('网络已断开。当前数据可能过期，恢复连接后请手动刷新。', true); } });
  window.addEventListener('online', () => { if (state.token) { connection('网络已恢复，待刷新'); notice('网络已恢复，请点击刷新获取最新数据。'); } });
  setStatuses($('status-filter'), documentStatuses);
  logout(true);
  restoreBrowse();
})();
