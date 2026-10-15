// 우리집 AI: chat with a home computer's installed AI from a phone browser.
// The page comes from the relay, but everything after "연결" is end-to-end encrypted between this
// phone and the home computer (iroh, compiled to WebAssembly). The relay only passes bytes along.

import init, { Phone } from "./aic_web.js";

const STORE = "aic-phone/v1";
const PAIR_PATH = "/ai-character/pair";
const enc = new TextEncoder();
const dec = new TextDecoder();

const store = load();
let phone = null;
let page = "loading";
let busyText = "";
let errorText = "";
let current = null; // selected computer
let models = [];
let presence = {};
let streaming = null;

function load() {
  try {
    return { secret: "", token: "", name: "휴대폰", computers: [], computer: "", model: "", messages: [], ...JSON.parse(localStorage.getItem(STORE) || "{}") };
  } catch {
    return { secret: "", token: "", name: "휴대폰", computers: [], computer: "", model: "", messages: [] };
  }
}

function save() {
  store.messages = store.messages.slice(-60);
  localStorage.setItem(STORE, JSON.stringify(store));
}

function h(tag, props = {}, ...kids) {
  const node = document.createElement(tag);
  for (const [key, value] of Object.entries(props)) {
    if (value == null || value === false) continue;
    if (key === "text") node.textContent = value;
    else if (key.startsWith("on")) node.addEventListener(key.slice(2), value);
    else if (key in node && key !== "list") node[key] = value;
    else node.setAttribute(key, value);
  }
  for (const kid of kids.flat()) if (kid != null && kid !== false) node.append(kid);
  return node;
}

function confirmButton(label, armedLabel, action) {
  const button = h("button", { class: "small", text: label });
  let timer = null;
  button.addEventListener("click", () => {
    if (!button.classList.contains("armed")) {
      button.classList.add("armed");
      button.textContent = armedLabel;
      timer = setTimeout(() => { button.classList.remove("armed"); button.textContent = label; }, 4000);
      return;
    }
    clearTimeout(timer);
    action();
  });
  return button;
}

// ---------------------------------------------------------------- relay API (same origin)

async function relayApi(path, body) {
  const response = await fetch(`/aic/v1${path}`, {
    method: body ? "POST" : "GET",
    headers: { Authorization: `Bearer ${store.token}`, ...(body ? { "Content-Type": "application/json" } : {}) },
    body: body ? JSON.stringify(body) : undefined,
  });
  const data = await response.json().catch(() => ({}));
  return { status: response.status, data };
}

function refusal(status) {
  if (status === 401) return "접속 암호가 맞지 않습니다. 중계 서버를 설치할 때 받은 접속 암호를 다시 확인해 주세요.";
  if (status === 429) return "틀린 암호나 숫자를 너무 많이 보내 30분 동안 막혔습니다. 잠시 뒤 다시 시도하거나 서버 관리 화면에서 풀어 주세요.";
  return `중계 서버가 거절했습니다 (HTTP ${status}).`;
}

// ---------------------------------------------------------------- 6-digit codes (same scheme as the app)

async function sha256(text) {
  return new Uint8Array(await crypto.subtle.digest("SHA-256", enc.encode(text)));
}

const hex = (bytes) => [...bytes].map((b) => b.toString(16).padStart(2, "0")).join("");

function fromBase64Url(text) {
  const b64 = text.replace(/-/g, "+").replace(/_/g, "/").padEnd(Math.ceil(text.length / 4) * 4, "=");
  return Uint8Array.from(atob(b64), (c) => c.charCodeAt(0));
}

async function openTicket(digits, sealed) {
  const raw = fromBase64Url(sealed);
  const key = await crypto.subtle.importKey("raw", await sha256(`aic-short-key/1:${digits}`), "AES-GCM", false, ["decrypt"]);
  const plain = await crypto.subtle.decrypt({ name: "AES-GCM", iv: raw.slice(0, 12) }, key, raw.slice(12));
  return dec.decode(plain);
}

function decodeTicket(code) {
  const body = code.replace(/\s/g, "").replace(/^AIC1\./, "");
  const ticket = JSON.parse(dec.decode(fromBase64Url(body)));
  if (ticket.v !== 1 || !ticket.id || !ticket.s) throw new Error("연결 코드가 올바르지 않습니다.");
  if (!ticket.r) throw new Error("그 컴퓨터에 중계 서버가 설정되어 있지 않아 휴대폰에서는 연결할 수 없습니다. 그 컴퓨터의 '밖에서도 쓰기: 중계 서버'를 먼저 설정해 주세요.");
  return ticket;
}

