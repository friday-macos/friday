'use strict';

const byId = (id) => document.getElementById(id);
const node = (tag, className, text) => {
  const item = document.createElement(tag);
  if (className) item.className = className;
  if (text !== undefined) item.textContent = text;
  return item;
};
const labels = { idle:'대기 중', wake_wait:'Mac에서 호출어 대기 중', working:'답변 준비 중', listening:'음성 입력 중', speaking:'읽어주는 중', recognizing:'음성 인식 중' };
const statuses = { running:'실행 중', approval:'승인 대기', stopping:'중단 중', done:'완료', failed:'실패', interrupted:'이전 실행에서 중단됨' };
const pairingKey = 'jarvis-secret';
const securePairing = location.protocol === 'https:';
let secret = '';
try { secret = sessionStorage.getItem(pairingKey) || ''; } catch {}
if (!secret && securePairing) {
  try { secret = localStorage.getItem(pairingKey) || ''; } catch {}
}
byId('remember-device').hidden = !securePairing;
let selected = secret ? 'chat' : 'connection';
let state = null;
let rendered = '';
let polling = false;
let pending = false;
let rememberOnConnect = null;

for (let i = 0; i < 31; i++) byId('levels').append(node('i'));

async function call(path, command) {
  const headers = { Authorization: 'Bearer ' + secret };
  if (command) headers['Content-Type'] = 'application/json';
  const response = await fetch('/api/' + path, {
    headers, method: command ? 'POST' : 'GET', cache: 'no-store',
    body: command ? JSON.stringify(command) : undefined
  });
  if (response.ok) return response;
  const detail = await response.json().catch(() => ({}));
  const failure = new Error(detail.error || 'Mac에 연결하지 못했어.');
  failure.status = response.status;
  throw failure;
}

function select(tab) {
  if (tab !== 'chat' && typeof stopWebVoice === 'function') stopWebVoice();
  selected = tab;
  rendered = '';
  draw();
}
for (const button of document.querySelectorAll('[data-tab]')) {
  button.addEventListener('click', () => select(button.dataset.tab));
}
for (const button of document.querySelectorAll('[data-go-tasks]')) {
  button.addEventListener('click', () => select('tasks'));
}

function say(text = '') { byId('notice').textContent = text; }
function remember(value, keep = false) {
  secret = value;
  rememberOnConnect = null;
  try {
    if (value) sessionStorage.setItem(pairingKey, value);
    else sessionStorage.removeItem(pairingKey);
  } catch {}
  if (securePairing) {
    try {
      if (value && keep) localStorage.setItem(pairingKey, value);
      else localStorage.removeItem(pairingKey);
    } catch {
      return !keep;
    }
  }
  return true;
}
byId('pair').addEventListener('submit', async (event) => {
  event.preventDefault();
  if (polling) return;
  const code = byId('code').value.trim();
  if (!code) return;
  rememberOnConnect = securePairing && byId('keep-connection').checked;
  secret = code;
  byId('code').value = '';
  selected = 'chat';
  draw();
  await refresh();
});
byId('forget').addEventListener('click', () => {
  remember('');
  state = null;
  select('connection');
  say();
});

async function refresh() {
  if (!secret || polling) return;
  polling = true;
  const credential = secret;
  try {
    const next = await (await call('state')).json();
    if (credential !== secret) return;
    state = next;
    byId('connection-status').textContent = '연결됨';
    document.querySelector('.host').dataset.offline = 'false';
    draw();
    if (rememberOnConnect !== null && !remember(credential, rememberOnConnect)) {
      say('이 브라우저가 연결 저장을 허용하지 않아. 현재 탭에서 연결돼 있어.');
    }
  } catch (error) {
    if (credential !== secret) return;
    if (error.status === 401) {
      remember('');
      state = null;
      select('connection');
    } else {
      byId('connection-status').textContent = '다시 연결 중';
      document.querySelector('.host').dataset.offline = 'true';
    }
    say(error.message);
  } finally { polling = false; }
}

function bubble(parent, name, text, answer = false) {
  const card = node('article', 'bubble' + (answer ? ' answer' : ''));
  card.append(node('b', '', name), node('div', '', text));
  parent.append(card);
  return card;
}

function jobButton(label, action) {
  const button = node('button', '', label);
  button.type = 'button';
  button.addEventListener('click', action);
  return button;
}

async function send(command, feedback = say) {
  if (pending) {
    feedback('이전 요청을 보내는 중이야. 잠시 뒤 다시 눌러줘.');
    return false;
  }
  pending = true;
  try {
    await call('command', command);
    say();
    await refresh();
    return true;
  } catch (error) { feedback(error.message); return false; }
  finally { pending = false; }
}

