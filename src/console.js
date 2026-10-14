/* Standalone admin UI. Authentication and unresolved writes live only in this page. */
(() => {
  'use strict';
  const $ = (id) => document.getElementById(id);
  const kinds = { project: '项目', project_topic: '专题', process: '流程', session: 'Session', workstream: 'Workstream', execution_task: '执行任务', execution_attempt: '执行尝试', execution_context: '执行上下文', log: '日志' };
  const state = { token: '', epoch: 0, overview: null, namespace: null, view: 'namespace', tab: 'content', offset: 0, kind: '', project: '', total: 0, remaining: 0, documentsBusy: false, documentSequence: 0, previewSequence: 0, overviewSequence: 0, pending: null, mutationBusy: false, fresh: false, editor: null, dirty: false };
  const controllers = new Set();

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
  async function request(path, options = {}) {
    const controller = new AbortController();
    const epoch = state.epoch;
    controllers.add(controller);
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
      return await response.json();
    } finally {
      clearTimeout(timeout);
      controllers.delete(controller);
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
    $('global-agents').classList.toggle('active', state.view === 'agents');
    if (state.view === 'agents') $('global-agents').setAttribute('aria-current', 'page');
    else $('global-agents').removeAttribute('aria-current');
  }
  function renderNamespace() {
    const namespace = currentNamespace();
    $('namespace-view').hidden = !namespace || state.view !== 'namespace';
    $('agents-view').hidden = state.view !== 'agents';
    $('empty-view').hidden = !!namespace || state.view === 'agents';
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
      const data = await request('/admin/api/overview');
      if (epoch !== state.epoch || sequence !== state.overviewSequence) return false;
      if (!Array.isArray(data.namespaces) || !Array.isArray(data.agents)) throw new Error('无效响应');
      state.overview = data; state.fresh = true;
      if (!currentNamespace()) state.namespace = namespaces()[0]?.id ?? null;
      renderNavigation(); renderNamespace(); renderAgents(); updateControls();
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
    state.documentSequence++; state.previewSequence++; state.documentsBusy = false;
    $('documents-body').replaceChildren();
    $('documents-state').textContent = ''; $('page-label').textContent = '';
    $('previous-page').disabled = true; $('next-page').disabled = true;
    clearPreview();
  }
  function clearPreview() {
    state.previewSequence++;
    if ($('preview-dialog').open) $('preview-dialog').close();
    ['preview-title', 'preview-path', 'preview-sha', 'preview-content'].forEach((id) => { $(id).textContent = ''; });
  }
  async function selectNamespace(id) {
    if (!closeEditor()) return;
    state.namespace = id; state.view = 'namespace'; state.offset = 0;
    clearDocuments(); renderNavigation(); renderNamespace(); updateControls();
    if (state.tab === 'content') await loadDocuments();
  }
  async function loadDocuments() {
    if (!state.token || !currentNamespace()) return;
    const epoch = state.epoch; const sequence = ++state.documentSequence;
    state.documentsBusy = true;
    $('documents-body').replaceChildren(); $('documents-state').textContent = '正在加载文档…';
    $('page-label').textContent = ''; $('previous-page').disabled = true; $('next-page').disabled = true;
    const query = new URLSearchParams({ namespace: state.namespace, limit: '20', offset: String(state.offset) });
    if (state.kind) query.set('kind', state.kind);
    if (state.project) query.set('project', state.project);
    try {
      const data = await request(`/admin/api/documents?${query}`);
      if (epoch !== state.epoch || sequence !== state.documentSequence) return;
      const rows = list(data.items ?? data.documents).slice(0, 20);
      state.total = Number.isFinite(data.total) ? Math.max(0, data.total) : rows.length;
      state.remaining = Number.isFinite(data.remaining) ? Math.max(0, data.remaining) : Math.max(0, state.total - state.offset - rows.length);
      for (const item of rows) {
        const field = (key) => item[key] ?? item.metadata?.[key] ?? '';
        const row = element('tr'); const identity = element('td');
        identity.append(button(field('title') || item.path, () => preview(item.path), 'text-link'), element('span', item.path, 'hint mono'));
        const owner = element('td'); owner.append(element('span', field('project') || field('project_id') || '—'), element('span', field('status'), 'hint'));
        row.append(identity, element('td', kinds[field('kind')] || field('kind') || '—'), owner, element('td', field('updated_at') || '—'));
        $('documents-body').append(row);
      }
      if (!rows.length) emptyRow($('documents-body'), 4, state.offset ? '本页已无文档，请返回上一页。' : '暂无匹配文档');
      $('documents-state').textContent = '每页最多读取 20 条摘要，点击文档查看原文。';
      $('page-label').textContent = rows.length ? `${state.offset + 1}–${state.offset + rows.length} / ${state.total} 条` : `共 ${state.total} 条`;
      $('previous-page').disabled = state.offset === 0;
      $('next-page').disabled = state.remaining === 0 || rows.length === 0;
    } catch (error) {
      if (epoch !== state.epoch || sequence !== state.documentSequence) return;
      $('documents-state').textContent = errorText(error);
      emptyRow($('documents-body'), 4, '文档加载失败，可点击查询或刷新重试。');
      $('previous-page').disabled = state.offset === 0;
      if (!error.status) connection('连接异常', 'offline');
    } finally {
      if (epoch === state.epoch && sequence === state.documentSequence) state.documentsBusy = false;
    }
  }
  async function preview(path) {
    const epoch = state.epoch; const sequence = ++state.previewSequence;
    $('preview-title').textContent = '文档预览'; $('preview-path').textContent = path;
    $('preview-sha').textContent = ''; $('preview-content').textContent = '正在读取…';
    if (!$('preview-dialog').open) $('preview-dialog').showModal();
    const query = new URLSearchParams({ namespace: state.namespace, path });
    try {
      const data = await request(`/admin/api/document?${query}`);
      if (epoch !== state.epoch || sequence !== state.previewSequence) return;
      $('preview-sha').textContent = `SHA-256 · ${data.sha256 || '—'}`;
      $('preview-content').textContent = data.content ?? '';
    } catch (error) {
      if (epoch === state.epoch && sequence === state.previewSequence) $('preview-content').textContent = errorText(error);
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
      if (refreshed && state.view === 'namespace' && state.tab === 'content') await loadDocuments();
    } catch (error) {
      if (epoch !== state.epoch) return;
      if (error.status === 409) {
        state.pending = null; state.fresh = false;
        closeEditor(true); clearDocuments();
        await loadOverview();
        if (epoch !== state.epoch) return;
        notice(state.fresh ? '版本冲突：已加载最新数据，旧表单已关闭。请重新打开编辑，审阅后再保存。' : '版本冲突：旧表单已关闭，最新数据尚未加载。请刷新后重新审阅并编辑。', true);
        if (state.fresh && state.view === 'namespace' && state.tab === 'content') await loadDocuments();
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
    clearDocuments();
    const refreshed = await loadOverview();
    if (refreshed && state.view === 'namespace' && state.tab === 'content') await loadDocuments();
  }
  function logout() {
    state.epoch++;
    controllers.forEach((controller) => controller.abort()); controllers.clear();
    state.token = ''; state.overview = null; state.namespace = null; state.pending = null;
    state.fresh = false; state.mutationBusy = false; state.view = 'namespace'; state.tab = 'content';
    state.offset = 0; state.kind = ''; state.project = ''; state.total = 0; state.remaining = 0;
    closeEditor(true); clearDocuments(); notice('');
    ['namespace-list', 'agents-list', 'members-body', 'audit-table', 'counts'].forEach((id) => $(id).replaceChildren());
    ['namespace-title', 'namespace-description', 'namespace-id', 'namespace-status', 'limits-text', 'agent-count', 'pending-description', 'namespace-form-note', 'login-error'].forEach((id) => { $(id).textContent = ''; });
    $('namespace-search').value = ''; $('filters').reset(); $('login-form').reset();
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
    if (currentNamespace()) await loadDocuments();
  });
  $('logout').addEventListener('click', () => {
    if ((state.dirty || state.pending || state.mutationBusy) && !window.confirm('退出将清除未保存表单和待重试请求。已发送的写入仍可能完成，确定退出？')) return;
    logout();
  });
  $('refresh').addEventListener('click', refresh);
  $('namespace-search').addEventListener('input', renderNavigation);
  $('create-namespace').addEventListener('click', () => openNamespace(true));
  $('edit-namespace').addEventListener('click', () => openNamespace(false));
  $('global-agents').addEventListener('click', () => {
    if (!closeEditor()) return;
    state.view = 'agents'; clearDocuments(); renderNavigation(); renderNamespace(); renderAgents(); updateControls();
  });
  document.querySelectorAll('[data-tab]').forEach((node) => {
    const activate = () => {
      state.tab = node.dataset.tab; setTabVisibility();
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
    if (state.dirty || state.pending || state.mutationBusy) { event.preventDefault(); event.returnValue = ''; }
  });
  window.addEventListener('offline', () => { if (state.token) connection('网络已断开', 'offline'); });
  logout();
})();