// ---------------------------------------------------------------- HTTP over the encrypted connection

/** Parses an HTTP/1.1 response as it streams in; calls onBody with decoded body text pieces. */
function responseReader(onHead, onBody) {
  let buffer = new Uint8Array(0);
  let head = null;
  let chunked = false;
  let chunkLeft = -1; // -1: reading a size line
  const textDecoder = new TextDecoder();
  const append = (bytes) => {
    const next = new Uint8Array(buffer.length + bytes.length);
    next.set(buffer);
    next.set(bytes, buffer.length);
    buffer = next;
  };
  const indexOf = (seq, from = 0) => {
    outer: for (let i = from; i <= buffer.length - seq.length; i++) {
      for (let j = 0; j < seq.length; j++) if (buffer[i + j] !== seq[j]) continue outer;
      return i;
    }
    return -1;
  };
  const CRLF = [13, 10];
  return (bytes) => {
    append(bytes);
    if (!head) {
      const end = indexOf([13, 10, 13, 10]);
      if (end < 0) return;
      const lines = dec.decode(buffer.slice(0, end)).split("\r\n");
      const status = Number(lines[0].split(" ")[1]);
      const headers = Object.fromEntries(lines.slice(1).map((line) => [line.split(":")[0].trim().toLowerCase(), line.slice(line.indexOf(":") + 1).trim()]));
      head = { status, headers };
      chunked = /chunked/i.test(headers["transfer-encoding"] || "");
      buffer = buffer.slice(end + 4);
      onHead(head);
    }
    if (!chunked) {
      if (buffer.length) onBody(textDecoder.decode(buffer, { stream: true }));
      buffer = new Uint8Array(0);
      return;
    }
    for (;;) {
      if (chunkLeft < 0) {
        const end = indexOf(CRLF);
        if (end < 0) return;
        chunkLeft = parseInt(dec.decode(buffer.slice(0, end)), 16) || 0;
        buffer = buffer.slice(end + 2);
        if (chunkLeft === 0) return;
      }
      if (buffer.length < chunkLeft + 2) {
        if (buffer.length > 0 && buffer.length <= chunkLeft) {
          onBody(textDecoder.decode(buffer, { stream: true }));
          chunkLeft -= buffer.length;
          buffer = new Uint8Array(0);
        }
        return;
      }
      onBody(textDecoder.decode(buffer.slice(0, chunkLeft), { stream: true }));
      buffer = buffer.slice(chunkLeft + 2);
      chunkLeft = -1;
    }
  };
}

async function httpRequest(computer, method, path, body, onHead, onBody) {
  const payload = body == null ? "" : JSON.stringify(body);
  const length = enc.encode(payload).length;
  const raw = `${method} ${path} HTTP/1.1\r\nHost: home\r\nContent-Type: application/json\r\nContent-Length: ${length}\r\n\r\n${payload}`;
  const reader = responseReader(onHead, onBody);
  await phone.request(computer.id, computer.relay, computer.token, enc.encode(raw), (chunk) => reader(chunk));
}

async function httpJson(computer, method, path, body) {
  let status = 0;
  let text = "";
  await httpRequest(computer, method, path, body, (head) => (status = head.status), (piece) => (text += piece));
  let data = null;
  try { data = JSON.parse(text); } catch { data = null; }
  return { status, data };
}

// ---------------------------------------------------------------- actions

async function startPhone() {
  await init();
  phone = await Phone.start(store.secret, location.origin, store.token);
  store.secret = phone.secret();
  save();
  if (!(await phone.online(10000))) {
    phone = null;
    throw new Error("중계 서버에 연결하지 못했습니다. 접속 암호가 맞는지, 인터넷이 되는지 확인해 주세요.");
  }
}

async function run(text, task) {
  busyText = text;
  errorText = "";
  render();
  try {
    await task();
  } catch (error) {
    errorText = (error && error.message) || String(error);
  }
  busyText = "";
  render();
}

async function saveToken(token) {
  store.token = token.trim();
  const { status } = await relayApi("/info");
  if (status !== 200) {
    store.token = "";
    throw new Error(refusal(status));
  }
  save();
  await startPhone();
  page = store.computers.length ? "chat" : "pair";
  if (page === "chat") await openComputer(store.computer);
}