const openedPreviews = new Set();
function job(item, parent) {
  const card = node('div', 'job');
  card.append(node('strong', '', statuses[item.status] || '대기 중'));
  if (item.status === 'running' && item.progress_label) {
    card.append(node('small', '', item.progress_label));
  }
  if (item.plan) {
    const {step, completed, total} = item.plan;
    if (step) card.append(node('p', '', step));
    const label = '단계 ' + completed + ' / ' + total;
    const meter = node('progress');
    meter.max = total;
    meter.value = completed;
    meter.setAttribute('aria-label', '완료한 작업 단계');
    card.append(meter, node('small', '', label));
  }
  if (item.error) card.append(node('p', '', item.error));
  if (item.approval) {
    const approval = item.approval;
    card.append(node('p', '', approval.title), node('p', '', approval.detail));
    const key = item.id + '/' + approval.nonce;
    const approve = jobButton('이번 동작 승인', () => send({
      action:'decide', id:item.id, nonce:approval.nonce, approve:true
    }));
    const deny = jobButton('거절', () => send({ action:'decide', id:item.id, nonce:approval.nonce, approve:false }));
    if (approval.preview) {
      approve.disabled = !openedPreviews.has(key);
      card.append(jobButton('전송할 화면 보기', async () => {
        try {
          const response = await call('preview/' + key);
          const blob = await response.blob();
          const image = node('img');
          image.alt = '승인 대상 캡처';
          const objectUrl = URL.createObjectURL(blob);
          image.src = objectUrl;
          image.onload = () => {
            URL.revokeObjectURL(objectUrl);
            openedPreviews.add(key);
            approve.disabled = false;
          };
          image.onerror = () => URL.revokeObjectURL(objectUrl);
          card.querySelector('img')?.remove();
          card.append(image);
        } catch (error) { say(error.message); }
      }));
    }
    card.append(approve, deny);
  } else if (item.status === 'running') {
    card.append(jobButton('중단', () => send({ action:'cancel', id:item.id })));
  }
  for (const decision of item.decisions) {
    card.append(node('small', '', (decision.approved ? '승인 · ' : '거절 · ') + decision.action));
  }
  parent.append(card);
}

function draw() {
  for (const button of document.querySelectorAll('[data-tab]')) {
    button.setAttribute('aria-pressed', String(button.dataset.tab === selected));
  }
  const connected = state !== null;
  byId('connection').hidden = connected && selected !== 'connection';
  const reconnecting = !connected && Boolean(secret);
  byId('pair').hidden = connected || reconnecting;
  byId('pair-instructions').hidden = connected || reconnecting;
  byId('reconnecting').hidden = !reconnecting;
  byId('forget').hidden = !connected && !reconnecting;
  byId('workspace').hidden = !connected || selected === 'connection';
  if (!connected || selected === 'connection') return;
  const tasks = selected === 'tasks';
  byId('voice').hidden = tasks;
  byId('layout').classList.toggle('tasks', tasks);
  byId('composer').hidden = tasks;
  byId('conversation-title').textContent = tasks ? '작업 관제' : '대화';
  byId('host').textContent = state.host;
  byId('project').textContent = '프로젝트 · ' + state.project;
  if (typeof drawWebVoice !== 'function' || !drawWebVoice()) {
    byId('orb').dataset.mode = state.mode;
    byId('mode').textContent = labels[state.mode] || '대기 중';
    byId('levels').classList.toggle('active', ['listening','speaking'].includes(state.mode));
  }
  byId('running').textContent = state.running;
  byId('waiting').textContent = state.approvals;
  byId('pending').textContent = state.approvals;
  byId('pending').hidden = state.approvals === 0;
  byId('send').disabled = state.busy;
  const visible = tasks
    ? state.exchanges.filter((item) => item.dashboard_rank !== null)
      .sort((a, b) => a.dashboard_rank - b.dashboard_rank || b.id - a.id)
    : state.exchanges;
  const signature = selected + JSON.stringify(visible);
  if (rendered === signature) return;
  rendered = signature;
  const messages = byId('messages');
  const follow = messages.scrollHeight - messages.scrollTop - messages.clientHeight < 80;
  const content = document.createDocumentFragment();
  if (!visible.length) content.append(node('p','',tasks ? '아직 실행된 작업이 없어.' : '프라이데이에게 첫 메시지를 보내봐.'));
  for (const item of visible) {
    const group = node('div');
    bubble(group, (item.background ? '백그라운드' : '나') + ' · ' + item.project, item.prompt);
    if (item.reply) bubble(group, 'FRIDAY', item.reply, true);
    if (tasks || item.approval || !item.reply) job(item, group);
    content.append(group);
  }
  messages.replaceChildren(content);
  if (follow && !tasks) messages.scrollTop = messages.scrollHeight;
}

const composer = byId('composer');
const message = byId('message');
const composerFeedback = (text) => { byId('composer-feedback').textContent = text; };

async function submitMessage(kind) {
  const text = message.value.trim();
  if (!text) {
    composerFeedback('보낼 내용을 먼저 입력해줘.');
    message.focus();
    return;
  }
  composerFeedback('보내는 중…');
  if (await send({action:'submit', kind, text}, composerFeedback)) {
    if (message.value.trim() === text) message.value = '';
    composerFeedback(kind === 'background' ? '백그라운드 작업을 보냈어. 작업 탭에서 확인할 수 있어.' : '요청을 보냈어.');
  }
}

message.addEventListener('input', () => composerFeedback(''));
composer.addEventListener('submit', (event) => {
  event.preventDefault();
  submitMessage('chat');
});
for (const button of composer.querySelectorAll('[data-submit-kind]')) {
  button.addEventListener('click', () => submitMessage(button.dataset.submitKind));
}

draw();
refresh();
setInterval(refresh, 1250);
