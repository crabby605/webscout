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
  // A navigation the page (or an action) started. Rust stops this document and loads
  // the target in a new one, like a browser; the first request wins.
  G.__ws_nav = null;
  const requestNav = (url, method = "GET", body = null, type = null) => {
    const u = new URL(url, loc);
    if (u.href.split("#")[0] === loc.href.split("#")[0] && u.hash && method === "GET") { loc.hash = u.hash; return; }
    if (/^(javascript|mailto|tel|data|blob):/i.test(u.protocol)) return;
    __ws_log("navigate", `${method} ${u.href}`);
    G.__ws_nav ??= { url: u.href, method, body, contentType: type };
  };
  const navigate = v => requestNav(v);
  const location = {
    get href() { return loc.href; }, set href(v) { navigate(v); },
    assign: navigate, replace: navigate, reload() { G.__ws_nav ??= { url: loc.href, method: "GET", body: null, contentType: null }; },
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
  const ua = cfg.userAgent, dev = cfg.device, os = dev.os;
  const chromium = /Chrome\//.test(ua), firefox = /Firefox\//.test(ua);
  const brave = cfg.browser === "brave", opera = cfg.browser === "opera";
  const desktop = dev.kind === "desktop", phone = dev.kind === "phone";
  // An iPad asks for desktop sites, so it reports itself as a Mac (with touch points).
  const ipad = os === "ios" && !phone;
  const uaPlatform = { windows: "Windows", macos: "macOS", linux: "Linux", android: "Android", ios: "iOS" }[os];
  // ponytail: fixed browser-UI heights (taskbar/menu bar, tabs+toolbar, mobile status+URL bars).
  const [sw, sh] = dev.screen;
  const availH = desktop ? sh - (os === "windows" ? 48 : os === "macos" ? 25 : 27) : sh;
  const vw = sw, vh = desktop ? availH - 85 : sh - (phone ? 140 : 90);
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
    get mobile() { return phone; },
    get platform() { return uaPlatform; },
    getHighEntropyValues(hints) {
      const all = {
        architecture: dev.arch, bitness: "64", model: dev.model || "", wow64: false,
        formFactors: [{ desktop: "Desktop", phone: "Mobile", tablet: "Tablet" }[dev.kind]],
        platformVersion: dev.os_version,
        uaFullVersion: fullVersion,
        // GREASE brands ("Not/A)Brand") keep their fake version.
        fullVersionList: brands.map(b => ({ brand: b.brand, version: /Brand/.test(b.brand) ? `${b.version}.0.0.0` : fullVersion })),
      };
      const out = { brands: this.brands, mobile: phone, platform: uaPlatform };
      for (const h of hints || []) if (h in all) out[h] = all[h];
      return Promise.resolve(out);
    },
    toJSON() { return { brands: this.brands, mobile: phone, platform: uaPlatform }; },
  });

  const nav = {
    userAgent: ua,
    appVersion: firefox ? `5.0 (${{ windows: "Windows", macos: "Macintosh", linux: "X11" }[os] || `Android ${dev.os_version.split(".")[0]}`})` : ua.replace(/^Mozilla\//, ""),
    appName: "Netscape", appCodeName: "Mozilla", product: "Gecko",
    productSub: firefox ? "20100101" : "20030107",
    platform: ipad ? "MacIntel" : { windows: "Win32", macos: "MacIntel", linux: "Linux x86_64", android: "Linux armv81", ios: "iPhone" }[os],
    vendor: chromium ? "Google Inc." : /Safari/.test(ua) && !firefox ? "Apple Computer, Inc." : "",
    vendorSub: "",
    language: cfg.languages[0], languages: Object.freeze([...cfg.languages]),
    cookieEnabled: true, onLine: true, webdriver: false, doNotTrack: null,
    hardwareConcurrency: dev.cpu_cores, maxTouchPoints: dev.touch,
    // Phones and Android tablets have no built-in PDF viewer: empty plugin lists.
    pdfViewerEnabled: desktop || ipad,
    plugins: makeList(PluginArray, desktop || ipad ? plugins : []), mimeTypes: makeList(MimeTypeArray, desktop || ipad ? pdfTypes : []),
    permissions,
    // Device labels/ids stay empty until a permission grant, as in a fresh profile.
    mediaDevices: {
      enumerateDevices: () => Promise.resolve(["audioinput", "videoinput", "audiooutput"].map(kind => ({ kind, label: "", deviceId: "", groupId: "" }))),
      getUserMedia: () => Promise.reject(new DOMException("Permission denied", "NotAllowedError")),
      addEventListener() {}, removeEventListener() {},
    },
  };
  if (chromium) Object.assign(nav, {
    // Rounded down to a power of two and capped at 8, as Chromium does.
    deviceMemory: Math.min(8, 2 ** Math.floor(Math.log2(dev.ram_gb))),
    userAgentData: make(NavigatorUAData),
    connection: { ...(os === "android" && { type: "wifi" }), effectiveType: "4g", rtt: 50, downlink: 10, saveData: false, addEventListener() {}, removeEventListener() {} },
  });
  if (brave) Object.assign(nav, { globalPrivacyControl: true, brave: { isBrave: () => Promise.resolve(true) } });
  if (firefox) Object.assign(nav, {
    oscpu: { windows: "Windows NT 10.0; Win64; x64", macos: "Intel Mac OS X 10.15", linux: "Linux x86_64" }[os] || "Linux aarch64",
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
  if (opera) G.opr = { addons: {} };
  G.screen = {
    width: sw, height: sh, availWidth: sw, availHeight: availH, colorDepth: 24, pixelDepth: 24,
    orientation: { type: sw > sh ? "landscape-primary" : "portrait-primary", angle: 0, onchange: null, addEventListener() {}, removeEventListener() {} },
  };
  Object.assign(G, {
    innerWidth: vw, innerHeight: vh, outerWidth: sw, outerHeight: desktop ? availH : vh,
    devicePixelRatio: dev.dpr, scrollX: 0, scrollY: 0, pageXOffset: 0, pageYOffset: 0,
    scrollTo() {}, scroll() {}, scrollBy() {}, focus() {}, blur() {}, print() {},
    alert() {}, confirm: () => false, prompt: () => null, open: () => null, close() {},
    isSecureContext: loc.protocol === "https:" || /^127\.|^\[::1\]$|(^|\.)localhost$/.test(loc.hostname),
  });
  // Touch devices expose ontouch* handlers; their presence is the classic mobile check.
  if (dev.touch && !ipad) for (const t of [G, document]) for (const e of ["touchstart", "touchend", "touchmove", "touchcancel"]) t[`on${e}`] = null;
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
  for (const m of ["scrollIntoView", "focus", "blur"]) E[m] ??= function () {};

  // --- forms & activation behaviour ----------------------------------------
  // linkedom has no form semantics: checked isn't reflected, no .form / .elements /
  // requestSubmit(), and click() runs no default action. Browsers do all of that.
  // Patch the prototypes parsed elements really have (linkedom's exported classes are facades).
  const proto = tag => Object.getPrototypeOf(document.createElement(tag));
  const [InputP, SelectP, TextAreaP, ButtonP, FormP] = ["input", "select", "textarea", "button", "form"].map(proto);
  const def = (p, k, d) => Object.defineProperty(p, k, { configurable: true, ...d });
  def(InputP, "checked", { get() { return this.hasAttribute("checked"); }, set(v) { v ? this.setAttribute("checked", "") : this.removeAttribute("checked"); } });
  const optionsOf = s => [...s.querySelectorAll("option")];
  const optValue = o => o.getAttribute("value") ?? o.textContent;
  def(SelectP, "selectedIndex", {
    get() { const o = optionsOf(this), i = o.findIndex(x => x.hasAttribute("selected")); return i >= 0 ? i : o.length && !this.multiple ? 0 : -1; },
    set(i) { optionsOf(this).forEach((o, j) => j === i ? o.setAttribute("selected", "") : o.removeAttribute("selected")); },
  });
  def(SelectP, "value", {
    get() { const o = optionsOf(this)[this.selectedIndex]; return o ? optValue(o) : ""; },
    set(v) { this.selectedIndex = optionsOf(this).findIndex(o => optValue(o) === String(v)); },
  });
  def(SelectP, "options", { get() { return optionsOf(this); } });
  for (const p of [InputP, SelectP, TextAreaP, ButtonP]) def(p, "form", { get() { return this.closest("form"); } });
  const controls = f => [...f.querySelectorAll("input, select, textarea, button")];
  def(FormP, "elements", { get() { return controls(this); } });
  // Successful controls -> name/value pairs (HTML "constructing the entry list").
  const formEntries = (form, submitter) => {
    const out = [];
    for (const el of controls(form)) {
      const name = el.getAttribute("name"), type = (el.getAttribute("type") || "").toLowerCase();
      if (!name || el.hasAttribute("disabled") || el.localName === "button" || ["submit", "button", "image", "reset", "file"].includes(type)) continue;
      if ((type === "checkbox" || type === "radio") && !el.checked) continue;
      if (el.localName === "select") { for (const o of optionsOf(el).filter((o, i) => o.hasAttribute("selected") || (!el.multiple && i === el.selectedIndex))) out.push([name, optValue(o)]); continue; }
      out.push([name, (type === "checkbox" || type === "radio") ? (el.getAttribute("value") ?? "on") : String(el.value ?? "")]);
    }
    if (submitter?.getAttribute("name")) out.push([submitter.getAttribute("name"), submitter.getAttribute("value") || ""]);
    return out;
  };
  const submitForm = (form, submitter) => {
    const attr = k => submitter?.getAttribute(`form${k}`) || form.getAttribute(k);
    const method = (attr("method") || "get").toLowerCase();
    const action = new URL(attr("action") || loc.href, loc);
    const body = formEntries(form, submitter).map(kv => kv.map(encodeURIComponent).join("=")).join("&").replace(/%20/g, "+");
    // ponytail: multipart forms are sent urlencoded and file inputs are skipped.
    if (method === "post") requestNav(action.href, "POST", body, "application/x-www-form-urlencoded");
    else { action.search = body; requestNav(action.href); }
  };
  class SubmitEvent extends L.Event {
    constructor(type, init = {}) { super(type, init); this.submitter = init.submitter ?? null; }
  }
  G.SubmitEvent = SubmitEvent;
  FormP.submit = function () { submitForm(this, null); }; // no submit event, like browsers
  FormP.requestSubmit = function (submitter = null) {
    if (this.dispatchEvent(new SubmitEvent("submit", { bubbles: true, cancelable: true, submitter }))) submitForm(this, submitter);
  };
  FormP.reset = function () {};
  const fireInput = el => {
    el.dispatchEvent(new L.Event("input", { bubbles: true }));
    el.dispatchEvent(new L.Event("change", { bubbles: true }));
  };
  // click(): events first, then the default action unless a handler cancelled it.
  const click = function () {
    const type = (this.getAttribute?.("type") || "").toLowerCase(), toggles = this.localName === "input" && (type === "checkbox" || type === "radio");
    const was = toggles ? this.checked : undefined;
    if (toggles) {
      if (type === "radio") for (const r of (this.form || document).querySelectorAll(`input[type=radio][name="${this.getAttribute("name")}"]`)) r.checked = false;
      this.checked = type === "radio" ? true : !was;
    }
    const ok = this.dispatchEvent(new (G.MouseEvent || L.Event)("click", { bubbles: true, cancelable: true, detail: 1, button: 0 }));
    if (toggles) { if (!ok) this.checked = was; else if (this.checked !== was) fireInput(this); return; }
    if (!ok) return;
    const a = this.closest?.("a[href], area[href]");
    if (a) return requestNav(a.getAttribute("href"));
    const btn = this.closest?.("button, input[type=submit], input[type=image]");
    if (btn && !btn.hasAttribute("disabled")) {
      const t = (btn.getAttribute("type") || "submit").toLowerCase();
      if (t === "submit" || t === "image") btn.form?.requestSubmit(btn);
      return;
    }
    const label = this.closest?.("label");
    const target = label && (label.getAttribute("for") ? document.getElementById(label.getAttribute("for")) : label.querySelector("input, select, textarea"));
    if (target && target !== this) target.click();
  };
  E.click = click;
  proto("div").click = click;

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
  };
  // SubtleCrypto via Rust (src/subtle.rs): digest, HMAC, AES-GCM, RSA-OAEP, ECDSA P-256, PBKDF2.
  // Key material lives in a WeakMap: bytes for secret keys, canonical JWK JSON for RSA/EC keys.
  // [SecureContext] in browsers: on http pages crypto.subtle, SubtleCrypto and CryptoKey don't exist.
  // ponytail: no AES-CBC/CTR/KW, RSA-PSS/RSASSA-PKCS1, ECDH, P-384/521 or HKDF; add per algorithm when a site calls it.
  if (G.isSecureContext) {
    const material = new WeakMap();
    const u8 = d => {
      if (ArrayBuffer.isView(d)) return new Uint8Array(d.buffer, d.byteOffset, d.byteLength);
      if (d instanceof ArrayBuffer) return new Uint8Array(d);
      throw new TypeError("parameter is not of type '(ArrayBuffer or ArrayBufferView)'.");
    };
    const none = new Uint8Array(0);
    const random = n => G.crypto.getRandomValues(new Uint8Array(n));
    const b64u = b => btoa(String.fromCharCode(...b)).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
    const unb64u = s => Uint8Array.from(atob(s.replace(/-/g, "+").replace(/_/g, "/") + "===".slice((s.length + 3) % 4)), c => c.charCodeAt(0));
    const err = (name, msg) => new DOMException(msg, name);
    const unsupported = () => err("NotSupportedError", "Algorithm: Unrecognized name");
    const NAMES = ["SHA-1", "SHA-256", "SHA-384", "SHA-512", "HMAC", "AES-GCM", "RSA-OAEP", "ECDSA", "PBKDF2"];
    const norm = a => (typeof a === "string" ? { name: a } : a) ?? {};
    const canon = (a, ok) => {
      const n = String(norm(a).name).toUpperCase();
      if (!ok.includes(n)) throw unsupported();
      return n;
    };
    const hashOf = a => canon(a, NAMES.slice(0, 4));
    const blockBits = h => h === "SHA-384" || h === "SHA-512" ? 1024 : 512;
    const KTY = { "RSA-OAEP": "RSA", ECDSA: "EC" };
    const USAGES = {
      HMAC: { secret: ["sign", "verify"] },
      "AES-GCM": { secret: ["encrypt", "decrypt", "wrapKey", "unwrapKey"] },
      PBKDF2: { secret: ["deriveBits", "deriveKey"] },
      "RSA-OAEP": { public: ["encrypt", "wrapKey"], private: ["decrypt", "unwrapKey"] },
      ECDSA: { public: ["verify"], private: ["sign"] },
    };
    const ORDER = ["encrypt", "decrypt", "sign", "verify", "deriveKey", "deriveBits", "wrapKey", "unwrapKey"];
    const CryptoKey = iface("CryptoKey");
    const makeKey = (type, extractable, algorithm, usages, data) => {
      const allowed = USAGES[algorithm.name][type];
      if (usages.some(u => !allowed.includes(u))) throw err("SyntaxError", "Cannot create a key using the specified key usages.");
      if (!usages.length && type !== "public") throw err("SyntaxError", "Usages cannot be empty when creating a key.");
      const key = Object.create(CryptoKey.prototype);
      Object.defineProperties(key, {
        type: { value: type }, extractable: { value: type === "public" || !!extractable },
        algorithm: { value: algorithm }, usages: { value: ORDER.filter(u => usages.includes(u)) },
      });
      material.set(key, data);
      return key;
    };
    const use = (key, name, usage) => {
      if (!(key instanceof CryptoKey)) throw new TypeError("parameter is not of type 'CryptoKey'.");
      if (key.algorithm.name !== name) throw err("InvalidAccessError", "key.algorithm does not match that of operation");
      if (!key.usages.includes(usage)) throw err("InvalidAccessError", "key.usages does not permit this operation");
      return material.get(key);
    };
    const secretKey = (name, alg, raw, extractable, usages) => {
      let algorithm = { name };
      if (name === "HMAC") {
        const bits = raw.length * 8, length = alg.length ?? bits;
        if (!length || length > bits || length <= bits - 8)
          throw err("DataError", "The optional HMAC key length must be shorter than the key data, and by less than 8 bits.");
        algorithm = { name, hash: { name: hashOf(alg.hash) }, length };
      } else if (name === "AES-GCM") {
        if (raw.length !== 16 && raw.length !== 32) throw err("DataError", "AES key data must be 128 or 256 bits");
        algorithm = { name, length: raw.length * 8 };
      } else if (extractable) throw err("SyntaxError", "PBKDF2 keys must not be extractable");
      return makeKey("secret", extractable, algorithm, usages, raw);
    };
    const asymKey = (name, alg, jwk, extractable, usages) => {
      const { n, e, d } = JSON.parse(jwk);
      const algorithm = name === "ECDSA" ? { name, namedCurve: "P-256" }
        : { name, modulusLength: unb64u(n).length * 8, publicExponent: unb64u(e), hash: { name: hashOf(alg.hash) } };
      return makeKey(d ? "private" : "public", extractable, algorithm, usages, jwk);
    };
    const publicOf = jwk => {
      const { kty, n, e, crv, x, y } = JSON.parse(jwk);
      return JSON.stringify(kty === "RSA" ? { kty, n, e } : { kty, crv, x, y });
    };
    const checkCurve = (name, alg) => { if (name === "ECDSA" && alg.namedCurve !== "P-256") throw unsupported(); };
    const badFormat = () => err("NotSupportedError", "Unsupported key format for algorithm");
    const cipher = (alg, key, data, encrypt, usage) => {
      const d = u8(data), name = canon(alg, ["AES-GCM", "RSA-OAEP"]);
      alg = norm(alg);
      const m = use(key, name, usage);
      let out;
      if (name === "AES-GCM") {
        if ((alg.tagLength ?? 128) !== 128) throw unsupported();
        out = __ws_aes_gcm(encrypt, m, u8(alg.iv), u8(alg.additionalData ?? none), d);
      } else out = __ws_rsa_oaep(encrypt, key.algorithm.hash.name, m, u8(alg.label ?? none), d);
      if (!out) throw err("OperationError", "");
      return out;
    };
    const pbkdf2 = (alg, key, length, usage) => {
      const name = canon(alg, ["PBKDF2"]);
      alg = norm(alg);
      const m = use(key, name, usage);
      if (length == null || length % 8) throw err("OperationError", "length must be a multiple of 8 bits");
      if (!alg.iterations) throw err("OperationError", "PBKDF2 requires iterations > 0");
      return __ws_pbkdf2(hashOf(alg.hash), m, u8(alg.salt), alg.iterations, length / 8);
    };
    const S = {
      async digest(alg, data) {
        const d = u8(data);
        return __ws_digest(hashOf(alg), d);
      },
      async importKey(format, data, alg, extractable, usages) {
        const name = canon(alg, NAMES.slice(4));
        alg = norm(alg);
        usages = [...usages];
        if (!KTY[name]) {
          let raw;
          if (format === "raw") raw = u8(data).slice();
          else if (format === "jwk" && name !== "PBKDF2") {
            if (data?.kty !== "oct") throw err("DataError", "The JWK \"kty\" member was not \"oct\"");
            try { raw = unb64u(data.k); } catch { throw err("DataError", "The JWK \"k\" member was not valid base64url"); }
          } else throw badFormat();
          return secretKey(name, alg, raw, extractable, usages);
        }
        checkCurve(name, alg);
        if (format === "raw" ? name !== "ECDSA" : !["spki", "pkcs8", "jwk"].includes(format)) throw badFormat();
        const jwk = __ws_jwk_from(KTY[name], format, format === "jwk" ? new TextEncoder().encode(JSON.stringify(data)) : u8(data));
        if (!jwk) throw err("DataError", "Invalid keyData");
        return asymKey(name, alg, jwk, extractable, usages);
      },
      async exportKey(format, key) {
        if (!(key instanceof CryptoKey)) throw new TypeError("parameter 2 is not of type 'CryptoKey'.");
        if (!key.extractable) throw err("InvalidAccessError", "key is not extractable");
        const m = material.get(key), a = key.algorithm;
        if (format === "jwk") {
          const jwk = typeof m === "string" ? JSON.parse(m) : { kty: "oct", k: b64u(m) };
          const alg = a.name === "HMAC" ? "HS" + a.hash.name.slice(4) : a.name === "AES-GCM" ? `A${a.length}GCM`
            : a.name === "RSA-OAEP" ? "RSA-OAEP" + (a.hash.name === "SHA-1" ? "" : "-" + a.hash.name.slice(4)) : undefined;
          return Object.assign(jwk, alg && { alg }, { ext: true, key_ops: [...key.usages] });
        }
        if (typeof m !== "string") {
          if (format !== "raw") throw badFormat();
          return m.slice().buffer;
        }
        if (format === "raw" ? a.name !== "ECDSA" : !["spki", "pkcs8"].includes(format)) throw badFormat();
        if ((format === "pkcs8") !== (key.type === "private")) throw err("InvalidAccessError", "key type does not match the export format");
        return __ws_jwk_to(format, m);
      },
      async generateKey(alg, extractable, usages) {
        const name = canon(alg, NAMES.slice(4, 8));
        alg = norm(alg);
        usages = [...usages];
        if (name === "HMAC") {
          const length = alg.length ?? blockBits(hashOf(alg.hash));
          return secretKey(name, { ...alg, length }, random(Math.ceil(length / 8)), extractable, usages);
        }
        if (name === "AES-GCM") {
          if (alg.length !== 128 && alg.length !== 256) throw err("OperationError", "AES key length must be 128 or 256 bits");
          return secretKey(name, alg, random(alg.length / 8), extractable, usages);
        }
        checkCurve(name, alg);
        if (name === "RSA-OAEP") hashOf(alg.hash);
        const { public: pub, private: priv } = USAGES[name];
        if (usages.some(u => !pub.includes(u) && !priv.includes(u))) throw err("SyntaxError", "Cannot create a key using the specified key usages.");
        const jwk = __ws_jwk_gen(KTY[name], alg.modulusLength | 0, u8(alg.publicExponent ?? new Uint8Array([1, 0, 1])));
        if (!jwk) throw err("OperationError", "Failed to generate key");
        return {
          publicKey: asymKey(name, alg, publicOf(jwk), true, usages.filter(u => pub.includes(u))),
          privateKey: asymKey(name, alg, jwk, extractable, usages.filter(u => priv.includes(u))),
        };
      },
      async encrypt(alg, key, data) { return cipher(alg, key, data, true, "encrypt"); },
      async decrypt(alg, key, data) { return cipher(alg, key, data, false, "decrypt"); },
      async sign(alg, key, data) {
        const d = u8(data), name = canon(alg, ["HMAC", "ECDSA"]);
        const m = use(key, name, "sign");
        return name === "HMAC" ? __ws_hmac(key.algorithm.hash.name, m, d) : __ws_ecdsa_sign(hashOf(norm(alg).hash), m, d);
      },
      async verify(alg, key, sig, data) {
        const s = u8(sig), d = u8(data), name = canon(alg, ["HMAC", "ECDSA"]);
        const m = use(key, name, "verify");
        if (name === "ECDSA") return __ws_ecdsa_verify(hashOf(norm(alg).hash), m, d, s);
        const want = new Uint8Array(__ws_hmac(key.algorithm.hash.name, m, d));
        return want.length === s.length && want.every((b, i) => b === s[i]);
      },
      async deriveBits(alg, key, length) { return pbkdf2(alg, key, length, "deriveBits"); },
      async deriveKey(alg, key, derived, extractable, usages) {
        const name = canon(derived, ["HMAC", "AES-GCM"]), d = norm(derived);
        const length = name === "HMAC" ? d.length ?? blockBits(hashOf(d.hash)) : d.length;
        return S.importKey("raw", pbkdf2(alg, key, length, "deriveKey"), derived, extractable, usages);
      },
      async wrapKey(format, key, wrappingKey, wrapAlg) {
        const data = await S.exportKey(format, key);
        return cipher(wrapAlg, wrappingKey, format === "jwk" ? new TextEncoder().encode(JSON.stringify(data)) : data, true, "wrapKey");
      },
      async unwrapKey(format, wrapped, unwrappingKey, unwrapAlg, keyAlg, extractable, usages) {
        const data = cipher(unwrapAlg, unwrappingKey, wrapped, false, "unwrapKey");
        return S.importKey(format, format === "jwk" ? JSON.parse(new TextDecoder().decode(data)) : data, keyAlg, extractable, usages);
      },
    };
    const SubtleCrypto = iface("SubtleCrypto", S);
    G.crypto.subtle = Object.create(SubtleCrypto.prototype);
  }
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

  // Timers run up to `horizon` virtual ms; actions push it forward (__ws_extend).
  let horizon = cfg.maxVirtualMs;
  G.__ws_extend = ms => { horizon = Math.max(horizon, now + ms); };
  // Runs the next due timer. Returns false when nothing is left within the virtual time budget.
  G.__ws_tick = () => {
    if (G.__ws_nav) return false; // the document is being replaced
    let next = null;
    for (const [id, t] of timers) if (!next || t.at < next[1].at) next = [id, t];
    if (!next || next[1].at > horizon) return false;
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

  // --- actions: scrape `actions`, run by Rust one at a time between event-loop turns ---
  // Selectors are CSS, or "text=Sign in" for the clickable thing (else any element) with that text.
  const find = sel => {
    if (!sel.startsWith("text=")) return document.querySelector(sel);
    const want = sel.slice(5).trim().toLowerCase();
    const text = el => (el.localName === "input" ? el.getAttribute("value") || "" : el.textContent || "").trim().toLowerCase();
    for (const pool of ["a, button, input[type=submit], input[type=button], label, summary, option, [role=button], [role=link], [role=tab], [onclick]", "*"]) {
      const els = [...document.querySelectorAll(pool)];
      const hit = els.find(el => text(el) === want)
        || els.filter(el => text(el).includes(want)).sort((a, b) => text(a).length - text(b).length)[0];
      if (hit) return hit;
    }
    return null;
  };
  const must = sel => { const el = find(String(sel)); if (!el) throw new Error(`no element matches ${sel}`); return el; };
  // Frameworks (React...) shadow `value` on the instance; the prototype setter is what they observe.
  const setValue = (el, v) => {
    let p = Object.getPrototypeOf(el), d;
    while (p && !(d = Object.getOwnPropertyDescriptor(p, "value"))) p = Object.getPrototypeOf(p);
    d?.set ? d.set.call(el, v) : (el.value = v);
  };
  const key = (el, type, k) => el.dispatchEvent(new (G.KeyboardEvent || L.Event)(type,
    { key: k, code: k.length === 1 ? `Key${k.toUpperCase()}` : k, bubbles: true, cancelable: true }));
  let focused = null;
  const focus = el => { focused = el; el.dispatchEvent(new L.Event("focus")); el.dispatchEvent(new L.Event("focusin", { bubbles: true })); };
  const actions = {
    click: a => { const el = must(a.click); focus(el); el.click(); },
    type: a => {
      const el = must(a.type);
      focus(el);
      if (a.clear !== false) setValue(el, "");
      for (const ch of String(a.text ?? "")) {
        if (!key(el, "keydown", ch)) continue;
        key(el, "keypress", ch);
        setValue(el, String(el.value ?? "") + ch);
        el.dispatchEvent(new (G.InputEvent || L.Event)("input", { bubbles: true, data: ch, inputType: "insertText" }));
        key(el, "keyup", ch);
      }
      el.dispatchEvent(new L.Event("change", { bubbles: true }));
    },
    select: a => {
      const el = must(a.select);
      el.value = String(a.value);
      if (el.selectedIndex < 0) throw new Error(`no option with value ${a.value}`);
      fireInput(el);
    },
    check: a => { const el = must(a.check); if (!el.checked) el.click(); },
    uncheck: a => { const el = must(a.uncheck); if (el.checked) el.click(); },
    submit: a => {
      const el = must(a.submit), form = el.localName === "form" ? el : el.closest("form");
      if (!form) throw new Error(`${a.submit} is not in a form`);
      form.requestSubmit();
    },
    press: a => {
      const el = a.selector ? must(a.selector) : focused || document.body, k = String(a.press);
      const ok = key(el, "keydown", k);
      if (k.length === 1) key(el, "keypress", k);
      key(el, "keyup", k);
      // Enter in a text field submits its form ("implicit submission").
      const form = el.closest?.("form");
      if (ok && k === "Enter" && form && el.localName === "input") form.requestSubmit(form.querySelector("button:not([type]), button[type=submit], input[type=submit]"));
    },
    goto: a => requestNav(String(a.goto)),
    // ponytail: a returned Promise isn't awaited; read async results with a later eval.
    eval: a => { const v = (0, eval)(String(a.eval)); return v instanceof Promise ? null : JSON.parse(JSON.stringify(v ?? null)); },
    wait: a => G.__ws_extend(Number(a.wait) || 0),
    wait_for: () => {}, // Rust runs the event loop until it matches
  };
  G.__ws_act = json => {
    const a = JSON.parse(json);
    const verb = Object.keys(actions).find(k => k in a);
    if (!verb) return JSON.stringify({ ok: false, error: `unknown action; use one of ${Object.keys(actions).join(", ")}` });
    try {
      const value = actions[verb](a);
      return JSON.stringify({ ok: true, ...(verb === "eval" && { value }) });
    } catch (e) {
      return JSON.stringify({ ok: false, error: String(e?.message ?? e) });
    }
  };
  G.__ws_found = sel => { try { return !!find(sel); } catch { return false; } };

  // --- network: fetch / XHR -----------------------------------------------
  // Rust adds the browser's own headers (sec-fetch-*, referer, origin...) per dest.
  const http = (method, url, headers, body, dest = "empty") => JSON.parse(__ws_http(
    String(method || "GET").toUpperCase(), new URL(url, loc).href,
    JSON.stringify(headers), body == null ? null : String(body), dest,
  ));

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
      const r = http(req.method, req.url, Object.fromEntries(req.headers), req.body);
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
          r = http(this._m, this._u, this._h, body);
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
        const r = http("GET", url, {}, null, "script");
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