async function pairWithCode(input, name) {
  const digits = input.replace(/\D/g, "");
  if (digits.length !== 6) throw new Error("6자리 숫자를 넣어 주세요.");
  const { status, data } = await relayApi("/codes/take", { id: hex(await sha256(`aic-short-id/1:${digits}`)) });
  if (status === 404) throw new Error("그 숫자를 찾지 못했습니다. 숫자를 다시 확인하거나 (10분이 지났거나 한 번 쓰면 사라집니다) 컴퓨터에서 새로 만들어 주세요.");
  if (status !== 200) throw new Error(refusal(status));
  let ticket;
  try {
    ticket = decodeTicket(await openTicket(digits, data.sealed));
  } catch (error) {
    throw new Error(error.message.includes("중계") ? error.message : "받은 연결 정보를 풀지 못했습니다. 새 숫자를 만들어 다시 시도해 주세요.");
  }
  const computer = { id: ticket.id, name: ticket.n || "우리집 컴퓨터", relay: ticket.r, token: ticket.t || store.token };
  store.name = name.trim() || "휴대폰";
  const { status: paired, data: answer } = await httpJson(computer, "POST", PAIR_PATH, { secret: ticket.s, name: store.name });
  if (paired !== 200) throw new Error((answer && answer.error) || "그 컴퓨터가 연결을 허락하지 않았습니다.");
  store.computers = store.computers.filter((c) => c.id !== computer.id).concat(computer);
  store.computer = computer.id;
  save();
  draft.code = "";
  page = "chat";
  await openComputer(computer.id);
}

async function refreshPresence() {
  if (!store.computers.length) return;
  const { status, data } = await relayApi("/presence", { ids: store.computers.map((c) => c.id) }).catch(() => ({ status: 0 }));
  if (status === 200) presence = data.devices || {};
}

async function openComputer(id) {
  current = store.computers.find((c) => c.id === id) || store.computers[0] || null;
  models = [];
  if (!current) return;
  store.computer = current.id;
  save();
  await refreshPresence();
  if (presence[current.id] && presence[current.id].online === false) {
    throw new Error(`${current.name}이(가) 꺼져 있거나 인터넷에 연결되어 있지 않습니다. 컴퓨터를 켜고 AI Character를 실행해 주세요.`);
  }
  const { status, data } = await httpJson(current, "GET", "/api/tags");
  if (status === 403) throw new Error("그 컴퓨터가 이 휴대폰의 연결을 끊었습니다. 컴퓨터에서 새 숫자를 만들어 다시 연결해 주세요.");
  if (status !== 200) throw new Error((data && data.error) || "그 컴퓨터의 AI(Ollama)에 연결하지 못했습니다. 그 컴퓨터에서 Ollama가 켜져 있는지 확인해 주세요.");
  models = ((data && data.models) || []).map((m) => m.name).filter((name) => !/embed/i.test(name));
  if (!models.includes(store.model)) store.model = models[0] || "";
  save();
}

async function send(text) {
  if (!current || !store.model || streaming) return;
  store.messages.push({ role: "user", content: text });
  const answer = { role: "assistant", content: "" };
  store.messages.push(answer);
  streaming = { id: current.id };
  render();
  let status = 0;
  let pending = "";
  let errorBody = "";
  const bubble = () => document.querySelector("#messages .msg.assistant:last-of-type");
  try {
    const history = store.messages.slice(0, -1).filter((m) => m.role !== "error").slice(-20).map(({ role, content }) => ({ role, content }));
    await httpRequest(current, "POST", "/v1/chat/completions", { model: store.model, messages: history, stream: true }, (head) => (status = head.status), (piece) => {
      if (status !== 200) {
        errorBody += piece;
        return;
      }
      pending += piece;
      const lines = pending.split("\n");
      pending = lines.pop();
      for (const line of lines) {
        const data = line.startsWith("data:") ? line.slice(5).trim() : "";
        if (!data || data === "[DONE]") continue;
        try {
          const delta = JSON.parse(data).choices?.[0]?.delta?.content;
          if (delta) {
            answer.content += delta;
            const node = bubble();
            if (node) node.textContent = answer.content;
            scrollDown();
          }
        } catch { /* partial line */ }
      }
    });
    if (status !== 200) {
      let message = "";
      try { message = JSON.parse(errorBody).error?.message || JSON.parse(errorBody).error; } catch { message = ""; }
      throw new Error(typeof message === "string" && message ? message : `답을 받지 못했습니다 (HTTP ${status}).`);
    }
  } catch (error) {
    if (streaming && streaming.stopped) {
      answer.content += answer.content ? "\n\n(멈춤)" : "(멈춤)";
    } else {
      if (!answer.content) store.messages.pop();
      store.messages.push({ role: "error", content: (error && error.message) || String(error) });
    }
  }
  streaming = null;
  save();
  render();
}

