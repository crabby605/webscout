// Turns QuickJS's global object into a browser window around a linkedom document.
// Rust provides: __linkedom (vendor/dom-bundle.js), __ws_config, __ws_http,
// __ws_cookie_get, __ws_cookie_set, __ws_log. Rust then calls __ws_tick() until
// it returns false, and __ws_serialize() at the end.
(() => {
  const L = __linkedom, cfg = __ws_config, G = globalThis;
  const { window: lwin, document } = L.parseHTML(cfg.html);

  // Classes (Node, HTMLElement, Event, ...) as globals, like a browser window.
  for (const k of Object.keys(L)) if (/^[A-Z]/.test(k) && !(k in G)) G[k] = L[k];
  for (const k of ["MutationObserver", "customElements", "CustomElementRegistry"]) {
    if (lwin[k] && !(k in G)) G[k] = lwin[k];
  }
  G.window = G.self = G.top = G.parent = G.frames = G;
  G.document = document;

  // window is an event target too.
  const winEvents = new L.EventTarget();
  for (const m of ["addEventListener", "removeEventListener", "dispatchEvent"]) {
    G[m] = winEvents[m].bind(winEvents);
  }

  // QuickJS's e.stack omits the message, so prepend it.
  const errText = e => e instanceof Error ? `${e}\n${e.stack || ""}`.trim() : String(e);
  const log = (level, args) => {
    const msg = args.map(a => {
      if (typeof a === "string") return a;
      if (a instanceof Error) return errText(a);
      try { return JSON.stringify(a); } catch { return String(a); }
    }).join(" ");
    __ws_log(level, msg);
  };
  G.console = {};
  for (const l of ["log", "info", "warn", "error", "debug", "trace"]) G.console[l] = (...a) => log(l, a);
  for (const l of ["group", "groupEnd", "groupCollapsed", "time", "timeEnd", "table", "assert", "count", "dir"]) G.console[l] = () => {};

  // --- location / history -------------------------------------------------
  let loc = new URL(cfg.url);
  const navigate = v => { const u = new URL(v, loc); __ws_log("navigate", u.href); };
  const location = {
    get href() { return loc.href; }, set href(v) { navigate(v); },
    assign: navigate, replace: navigate, reload() {},
    toString() { return loc.href; },
    get hash() { return loc.hash; }, set hash(v) { loc.hash = v; },
  };
  for (const k of ["protocol", "host", "hostname", "port", "pathname", "search", "origin"]) {
    Object.defineProperty(location, k, { get: () => loc[k], enumerable: true });
  }
  G.location = location;
  const setUrl = url => { if (url != null) loc = new URL(url, loc); };
  G.history = {
    length: 1, state: null, scrollRestoration: "auto",
    pushState(s, _t, url) { this.state = s; setUrl(url); },
    replaceState(s, _t, url) { this.state = s; setUrl(url); },
    back() {}, forward() {}, go() {},
  };
  const docProps = {
    location: () => location,
    URL: () => loc.href,
    documentURI: () => loc.href,
    domain: () => loc.hostname,
    referrer: () => "",
    readyState: () => readyState,
    visibilityState: () => "visible",
    hidden: () => false,
    defaultView: () => G,
    currentScript: () => currentScript,
  };
  let readyState = "loading", currentScript = null;
  for (const [k, get] of Object.entries(docProps)) Object.defineProperty(document, k, { get, configurable: true });
  Object.defineProperty(document, "cookie", {
    get: () => __ws_cookie_get(loc.href),
    set: v => __ws_cookie_set(loc.href, String(v)),
    configurable: true,
  });
  document.hasFocus = () => true;

  // --- navigator / screen / viewport --------------------------------------
  // Bot checks probe the *shape* too: real browsers expose these as getters on
  // interface prototypes (Navigator.prototype etc.), never as own properties.
  const ua = cfg.userAgent;
  const [vw, vh] = cfg.viewport.split("x").map(Number);
  const chromium = /Chrome\//.test(ua), firefox = /Firefox\//.test(ua);
  const ifaces = [];
  const iface = (name, members = {}) => {
    const C = { [name]: function () { throw new TypeError("Illegal constructor"); } }[name];
    Object.defineProperties(C.prototype, Object.getOwnPropertyDescriptors(members));
    Object.defineProperty(C.prototype, Symbol.toStringTag, { value: name, configurable: true });
    G[name] = C;
    ifaces.push(C);
    return C;
  };
  // Fields non-enumerable: in browsers they're prototype getters, invisible to
  // JSON.stringify / Object.keys (and plugin <-> mimeType links would be circular).
  const make = (C, fields = {}) => {
    const o = Object.create(C.prototype);
    for (const [k, value] of Object.entries(fields)) Object.defineProperty(o, k, { value, writable: true, configurable: true });
    return o;
  };
  // Array-likes (PluginArray, Plugin, MimeTypeArray): indexed items + namedItem(key).
  const lists = new WeakMap();
  const listMembers = key => ({
    get length() { return lists.get(this).length; },
    item(i) { return lists.get(this)[i] ?? null; },
    namedItem(n) { return lists.get(this).find(x => x[key] === n) ?? null; },
    refresh() {},
    [Symbol.iterator]() { return lists.get(this)[Symbol.iterator](); },
  });
  const makeList = (C, items, fields) => {
    const o = make(C, fields);
    lists.set(o, items);
    items.forEach((x, i) => Object.defineProperty(o, i, { value: x, enumerable: true }));
    return o;
  };
  const PluginArray = iface("PluginArray", listMembers("name"));
  const Plugin = iface("Plugin", listMembers("type"));
  const MimeTypeArray = iface("MimeTypeArray", listMembers("type"));
  const MimeType = iface("MimeType");
  // Every current browser reports this same fixed list of built-in PDF viewers.
  const pdfTypes = ["application/pdf", "text/pdf"].map(type => make(MimeType, { type, suffixes: "pdf", description: "Portable Document Format" }));
  const plugins = ["PDF Viewer", "Chrome PDF Viewer", "Chromium PDF Viewer", "Microsoft Edge PDF Viewer", "WebKit built-in PDF"]
    .map(name => makeList(Plugin, pdfTypes, { name, filename: "internal-pdf-viewer", description: "Portable Document Format" }));
  for (const t of pdfTypes) Object.defineProperty(t, "enabledPlugin", { value: plugins[0] });

  const PermissionStatus = iface("PermissionStatus", { addEventListener() {}, removeEventListener() {}, dispatchEvent: () => true });
  const Permissions = iface("Permissions", {
    query(desc) {
      const name = desc?.name;
      if (typeof name !== "string") return Promise.reject(new TypeError("Failed to execute 'query' on 'Permissions'"));
      // "prompt" matches Notification.permission === "default", as in a fresh profile.
      return Promise.resolve(make(PermissionStatus, { name, state: "prompt", onchange: null }));
    },
  });
  const permissions = make(Permissions);

  // Client hints, parsed from the sec-ch-ua header the HTTP layer sends, so both agree.
  const brands = [...(cfg.secChUa || "").matchAll(/"([^"]+)";v="([^"]+)"/g)].map(([, brand, version]) => ({ brand, version }));
  const fullVersion = (ua.match(/Chrome\/([\d.]+)/) || [])[1] || "";
  const NavigatorUAData = iface("NavigatorUAData", {
    get brands() { return brands.map(b => ({ ...b })); },
    get mobile() { return false; },
    get platform() { return cfg.platform; },
    getHighEntropyValues(hints) {
      const all = {
        architecture: "x86", bitness: "64", model: "", wow64: false, formFactors: ["Desktop"],
        platformVersion: cfg.platform === "Windows" ? "10.0.0" : "15.0.0",
        uaFullVersion: fullVersion,
        // GREASE brands ("Not/A)Brand") keep their fake version.
        fullVersionList: brands.map(b => ({ brand: b.brand, version: /Brand/.test(b.brand) ? `${b.version}.0.0.0` : fullVersion })),
      };
      const out = { brands: this.brands, mobile: false, platform: cfg.platform };
      for (const h of hints || []) if (h in all) out[h] = all[h];
      return Promise.resolve(out);
    },
    toJSON() { return { brands: this.brands, mobile: false, platform: cfg.platform }; },
  });

  const nav = {
    userAgent: ua,
    appVersion: firefox ? `5.0 (${cfg.platform === "Windows" ? "Windows" : "Macintosh"})` : ua.replace(/^Mozilla\//, ""),
    appName: "Netscape", appCodeName: "Mozilla", product: "Gecko",
    productSub: firefox ? "20100101" : "20030107",
    platform: { Windows: "Win32", macOS: "MacIntel", Linux: "Linux x86_64" }[cfg.platform] || cfg.platform,
    vendor: chromium ? "Google Inc." : /Safari/.test(ua) && !firefox ? "Apple Computer, Inc." : "",
    vendorSub: "",
    language: cfg.languages[0], languages: Object.freeze([...cfg.languages]),
    cookieEnabled: true, onLine: true, webdriver: false, doNotTrack: null,
    hardwareConcurrency: 8, maxTouchPoints: 0, pdfViewerEnabled: true,
    plugins: makeList(PluginArray, plugins), mimeTypes: makeList(MimeTypeArray, pdfTypes),
    permissions,
    // Device labels/ids stay empty until a permission grant, as in a fresh profile.
    mediaDevices: {
      enumerateDevices: () => Promise.resolve(["audioinput", "videoinput", "audiooutput"].map(kind => ({ kind, label: "", deviceId: "", groupId: "" }))),
      getUserMedia: () => Promise.reject(new DOMException("Permission denied", "NotAllowedError")),
      addEventListener() {}, removeEventListener() {},
    },
  };
  if (chromium) Object.assign(nav, {
    deviceMemory: 8,
    userAgentData: make(NavigatorUAData),
    connection: { effectiveType: "4g", rtt: 50, downlink: 10, saveData: false, addEventListener() {}, removeEventListener() {} },
  });
  if (firefox) Object.assign(nav, {
    oscpu: cfg.platform === "Windows" ? "Windows NT 10.0; Win64; x64" : "Intel Mac OS X 10.15",
    buildID: "20181001000000",
  });
  const navMembers = { sendBeacon: () => true, javaEnabled: () => false, vibrate: () => true };
  if (chromium) navMembers.getBattery = () => Promise.resolve({ charging: true, level: 1, chargingTime: 0, dischargingTime: Infinity, addEventListener() {}, removeEventListener() {} });
  for (const [k, v] of Object.entries(nav)) {
    const get = () => v;
    Object.defineProperty(get, "name", { value: `get ${k}` });
    Object.defineProperty(navMembers, k, { get, enumerable: true, configurable: true });
  }
  G.navigator = make(iface("Navigator", navMembers));
  if (chromium) G.chrome = {
    app: {
      isInstalled: false, getDetails: () => null, getIsInstalled: () => false, runningState: () => "cannot_run",
      InstallState: { DISABLED: "disabled", INSTALLED: "installed", NOT_INSTALLED: "not_installed" },
      RunningState: { CANNOT_RUN: "cannot_run", READY_TO_RUN: "ready_to_run", RUNNING: "running" },
    },
    csi: () => ({ startE: Date.now(), onloadT: Date.now(), pageT: 1000, tran: 15 }),
    loadTimes: () => ({ requestTime: Date.now() / 1000, startLoadTime: Date.now() / 1000, navigationType: "Other", wasFetchedViaSpdy: true, npnNegotiatedProtocol: "h2", connectionInfo: "h2" }),
  };
  G.screen = { width: vw, height: vh, availWidth: vw, availHeight: vh - 40, colorDepth: 24, pixelDepth: 24 };
  Object.assign(G, {
    innerWidth: vw, innerHeight: vh, outerWidth: vw, outerHeight: vh,
    devicePixelRatio: 1, scrollX: 0, scrollY: 0, pageXOffset: 0, pageYOffset: 0,
    scrollTo() {}, scroll() {}, scrollBy() {}, focus() {}, blur() {}, print() {},
    alert() {}, confirm: () => false, prompt: () => null, open: () => null, close() {},
    isSecureContext: loc.protocol === "https:",
  });
  G.matchMedia = media => ({
    matches: false, media, onchange: null,
    addListener() {}, removeListener() {}, addEventListener() {}, removeEventListener() {},
  });
  G.getComputedStyle = el => el.style;
  class NoopObserver { observe() {} unobserve() {} disconnect() {} takeRecords() { return []; } }
  for (const k of ["IntersectionObserver", "ResizeObserver", "PerformanceObserver"]) G[k] = NoopObserver;
  // linkedom gaps that break real libraries (jQuery/Sizzle among them).
  for (const C of [L.Element, L.Document, L.DocumentFragment]) {
    const p = C.prototype, orig = p.getElementsByTagName;
    if (orig) p.getElementsByTagName = function (name) { return name === "*" ? this.querySelectorAll("*") : orig.call(this, name); };
  }
  const fragText = Object.getOwnPropertyDescriptor(L.DocumentFragment.prototype, "textContent")
    || Object.getOwnPropertyDescriptor(L.Node.prototype, "textContent");
  Object.defineProperty(L.DocumentFragment.prototype, "textContent", {
    get() { return fragText.get.call(this); },
    set(v) {
      while (this.firstChild) this.removeChild(this.firstChild);
      if (v != null && v !== "") this.appendChild(this.ownerDocument.createTextNode(String(v)));
    },
    configurable: true,
  });
  // linkedom's setters assume strings; browsers coerce (`el.innerHTML = 0`, `= null`).
  for (const k of ["innerHTML", "outerHTML", "textContent"]) {
    for (let p = L.Element.prototype; p; p = Object.getPrototypeOf(p)) {
      const d = Object.getOwnPropertyDescriptor(p, k);
      if (!d?.set) continue;
      Object.defineProperty(p, k, { ...d, set(v) { d.set.call(this, v == null ? "" : String(v)); } });
      break;
    }
  }
  const HE = (L.HTMLElement || L.Element).prototype;
  if (!Object.getOwnPropertyDescriptor(HE, "innerText")) {
    Object.defineProperty(HE, "innerText", {
      get() { return this.textContent; },
      set(v) { this.textContent = v; },
      configurable: true,
    });
  }
  // Same-origin iframes: their own empty document, and a window that inherits ours,
  // so iframe.contentWindow.navigator agrees with the top window (fingerprinters compare).
  const frames = new WeakMap();
  const frame = el => {
    if (!frames.has(el)) {
      const doc = implementation.createHTMLDocument("");
      const win = Object.create(G, { document: { value: doc }, frameElement: { value: el } });
      Object.assign(win, { window: win, self: win, parent: G, top: G });
      frames.set(el, { doc, win });
    }
    return frames.get(el);
  };
  const IF = (L.HTMLIFrameElement || L.HTMLElement).prototype;
  Object.defineProperty(IF, "contentDocument", { get() { return this.tagName === "IFRAME" ? frame(this).doc : undefined; }, configurable: true });
  Object.defineProperty(IF, "contentWindow", { get() { return this.tagName === "IFRAME" ? frame(this).win : undefined; }, configurable: true });
  // jQuery's init feature-tests document.implementation.createHTMLDocument.
  const DOMImplementation = iface("DOMImplementation", {
    createHTMLDocument(title) {
      const d = L.parseHTML("<!DOCTYPE html><html><head></head><body></body></html>").document;
      if (title !== undefined) d.title = String(title);
      return d;
    },
    createDocumentType: (name, publicId = "", systemId = "") => ({ nodeType: 10, name, publicId, systemId }),
    hasFeature: () => true,
  });
  const implementation = make(DOMImplementation);
  Object.defineProperty(document, "implementation", { get: () => implementation, configurable: true });
  const E = L.Element.prototype;
  E.getBoundingClientRect ??= () => ({ x: 0, y: 0, top: 0, left: 0, right: 0, bottom: 0, width: 0, height: 0 });
  E.getClientRects ??= () => [];
  for (const m of ["scrollIntoView", "focus", "blur", "click"]) E[m] ??= function () {};

  G.crypto = {
    getRandomValues(arr) {
      const bytes = __ws_random(arr.byteLength);
      new Uint8Array(arr.buffer, arr.byteOffset, arr.byteLength).set(bytes);
      return arr;
    },
    randomUUID() {
      const b = __ws_random(16);
      b[6] = (b[6] & 0x0f) | 0x40;
      b[8] = (b[8] & 0x3f) | 0x80;
      const h = b.map(x => x.toString(16).padStart(2, "0")).join("");
      return `${h.slice(0, 8)}-${h.slice(8, 12)}-${h.slice(12, 16)}-${h.slice(16, 20)}-${h.slice(20)}`;
    },
    // ponytail: no SubtleCrypto (digest/sign); back it with Rust (ring is already linked) if a site needs it.
  };
  // Legacy element constructors.
  G.Image = function Image(width, height) {
    const img = document.createElement("img");
    if (width != null) img.setAttribute("width", width);
    if (height != null) img.setAttribute("height", height);
    return img;
  };
  G.Audio = function Audio(src) {
    const a = document.createElement("audio");
    if (src != null) a.setAttribute("src", src);
    return a;
  };
  G.Option = function Option(text = "", value, defaultSelected, selected) {
    const o = document.createElement("option");
    o.textContent = text;
    if (value != null) o.setAttribute("value", value);
    if (defaultSelected || selected) o.setAttribute("selected", "");
    return o;
  };

  class Storage {
    #m = new Map();
    get length() { return this.#m.size; }
    key(i) { return [...this.#m.keys()][i] ?? null; }
    getItem(k) { return this.#m.has(String(k)) ? this.#m.get(String(k)) : null; }
    setItem(k, v) { this.#m.set(String(k), String(v)); }
    removeItem(k) { this.#m.delete(String(k)); }
    clear() { this.#m.clear(); }
  }
  G.Storage = Storage;
  G.localStorage = new Storage();
  G.sessionStorage = new Storage();

  G.TextEncoder ??= class {
    get encoding() { return "utf-8"; }
    encode(s = "") {
      const b = unescape(encodeURIComponent(s));
      return Uint8Array.from(b, c => c.charCodeAt(0));
    }
  };
  G.TextDecoder ??= class {
    decode(u) {
      if (!u) return "";
      const bytes = new Uint8Array(u.buffer ? u.buffer.slice(u.byteOffset, u.byteOffset + u.byteLength) : u);
      let s = "";
      for (const c of bytes) s += String.fromCharCode(c);
      try { return decodeURIComponent(escape(s)); } catch { return s; }
    }
  };

  // --- timers on a virtual clock ------------------------------------------
  // Time only advances when Rust calls __ws_tick, so a page's 3s setTimeout
  // runs immediately instead of making us wait 3 real seconds.
  let now = 0, seq = 0;
  const timers = new Map();
  const addTimer = (fn, ms, args, every) => {
    const id = ++seq;
    const delay = Math.max(0, Number(ms) || 0);
    timers.set(id, { fn, args, at: now + delay, every: every ? Math.max(1, delay) : 0 });
    return id;
  };
  G.setTimeout = (fn, ms, ...a) => addTimer(fn, ms, a, false);
  G.setInterval = (fn, ms, ...a) => addTimer(fn, ms, a, true);
  G.clearTimeout = G.clearInterval = id => { timers.delete(id); };
  G.requestAnimationFrame = fn => setTimeout(() => fn(now), 16);
  G.cancelAnimationFrame = id => clearTimeout(id);
  G.requestIdleCallback = fn => setTimeout(() => fn({ didTimeout: false, timeRemaining: () => 0 }), 1);
  G.cancelIdleCallback = id => clearTimeout(id);
  const t0 = Date.now();
  G.performance = {
    now: () => now, timeOrigin: t0, timing: { navigationStart: t0 },
    mark() {}, measure() {}, clearMarks() {}, clearMeasures() {},
    getEntries: () => [], getEntriesByType: () => [], getEntriesByName: () => [],
  };

  // Runs the next due timer. Returns false when nothing is left within the virtual time budget.
  G.__ws_tick = () => {
    let next = null;
    for (const [id, t] of timers) if (!next || t.at < next[1].at) next = [id, t];
    if (!next || next[1].at > cfg.maxVirtualMs) return false;
    const [id, t] = next;
    now = Math.max(now, t.at);
    if (t.every) t.at = now + t.every; else timers.delete(id);
    try {
      typeof t.fn === "function" ? t.fn(...t.args) : __ws_eval(String(t.fn), loc.href);
    } catch (e) {
      log("error", [e]);
    }
    return true;
  };

  // --- network: fetch / XHR -----------------------------------------------
  const http = (method, url, headers, body) => JSON.parse(__ws_http(
    String(method || "GET").toUpperCase(), new URL(url, loc).href,
    JSON.stringify(headers), body == null ? null : String(body),
  ));
  const corsHeaders = { "sec-fetch-dest": "empty", "sec-fetch-mode": "cors", "sec-fetch-site": "same-origin" };

  class Headers {
    constructor(init) {
      this._m = new Map();
      if (!init) return;
      const pairs = Array.isArray(init) ? init : init instanceof Headers ? [...init.entries()] : Object.entries(init);
      for (const [k, v] of pairs) this.append(k, v);
    }
    append(k, v) { k = String(k).toLowerCase(); this._m.set(k, this._m.has(k) ? `${this._m.get(k)}, ${v}` : String(v)); }
    set(k, v) { this._m.set(String(k).toLowerCase(), String(v)); }
    get(k) { return this._m.get(String(k).toLowerCase()) ?? null; }
    has(k) { return this._m.has(String(k).toLowerCase()); }
    delete(k) { this._m.delete(String(k).toLowerCase()); }
    entries() { return this._m.entries(); }
    keys() { return this._m.keys(); }
    values() { return this._m.values(); }
    forEach(cb, self) { this._m.forEach((v, k) => cb.call(self, v, k, this)); }
    [Symbol.iterator]() { return this._m.entries(); }
  }
  // ponytail: bodies are text only (lossy for binary); add ArrayBuffer transport if a site needs it.
  class Response {
    constructor(body, init = {}) {
      this._body = body == null ? "" : String(body);
      this.status = init.status ?? 200;
      this.statusText = init.statusText ?? "";
      this.ok = this.status >= 200 && this.status < 300;
      this.headers = new Headers(init.headers);
      this.url = init.url ?? "";
      this.redirected = false;
      this.type = "basic";
      this.bodyUsed = false;
    }
    text() { this.bodyUsed = true; return Promise.resolve(this._body); }
    json() { return this.text().then(JSON.parse); }
    arrayBuffer() { return this.text().then(t => new TextEncoder().encode(t).buffer); }
    clone() { return new Response(this._body, this); }
  }
  class Request {
    constructor(input, init = {}) {
      this.url = new URL(typeof input === "string" ? input : input.url, loc).href;
      this.method = String(init.method || input.method || "GET").toUpperCase();
      this.headers = new Headers(init.headers || input.headers);
      this.body = init.body ?? input.body ?? null;
      this.credentials = init.credentials || "same-origin";
    }
  }
  Object.assign(G, { Headers, Response, Request });
  G.fetch = (input, init = {}) => new Promise((resolve, reject) => {
    try {
      const req = new Request(input, init);
      const r = http(req.method, req.url, { ...corsHeaders, ...Object.fromEntries(req.headers) }, req.body);
      resolve(new Response(r.body, { status: r.status, headers: r.headers, url: r.url }));
    } catch (e) {
      reject(new TypeError(`Failed to fetch: ${e.message}`));
    }
  });

  class XMLHttpRequest extends L.EventTarget {
    constructor() {
      super();
      Object.assign(this, { readyState: 0, status: 0, statusText: "", responseText: "", response: "", responseType: "", responseURL: "", timeout: 0, withCredentials: false });
      this._h = {};
    }
    open(method, url) { this._m = method; this._u = url; this.readyState = 1; }
    setRequestHeader(k, v) { this._h[k] = v; }
    getResponseHeader(k) { return this._rh?.get(k) ?? null; }
    getAllResponseHeaders() { return [...(this._rh?.entries() ?? [])].map(([k, v]) => `${k}: ${v}`).join("\r\n"); }
    overrideMimeType() {}
    abort() {}
    send(body) {
      // Async like a real XHR: completes on the next task.
      setTimeout(() => {
        let r;
        try {
          r = http(this._m, this._u, { ...corsHeaders, ...this._h }, body);
        } catch (e) {
          __ws_log("warn", `xhr ${this._u}: ${e.message}`);
        }
        // Handlers run outside the try so page errors surface instead of becoming "network error".
        this.readyState = 4;
        if (r) {
          this.status = r.status;
          this.responseURL = r.url;
          this._rh = new Headers(r.headers);
          this.responseText = r.body;
          try { this.response = this.responseType === "json" ? JSON.parse(r.body) : r.body; } catch { this.response = null; }
        }
        this._fire("readystatechange");
        this._fire(r ? "load" : "error");
        this._fire("loadend");
      }, 0);
    }
    _fire(type) {
      const e = new L.Event(type);
      this.dispatchEvent(e);
      if (typeof this[`on${type}`] === "function") this[`on${type}`](e);
    }
  }
  Object.assign(XMLHttpRequest, { UNSENT: 0, OPENED: 1, HEADERS_RECEIVED: 2, LOADING: 3, DONE: 4 });
  G.XMLHttpRequest = XMLHttpRequest;

  // --- scripts ------------------------------------------------------------
  const JS_TYPES = ["", "text/javascript", "application/javascript", "text/ecmascript", "application/ecmascript", "text/jscript"];
  const ran = new WeakSet();
  let inlineModules = 0;
  let written = "";
  document.write = document.writeln = (...a) => { written += a.join(""); };
  document.open = document.close = () => {};

  const fire = (el, type) => {
    const e = new L.Event(type);
    el.dispatchEvent(e);
    if (typeof el[`on${type}`] === "function") el[`on${type}`](e);
  };

  const runScript = s => {
    if (ran.has(s)) return;
    ran.add(s);
    const type = (s.getAttribute("type") || "").trim().toLowerCase();
    if (type === "module") {
      // Imports are fetched by the Rust loader; evaluation finishes as promise jobs.
      const src = s.getAttribute("src");
      const name = src ? new URL(src, loc).href : `${loc.href.split("#")[0]}#inline-module-${++inlineModules}`;
      currentScript = s;
      try {
        __ws_module(name, src ? null : s.textContent)
          .then(() => src && fire(s, "load"), e => { __ws_log("error", `module ${name}: ${errText(e)}`); src && fire(s, "error"); });
      } catch (e) {
        __ws_log("error", `module ${name}: ${errText(e)}`);
      }
      currentScript = null;
      return;
    }
    if (!JS_TYPES.includes(type.split(";")[0])) return;
    const src = s.getAttribute("src");
    let code;
    if (src) {
      const url = new URL(src, loc).href;
      try {
        const r = http("GET", url, { "sec-fetch-dest": "script", "sec-fetch-mode": "no-cors" }, null);
        if (r.status >= 400) throw new Error(`HTTP ${r.status}`);
        code = r.body;
      } catch (e) {
        __ws_log("error", `script ${url}: ${e.message}`);
        fire(s, "error");
        return;
      }
    } else {
      code = s.textContent;
    }
    currentScript = s;
    written = "";
    try {
      __ws_eval(code, src ? new URL(src, loc).href : loc.href);
    } catch (e) {
      __ws_log("error", `script ${src || "(inline)"}: ${errText(e)}`);
    }
    if (written && s.parentNode) s.insertAdjacentHTML("afterend", written);
    currentScript = null;
    if (src) fire(s, "load");
  };

  // Scripts inserted by other scripts (loaders, webpack chunks) run like in a browser.
  // Scripts set via innerHTML stay inert, also like a browser.
  const insertMethods = ["appendChild", "insertBefore", "replaceChild", "append", "prepend", "after", "before", "replaceWith"];
  const probe = document.createElement("div");
  for (const m of insertMethods) {
    let proto = probe;
    while (proto && !Object.prototype.hasOwnProperty.call(proto, m)) proto = Object.getPrototypeOf(proto);
    if (!proto) continue;
    const orig = proto[m];
    proto[m] = function (...args) {
      const r = orig.apply(this, args);
      for (const n of args) {
        if (!n || n.nodeType !== 1) continue;
        const list = n.localName === "script" ? [n] : [...n.querySelectorAll("script")];
        for (const s of list) if (s.isConnected && !ran.has(s)) setTimeout(() => runScript(s), 0);
      }
      return r;
    };
  }

  // Page load, as tasks so microtasks (promises) drain between them.
  for (const s of document.querySelectorAll("script")) setTimeout(() => runScript(s), 0);
  setTimeout(() => {
    readyState = "interactive";
    document.dispatchEvent(new L.Event("DOMContentLoaded", { bubbles: true }));
  }, 0);
  setTimeout(() => {
    readyState = "complete";
    document.dispatchEvent(new L.Event("readystatechange"));
    const e = new L.Event("load");
    G.dispatchEvent(e);
    if (typeof G.onload === "function") G.onload(e);
  }, 0);

  // Browser APIs report as native code. Libraries feature-detect this to pick
  // fast native paths (Sizzle → querySelectorAll), and bot checks probe it.
  const native = new WeakSet();
  const markProto = obj => {
    for (let p = obj; p && p !== Object.prototype && p !== Function.prototype; p = Object.getPrototypeOf(p)) {
      for (const k of Object.getOwnPropertyNames(p)) {
        const d = Object.getOwnPropertyDescriptor(p, k);
        for (const f of [d.value, d.get, d.set]) if (typeof f === "function") native.add(f);
      }
    }
  };
  for (const k of Object.keys(L)) if (typeof L[k] === "function") { native.add(L[k]); markProto(L[k].prototype); }
  for (const C of ifaces) { native.add(C); markProto(C.prototype); }
  if (G.chrome) [G.chrome, G.chrome.app].forEach(markProto);
  for (const o of [G.navigator, G.location, G.history, G.performance, G.console, G.localStorage, G.crypto, document]) markProto(o);
  for (const k of ["fetch", "setTimeout", "setInterval", "clearTimeout", "clearInterval", "requestAnimationFrame",
    "cancelAnimationFrame", "requestIdleCallback", "cancelIdleCallback", "matchMedia", "getComputedStyle",
    "addEventListener", "removeEventListener", "dispatchEvent", "atob", "btoa", "queueMicrotask", "structuredClone",
    "Headers", "Response", "Request", "XMLHttpRequest", "Storage", "URL", "URLSearchParams", "TextEncoder", "TextDecoder",
    "IntersectionObserver", "ResizeObserver", "MutationObserver", "scrollTo", "alert", "confirm", "prompt", "open",
    "Image", "Audio", "Option"]) {
    if (typeof G[k] === "function") { native.add(G[k]); markProto(G[k].prototype); }
  }
  const fnToString = Function.prototype.toString;
  const toString = function () {
    return native.has(this) ? `function ${this.name.startsWith("bound ") ? "" : this.name}() { [native code] }` : fnToString.call(this);
  };
  native.add(toString);
  Function.prototype.toString = toString;
  // Lets js_webapis.js (loaded next) register what it adds.
  G.__ws_markNative = (...objs) => {
    for (const o of objs) if (typeof o === "function") { native.add(o); markProto(o.prototype); } else if (o) markProto(o);
  };

  G.__ws_serialize = () => `<!DOCTYPE html>${document.documentElement.outerHTML}`;
})();