function stop() {
  if (!streaming) return;
  streaming.stopped = true;
  phone.disconnect(streaming.id);
}

function scrollDown() {
  window.scrollTo({ top: document.body.scrollHeight });
}

// ---------------------------------------------------------------- screens

function header(title, ...right) {
  return h("header", {}, h("h1", { text: title }), ...right);
}

function status() {
  return [
    busyText ? h("p", { class: "notice ok", text: busyText }) : null,
    errorText ? h("p", { class: "notice", text: errorText }) : null,
  ];
}

function tokenPage() {
  const token = h("input", { type: "password", placeholder: "접속 암호", autocomplete: "current-password" });
  const go = h("button", { class: "primary", text: "다음", disabled: !!busyText, onclick: () => run("확인 중…", () => saveToken(token.value)) });
  return [
    header("🏠 우리집 AI"),
    h("div", { class: "page" },
      h("div", { class: "card" },
        h("h2", { text: "휴대폰에서 집 컴퓨터 AI 쓰기" }),
        h("p", { class: "muted", text: "집 컴퓨터에 설치한 AI와 휴대폰에서 대화합니다. 대화 내용은 휴대폰과 집 컴퓨터 사이에서만 암호화되어 오가고, 이 중계 서버도 볼 수 없습니다." }),
        h("p", { text: "먼저 이 중계 서버의 접속 암호를 넣어 주세요. (서버를 설치할 때 화면에 나온 값, 앱의 '중계 서버' 설정에 넣은 것과 같습니다)" }),
        token, go),
      ...status()),
  ];
}

const draft = { code: "", name: "" };

function pairPage() {
  const code = h("input", { class: "digits", inputmode: "numeric", autocomplete: "one-time-code", maxlength: 7, placeholder: "000000", value: draft.code, oninput: (e) => (draft.code = e.target.value) });
  const name = h("input", { type: "text", placeholder: "이 휴대폰 이름 (예: 내 아이폰)", value: draft.name || (store.name === "휴대폰" ? "" : store.name), maxlength: 40, oninput: (e) => (draft.name = e.target.value) });
  const go = h("button", { class: "primary", text: "연결하기", disabled: !!busyText, onclick: () => run("연결 중… (최대 20초)", () => pairWithCode(code.value, name.value)) });
  return [
    header("🏠 컴퓨터 연결", store.computers.length ? h("button", { class: "small", text: "닫기", onclick: () => { page = "chat"; errorText = ""; render(); } }) : null),
    h("div", { class: "page" },
      h("div", { class: "card" },
        h("h2", { text: "집 컴퓨터에서 6자리 숫자 만들기" }),
        h("ol", {},
          h("li", { text: "집 컴퓨터의 AI Character에서 '처음 설정 (웹) → 🌐 다른 컴퓨터 AI'를 엽니다." }),
          h("li", { text: "'A. 이 컴퓨터를 빌려주기'에서 공유를 켜고 '연결 코드 만들기'를 누릅니다." }),
          h("li", { text: "화면에 나온 6자리 숫자를 아래에 넣습니다 (10분 안에, 한 번만)." })),
        code, name, go),
      ...status()),
  ];
}

function chatPage() {
  const computerSelect = h("select", { onchange: (e) => run("연결 중…", () => openComputer(e.target.value)) },
    ...store.computers.map((c) => {
      const p = presence[c.id];
      const dot = p ? (p.online ? "🟢 " : "⚫ ") : "";
      return h("option", { value: c.id, selected: current && c.id === current.id, text: `${dot}${c.name}` });
    }));
  const modelSelect = h("select", { disabled: !models.length, onchange: (e) => { store.model = e.target.value; save(); } },
    ...(models.length ? models.map((m) => h("option", { value: m, selected: m === store.model, text: m })) : [h("option", { text: "모델 없음" })]));
  const input = h("textarea", { rows: 1, placeholder: models.length ? "메시지 입력" : "먼저 컴퓨터에 연결하세요", disabled: !models.length });
  input.addEventListener("input", () => { input.style.height = "auto"; input.style.height = `${input.scrollHeight}px`; });
  const submit = () => { const text = input.value.trim(); if (text) { input.value = ""; send(text); } };
  input.addEventListener("keydown", (e) => { if (e.key === "Enter" && !e.shiftKey && !e.isComposing && window.matchMedia("(pointer: fine)").matches) { e.preventDefault(); submit(); } });
  const action = streaming ? h("button", { text: "멈춤", onclick: stop }) : h("button", { class: "primary", text: "보내기", disabled: !models.length, onclick: submit });
  const messages = store.messages.length
    ? store.messages.map((m) => h("div", { class: m.role === "error" ? "msg error notice" : `msg ${m.role}`, text: m.content || (m.role === "assistant" ? "…" : "") }))
    : [h("div", { class: "empty", text: current ? `${current.name}의 AI와 대화합니다. 무엇이든 물어보세요.` : "" })];
  return [
    header("🏠 우리집 AI",
      h("button", { class: "small", text: "새 대화", disabled: !!streaming, onclick: () => { store.messages = []; save(); render(); } }),
      h("button", { class: "small", text: "설정", onclick: () => { page = "settings"; errorText = ""; render(); } })),
    h("div", { class: "bar" }, computerSelect, modelSelect),
    busyText || errorText ? h("div", { class: "page", style: "padding-bottom:0" }, ...status()) : null,
    h("div", { id: "chat" }, h("div", { id: "messages" }, ...messages)),
    h("div", { class: "composer" }, input, action),
  ];
}

function settingsPage() {
  return [
    header("⚙️ 설정", h("button", { class: "small", text: "닫기", onclick: () => { page = "chat"; render(); } })),
    h("div", { class: "page" },
      h("div", { class: "card" },
        h("h2", { text: "연결된 컴퓨터" }),
        ...store.computers.map((c) => h("div", { class: "row" }, h("span", { style: "flex:1", text: `🖥️ ${c.name}` }),
          confirmButton("빼기", "한 번 더", () => {
            store.computers = store.computers.filter((x) => x.id !== c.id);
            if (store.computer === c.id) store.computer = "";
            save();
            page = store.computers.length ? "settings" : "pair";
            if (current && current.id === c.id) current = null;
            render();
          }))),
        h("button", { text: "컴퓨터 추가 연결", onclick: () => { page = "pair"; errorText = ""; render(); } })),
      h("div", { class: "card" },
        h("h2", { text: "이 휴대폰" }),
        h("p", { class: "muted", text: "대화 기록과 연결 정보는 이 휴대폰의 브라우저에만 저장됩니다. 홈 화면에 추가하면 앱처럼 쓸 수 있습니다 (Safari: 공유 → 홈 화면에 추가)." }),
        confirmButton("이 휴대폰의 모든 정보 지우기", "한 번 더 누르면 지움", () => { localStorage.removeItem(STORE); location.reload(); })),
      h("div", { class: "card" },
        h("h2", { text: "안전한가요?" }),
        h("p", { class: "muted", text: "휴대폰과 집 컴퓨터 사이는 처음부터 끝까지 암호화됩니다. 중계 서버는 암호화된 데이터를 전달만 하고 내용을 볼 수 없습니다. 집 컴퓨터는 6자리 숫자로 허락한 기기만 받아들이며, 대화·모델 목록 기능만 쓸 수 있습니다. 휴대폰을 잃어버리면 집 컴퓨터의 'A' 카드에서 이 휴대폰을 '연결 끊기' 하세요." }))),
  ];
}

function render() {
  const root = document.getElementById("app");
  const views = { loading: () => [h("div", { class: "page" }, busyText ? null : h("p", { class: "muted", text: "불러오는 중…" }), ...status())], token: tokenPage, pair: pairPage, chat: chatPage, settings: settingsPage };
  root.replaceChildren(...(views[page] || views.loading)().filter((node) => node != null));
  if (page === "chat") scrollDown();
}

async function boot() {
  if (!store.token) {
    page = "token";
    return render();
  }
  page = "loading";
  await run("집 컴퓨터와 연결하는 중…", async () => {
    try {
      await startPhone();
    } catch (error) {
      page = "token";
      throw error;
    }
    page = store.computers.length ? "chat" : "pair";
    if (page === "chat") await openComputer(store.computer);
  });
}

// Phones freeze background tabs; a connection saved before that may be silently dead afterwards.
let hiddenAt = 0;
document.addEventListener("visibilitychange", () => {
  if (document.hidden) {
    hiddenAt = Date.now();
    return;
  }
  const slept = hiddenAt && Date.now() - hiddenAt > 15000;
  hiddenAt = 0;
  if (!slept || !phone || streaming) return;
  phone.forgetAll();
  if (page === "chat") refreshPresence().then(() => { if (!streaming && !busyText && document.activeElement?.tagName !== "TEXTAREA") render(); });
});

setInterval(() => { if (page === "chat" && !document.hidden && !streaming) refreshPresence().then(() => { if (!streaming && !busyText && document.activeElement?.tagName !== "TEXTAREA") render(); }); }, 60000);
boot();
