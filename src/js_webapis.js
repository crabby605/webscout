// Web APIs beyond the DOM that QuickJS lacks. Loaded after js_prelude.js, before any page script.
// Each one either works for real (in-process) or fails the way a blocked/unsupported
// browser feature would, so page code takes its normal fallback path.
(() => {
  const G = globalThis, L = __linkedom;
  const added = [];
  const def = (name, value) => { if (!(name in G)) { G[name] = value; added.push(value); } };
  const later = fn => setTimeout(fn, 0);

  def("DOMException", class DOMException extends Error {
    constructor(message = "", name = "Error") { super(message); this.name = name; }
  });

  // --- events ---------------------------------------------------------------
  const eventClass = (name, defaults) => class extends L.Event {
    constructor(type, init = {}) {
      super(type, init);
      for (const [k, v] of Object.entries({ ...defaults, ...init })) {
        if (!(k in L.Event.prototype)) Object.defineProperty(this, k, { value: v, enumerable: true, configurable: true });
      }
    }
    static get name() { return name; }
  };
  const mouse = { clientX: 0, clientY: 0, screenX: 0, screenY: 0, pageX: 0, pageY: 0, button: 0, buttons: 0, ctrlKey: false, shiftKey: false, altKey: false, metaKey: false, relatedTarget: null };
  def("UIEvent", eventClass("UIEvent", { view: null, detail: 0 }));
  def("MouseEvent", eventClass("MouseEvent", mouse));
  def("PointerEvent", eventClass("PointerEvent", { ...mouse, pointerId: 1, pointerType: "mouse", isPrimary: true }));
  def("KeyboardEvent", eventClass("KeyboardEvent", { key: "", code: "", keyCode: 0, which: 0, repeat: false, ctrlKey: false, shiftKey: false, altKey: false, metaKey: false }));
  def("FocusEvent", eventClass("FocusEvent", { relatedTarget: null }));
  def("ErrorEvent", eventClass("ErrorEvent", { message: "", filename: "", lineno: 0, colno: 0, error: null }));
  def("PopStateEvent", eventClass("PopStateEvent", { state: null }));
  def("HashChangeEvent", eventClass("HashChangeEvent", { oldURL: "", newURL: "" }));
  def("StorageEvent", eventClass("StorageEvent", { key: null, oldValue: null, newValue: null, url: "", storageArea: null }));
  def("MessageEvent", eventClass("MessageEvent", { data: null, origin: "", lastEventId: "", source: null, ports: [] }));
  def("ProgressEvent", eventClass("ProgressEvent", { lengthComputable: false, loaded: 0, total: 0 }));

  // --- abort ----------------------------------------------------------------
  class AbortSignal extends L.EventTarget {
    constructor() { super(); this.aborted = false; this.reason = undefined; this.onabort = null; }
    throwIfAborted() { if (this.aborted) throw this.reason; }
    _abort(reason = new DOMException("signal is aborted without reason", "AbortError")) {
      if (this.aborted) return;
      this.aborted = true;
      this.reason = reason;
      const e = new L.Event("abort");
      this.dispatchEvent(e);
      if (typeof this.onabort === "function") this.onabort(e);
    }
    static abort(reason) { const s = new AbortSignal(); s._abort(reason); return s; }
    static timeout(ms) { const s = new AbortSignal(); setTimeout(() => s._abort(new DOMException("signal timed out", "TimeoutError")), ms); return s; }
    static any(signals) {
      const s = new AbortSignal();
      for (const x of signals) {
        if (x.aborted) { s._abort(x.reason); break; }
        x.addEventListener("abort", () => s._abort(x.reason));
      }
      return s;
    }
  }
  def("AbortSignal", AbortSignal);
  def("AbortController", class AbortController {
    constructor() { this.signal = new AbortSignal(); }
    abort(reason) { this.signal._abort(reason); }
  });

  // --- Blob / File / FileReader / FormData ----------------------------------
  // ponytail: Blob keeps text; binary parts go through latin1. Fine for JSON/text uploads.
  const partText = p => typeof p === "string" ? p
    : p instanceof Blob ? p._text
    : p instanceof ArrayBuffer || ArrayBuffer.isView(p) ? new TextDecoder().decode(p)
    : String(p);
  class Blob {
    constructor(parts = [], opts = {}) { this._text = parts.map(partText).join(""); this.type = String(opts.type || "").toLowerCase(); }
    get size() { return new TextEncoder().encode(this._text).length; }
    text() { return Promise.resolve(this._text); }
    arrayBuffer() { return Promise.resolve(new TextEncoder().encode(this._text).buffer); }
    slice(start, end, type) { return new Blob([this._text.slice(start, end)], { type }); }
  }
  def("Blob", Blob);
  def("File", class File extends Blob {
    constructor(parts, name, opts = {}) { super(parts, opts); this.name = String(name); this.lastModified = opts.lastModified ?? Date.now(); }
  });
  def("FileReader", class FileReader extends L.EventTarget {
    constructor() { super(); this.readyState = 0; this.result = null; this.error = null; }
    _read(blob, fn) {
      this.readyState = 1;
      later(() => {
        this.result = fn(blob);
        this.readyState = 2;
        for (const t of ["load", "loadend"]) {
          const e = new L.Event(t);
          this.dispatchEvent(e);
          if (typeof this[`on${t}`] === "function") this[`on${t}`](e);
        }
      });
    }
    readAsText(b) { this._read(b, b => b._text); }
    readAsArrayBuffer(b) { this._read(b, b => new TextEncoder().encode(b._text).buffer); }
    readAsDataURL(b) { this._read(b, b => `data:${b.type || "application/octet-stream"};base64,${btoa(unescape(encodeURIComponent(b._text)))}`); }
    abort() {}
  });
  def("FormData", class FormData {
    constructor(form) {
      this._e = [];
      if (form && form.querySelectorAll) {
        for (const el of form.querySelectorAll("input[name], select[name], textarea[name]")) {
          const t = (el.getAttribute("type") || "").toLowerCase();
          if (["submit", "button", "file", "reset"].includes(t)) continue;
          if ((t === "checkbox" || t === "radio") && !el.hasAttribute("checked")) continue;
          this.append(el.getAttribute("name"), el.value ?? el.getAttribute("value") ?? "");
        }
      }
    }
    append(k, v) { this._e.push([String(k), v instanceof Blob ? v : String(v)]); }
    set(k, v) { this.delete(k); this.append(k, v); }
    get(k) { return this._e.find(e => e[0] === k)?.[1] ?? null; }
    getAll(k) { return this._e.filter(e => e[0] === k).map(e => e[1]); }
    has(k) { return this._e.some(e => e[0] === k); }
    delete(k) { this._e = this._e.filter(e => e[0] !== k); }
    entries() { return this._e[Symbol.iterator](); }
    keys() { return this._e.map(e => e[0])[Symbol.iterator](); }
    values() { return this._e.map(e => e[1])[Symbol.iterator](); }
    forEach(cb, self) { this._e.forEach(([k, v]) => cb.call(self, v, k, this)); }
    [Symbol.iterator]() { return this.entries(); }
  });

  // fetch: honour AbortSignal and encode FormData / URLSearchParams / Blob bodies.
  const baseFetch = G.fetch;
  G.fetch = function fetch(input, init = {}) {
    const signal = init.signal || input?.signal;
    if (signal?.aborted) return Promise.reject(signal.reason);
    let { body } = init, headers = new Headers(init.headers || input?.headers);
    // ponytail: FormData goes out urlencoded, not multipart; file uploads would need real multipart.
    if (body instanceof FormData || body instanceof URLSearchParams) {
      body = new URLSearchParams([...body].map(([k, v]) => [k, partText(v)])).toString();
      if (!headers.has("content-type")) headers.set("content-type", "application/x-www-form-urlencoded;charset=UTF-8");
    } else if (body instanceof Blob) {
      if (body.type && !headers.has("content-type")) headers.set("content-type", body.type);
      body = body._text;
    }
    const p = baseFetch(input, { ...init, body, headers });
    return signal ? Promise.race([p, new Promise((_, rej) => signal.addEventListener("abort", () => rej(signal.reason)))]) : p;
  };
  added.push(G.fetch);

  // --- messaging --------------------------------------------------------------
  class MessagePort extends L.EventTarget {
    constructor() { super(); this.onmessage = null; this._other = null; }
    postMessage(data) {
      const to = this._other;
      later(() => {
        const e = new MessageEvent("message", { data: structuredClone(data) });
        to.dispatchEvent(e);
        if (typeof to.onmessage === "function") to.onmessage(e);
      });
    }
    start() {}
    close() {}
  }
  def("MessagePort", MessagePort);
  def("MessageChannel", class MessageChannel {
    constructor() { this.port1 = new MessagePort(); this.port2 = new MessagePort(); this.port1._other = this.port2; this.port2._other = this.port1; }
  });
  G.postMessage = (data, _origin) => later(() => {
    const e = new MessageEvent("message", { data: structuredClone(data), origin: location.origin, source: G });
    G.dispatchEvent(e);
    if (typeof G.onmessage === "function") G.onmessage(e);
  });
  def("BroadcastChannel", class BroadcastChannel extends L.EventTarget {
    constructor(name) { super(); this.name = name; this.onmessage = null; }
    postMessage() {}
    close() {}
  });

  // Network-push and threads aren't supported: they construct, then fail like a blocked connection.
  const failing = (name, closeEvent) => class extends L.EventTarget {
    constructor(url) {
      super();
      this.url = String(url);
      this.readyState = 0;
      __ws_log("warn", `${name} not supported: ${this.url}`);
      later(() => {
        this.readyState = closeEvent ? 3 : 2;
        for (const t of closeEvent ? ["error", "close"] : ["error"]) {
          const e = new L.Event(t);
          this.dispatchEvent(e);
          if (typeof this[`on${t}`] === "function") this[`on${t}`](e);
        }
      });
    }
    send() {}
    close() {}
    postMessage() {}
    terminate() {}
    static get name() { return name; }
  };
  def("WebSocket", Object.assign(failing("WebSocket", true), { CONNECTING: 0, OPEN: 1, CLOSING: 2, CLOSED: 3 }));
  def("EventSource", Object.assign(failing("EventSource", false), { CONNECTING: 0, OPEN: 1, CLOSED: 2 }));
  def("Worker", failing("Worker", false));
  def("SharedWorker", failing("SharedWorker", false));
  def("Notification", Object.assign(function Notification() {}, { permission: "default", requestPermission: () => Promise.resolve("denied") }));

  // --- geometry / layout (there is no layout engine: everything measures 0) -----
  class DOMRectReadOnly {
    constructor(x = 0, y = 0, width = 0, height = 0) { Object.assign(this, { x, y, width, height }); }
    get top() { return Math.min(this.y, this.y + this.height); }
    get left() { return Math.min(this.x, this.x + this.width); }
    get right() { return Math.max(this.x, this.x + this.width); }
    get bottom() { return Math.max(this.y, this.y + this.height); }
    toJSON() { const { x, y, width, height, top, left, right, bottom } = this; return { x, y, width, height, top, left, right, bottom }; }
    static fromRect(r = {}) { return new this(r.x, r.y, r.width, r.height); }
  }
  def("DOMRectReadOnly", DOMRectReadOnly);
  def("DOMRect", class DOMRect extends DOMRectReadOnly {});
  def("DOMMatrix", class DOMMatrix {
    constructor() { Object.assign(this, { a: 1, b: 0, c: 0, d: 1, e: 0, f: 0, is2D: true, isIdentity: true }); }
    multiply() { return this; } translate() { return this; } scale() { return this; } rotate() { return this; } inverse() { return this; }
  });
  L.Element.prototype.getBoundingClientRect = () => new DOMRect();
  const zero = ["offsetWidth", "offsetHeight", "offsetTop", "offsetLeft", "clientWidth", "clientHeight", "clientTop", "clientLeft", "scrollWidth", "scrollHeight"];
  for (const k of zero) if (!(k in L.Element.prototype)) Object.defineProperty(L.Element.prototype, k, { get: () => 0, configurable: true });
  for (const k of ["scrollTop", "scrollLeft"]) {
    if (!(k in L.Element.prototype)) Object.defineProperty(L.Element.prototype, k, { get() { return this[`_${k}`] || 0; }, set(v) { this[`_${k}`] = Number(v) || 0; }, configurable: true });
  }
  if (!("offsetParent" in L.Element.prototype)) Object.defineProperty(L.Element.prototype, "offsetParent", { get() { return this.parentElement; }, configurable: true });
  document.elementFromPoint = () => null;
  document.elementsFromPoint = () => [];
  L.Element.prototype.animate ??= function () {
    const a = { playState: "finished", finished: Promise.resolve(), onfinish: null, cancel() {}, play() {}, pause() {}, finish() {}, reverse() {}, addEventListener() {}, removeEventListener() {} };
    a.finished.then(() => { a.finished = Promise.resolve(a); typeof a.onfinish === "function" && a.onfinish(); });
    return a;
  };
  L.Element.prototype.getAnimations ??= () => [];

  // --- canvas: a context that accepts every call and draws nothing --------------
  const noop = () => {};
  const ctx2d = canvas => new Proxy({ canvas }, {
    get(t, k) {
      if (k in t) return t[k];
      if (k === "measureText") return text => ({ width: String(text).length * 7, actualBoundingBoxAscent: 8, actualBoundingBoxDescent: 2 });
      if (k === "getImageData" || k === "createImageData") return (_x, _y, w = 1, h = 1) => ({ width: w, height: h, data: new Uint8ClampedArray(Math.max(0, w * h * 4)) });
      if (k === "getTransform") return () => new DOMMatrix();
      if (k === "getLineDash") return () => [];
      if (k === "isPointInPath" || k === "isPointInStroke") return () => false;
      if (k.startsWith?.("create")) return () => ({ addColorStop: noop });
      return noop;
    },
    set(t, k, v) { t[k] = v; return true; },
  });
  def("CanvasRenderingContext2D", function CanvasRenderingContext2D() {});
  const canvasProto = (L.HTMLCanvasElement || L.HTMLElement).prototype;
  // ponytail: 2D only; WebGL returns null (pages fall back or show "unsupported").
  canvasProto.getContext = function (type) { return type === "2d" ? (this._ctx ||= ctx2d(this)) : null; };
  canvasProto.toDataURL = () => "data:,";
  canvasProto.toBlob = cb => later(() => cb(null));

  // --- misc -------------------------------------------------------------------
  def("XMLSerializer", class XMLSerializer { serializeToString(node) { return node.outerHTML ?? node.textContent ?? ""; } });
  const sampleRange = document.createRange?.();
  if (sampleRange) def("Range", sampleRange.constructor);
  const sampleWalker = document.createTreeWalker?.(document);
  if (sampleWalker) def("TreeWalker", sampleWalker.constructor);
  const selection = {
    rangeCount: 0, isCollapsed: true, type: "None", anchorNode: null, focusNode: null,
    addRange() {}, removeAllRanges() {}, removeRange() {}, collapse() {}, extend() {}, selectAllChildren() {},
    getRangeAt() { throw new DOMException("index out of range", "IndexSizeError"); }, toString: () => "",
  };
  G.getSelection = document.getSelection = () => selection;
  def("CSS", {
    supports: () => false,
    escape: s => String(s).replace(/([\0-\x1f\x7f]|^-?\d)|^-$|[^\x80-￿\w-]/g, (m, ctrl) => ctrl ? `\\${m.charCodeAt(0).toString(16)} ` : `\\${m}`),
  });
  def("CSSStyleSheet", class CSSStyleSheet {
    constructor() { this.cssRules = []; }
    insertRule(rule, i = 0) { this.cssRules.splice(i, 0, { cssText: rule }); return i; }
    deleteRule(i) { this.cssRules.splice(i, 1); }
    replaceSync(text) { this.cssRules = [{ cssText: text }]; }
    replace(text) { this.replaceSync(text); return Promise.resolve(this); }
  });
  if (!("adoptedStyleSheets" in document)) document.adoptedStyleSheets = [];
  for (const n of ["SVGAnimatedString", "SVGAnimatedLength", "SVGAnimatedNumber", "SVGLength", "SVGGraphicsElement", "SVGSVGElement"]) {
    def(n, (L[n] || { [n]: class {} }[n]));
  }
  G.MediaQueryList = class MediaQueryList extends L.EventTarget {
    constructor(media) { super(); this.media = media; this.matches = false; this.onchange = null; }
    addListener() {}
    removeListener() {}
  };
  G.matchMedia = media => new MediaQueryList(String(media));
  added.push(G.matchMedia);
  def("setImmediate", (fn, ...a) => setTimeout(fn, 0, ...a));
  def("clearImmediate", id => clearTimeout(id));
  def("reportError", e => __ws_log("error", `reportError: ${e instanceof Error ? `${e}\n${e.stack || ""}` : e}`));

  // --- Intl (QuickJS has none) ------------------------------------------------
  // ponytail: en-US-only shim covering the common formatters. Swap for @formatjs polyfills
  // (bundled via vendor/) if non-English formatting matters.
  if (!G.Intl) {
    const locale = () => (__ws_config.languages[0] || "en-US");
    const group = s => s.replace(/\B(?=(\d{3})+(?!\d))/g, ",");
    const currencySymbols = { USD: "$", EUR: "€", GBP: "£", JPY: "¥", INR: "₹", CNY: "¥", KRW: "₩", CAD: "CA$", AUD: "A$" };
    class NumberFormat {
      constructor(_l, o = {}) { this.o = o; }
      format(n) {
        const o = this.o;
        n = Number(n);
        if (!Number.isFinite(n)) return Number.isNaN(n) ? "NaN" : (n < 0 ? "-∞" : "∞");
        let suffix = "";
        if (o.style === "percent") n *= 100;
        if (o.notation === "compact") {
          for (const [v, s] of [[1e12, "T"], [1e9, "B"], [1e6, "M"], [1e3, "K"]]) {
            if (Math.abs(n) >= v) { n /= v; suffix = s; break; }
          }
        }
        const isMoney = o.style === "currency";
        const min = o.minimumFractionDigits ?? (isMoney ? 2 : 0);
        const max = Math.max(min, o.maximumFractionDigits ?? (isMoney ? 2 : o.style === "percent" || suffix ? (suffix ? 1 : 0) : 3));
        let [int, frac = ""] = Math.abs(n).toFixed(max).split(".");
        frac = frac.replace(/0+$/, "").padEnd(min, "0");
        if (o.useGrouping !== false) int = group(int);
        let s = frac ? `${int}.${frac}` : int;
        s += suffix;
        if (o.style === "percent") s += "%";
        if (isMoney) s = (currencySymbols[o.currency] ?? `${o.currency} `) + s;
        if (o.style === "unit" && o.unit) s += ` ${o.unit}`;
        return (n < 0 ? "-" : "") + s;
      }
      formatToParts(n) { return [{ type: "literal", value: this.format(n) }]; }
      resolvedOptions() { return { locale: locale(), numberingSystem: "latn", style: "decimal", ...this.o }; }
      static supportedLocalesOf(l) { return [].concat(l || []); }
    }
    const MONTHS = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];
    const DAYS = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];
    class DateTimeFormat {
      constructor(_l, o = {}) {
        const has = ["weekday", "year", "month", "day", "hour", "minute", "second"].some(k => o[k]);
        this.o = has || o.dateStyle || o.timeStyle ? o : { year: "numeric", month: "numeric", day: "numeric", ...o };
        if (o.dateStyle) Object.assign(this.o, { year: "numeric", month: { full: "long", long: "long", medium: "short", short: "numeric" }[o.dateStyle], day: "numeric", weekday: o.dateStyle === "full" ? "long" : undefined });
        if (o.timeStyle) Object.assign(this.o, { hour: "numeric", minute: "2-digit", second: ["short"].includes(o.timeStyle) ? undefined : "2-digit" });
      }
      formatToParts(d = Date.now()) {
        d = new Date(d);
        const o = this.o, utc = o.timeZone === "UTC";
        const get = k => utc ? d[`getUTC${k}`]() : d[`get${k}`]();
        const parts = [], lit = v => parts.push({ type: "literal", value: v });
        const pad = (n, style) => style === "2-digit" ? String(n).padStart(2, "0").slice(-2) : String(n);
        if (o.weekday) { const w = DAYS[get("Day")]; parts.push({ type: "weekday", value: o.weekday === "long" ? w : w.slice(0, 3) }); lit(", "); }
        const month = get("Month"), day = get("Date"), year = get("FullYear");
        if (o.month && !/^(numeric|2-digit)$/.test(o.month)) {
          parts.push({ type: "month", value: o.month === "long" ? MONTHS[month] : MONTHS[month].slice(0, 3) });
          if (o.day) { lit(" "); parts.push({ type: "day", value: pad(day, o.day) }); }
          if (o.year) { lit(o.day ? ", " : " "); parts.push({ type: "year", value: o.year === "2-digit" ? pad(year, "2-digit") : String(year) }); }
        } else {
          const ds = [];
          if (o.month) ds.push({ type: "month", value: pad(month + 1, o.month) });
          if (o.day) ds.push({ type: "day", value: pad(day, o.day) });
          if (o.year) ds.push({ type: "year", value: o.year === "2-digit" ? pad(year, "2-digit") : String(year) });
          ds.forEach((p, i) => { if (i) lit("/"); parts.push(p); });
        }
        if (o.hour) {
          if (parts.length && parts[parts.length - 1].type !== "literal") lit(", ");
          const h24 = get("Hours"), h12 = o.hour12 === false || o.hourCycle === "h23";
          parts.push({ type: "hour", value: pad(h12 ? h24 : (h24 % 12 || 12), h12 ? "2-digit" : o.hour) });
          if (o.minute) { lit(":"); parts.push({ type: "minute", value: pad(get("Minutes"), "2-digit") }); }
          if (o.second) { lit(":"); parts.push({ type: "second", value: pad(get("Seconds"), "2-digit") }); }
          if (!h12) { lit(" "); parts.push({ type: "dayPeriod", value: h24 < 12 ? "AM" : "PM" }); }
        }
        return parts;
      }
      format(d) { return this.formatToParts(d).map(p => p.value).join(""); }
      formatRange(a, b) { return `${this.format(a)} – ${this.format(b)}`; }
      resolvedOptions() { return { locale: locale(), calendar: "gregory", numberingSystem: "latn", timeZone: this.o.timeZone || "UTC", ...this.o }; }
      static supportedLocalesOf(l) { return [].concat(l || []); }
    }
    class PluralRules {
      constructor(_l, o = {}) { this.o = o; }
      select(n) {
        n = Math.abs(Number(n));
        if (this.o.type === "ordinal") {
          const t = n % 10, h = n % 100;
          return t === 1 && h !== 11 ? "one" : t === 2 && h !== 12 ? "two" : t === 3 && h !== 13 ? "few" : "other";
        }
        return n === 1 ? "one" : "other";
      }
      resolvedOptions() { return { locale: locale(), type: "cardinal", ...this.o, pluralCategories: ["one", "other"] }; }
      static supportedLocalesOf(l) { return [].concat(l || []); }
    }
    class Collator {
      constructor(_l, o = {}) { this.o = o; }
      compare = (a, b) => {
        a = String(a); b = String(b);
        if (this.o.sensitivity === "base" || this.o.sensitivity === "accent") { a = a.toLowerCase(); b = b.toLowerCase(); }
        if (this.o.numeric) return a.localeCompare(b, undefined, { numeric: true });
        return a < b ? -1 : a > b ? 1 : 0;
      };
      resolvedOptions() { return { locale: locale(), usage: "sort", sensitivity: "variant", ...this.o }; }
      static supportedLocalesOf(l) { return [].concat(l || []); }
    }
    class RelativeTimeFormat {
      constructor(_l, o = {}) { this.o = o; }
      format(v, unit) {
        v = Number(v);
        unit = String(unit).replace(/s$/, "");
        if (this.o.numeric === "auto") {
          const named = { day: ["yesterday", "today", "tomorrow"], year: ["last year", "this year", "next year"], month: ["last month", "this month", "next month"], week: ["last week", "this week", "next week"] }[unit];
          if (named && Math.abs(v) <= 1) return named[v + 1];
          if (v === 0 && ["second", "minute", "hour"].includes(unit)) return unit === "second" ? "now" : `this ${unit}`;
        }
        const n = Math.abs(v), label = `${new NumberFormat().format(n)} ${unit}${n === 1 ? "" : "s"}`;
        return v < 0 || Object.is(v, -0) ? `${label} ago` : `in ${label}`;
      }
      formatToParts(v, unit) { return [{ type: "literal", value: this.format(v, unit) }]; }
      resolvedOptions() { return { locale: locale(), style: "long", numeric: "always", ...this.o }; }
      static supportedLocalesOf(l) { return [].concat(l || []); }
    }
    class ListFormat {
      constructor(_l, o = {}) { this.o = o; }
      format(list) {
        const a = [...list].map(String), word = this.o.type === "disjunction" ? "or" : "and";
        if (a.length < 3) return a.join(` ${word} `);
        return `${a.slice(0, -1).join(", ")}, ${word} ${a[a.length - 1]}`;
      }
      formatToParts(list) { return [{ type: "literal", value: this.format(list) }]; }
      static supportedLocalesOf(l) { return [].concat(l || []); }
    }
    class Segmenter {
      constructor(_l, o = {}) { this.o = o; }
      segment(str) {
        str = String(str);
        const pieces = this.o.granularity === "word" ? str.split(/(\s+|[^\w\s]+)/).filter(Boolean)
          : this.o.granularity === "sentence" ? str.split(/(?<=[.!?])\s+/) : Array.from(str);
        let index = 0;
        const segs = pieces.map(segment => { const s = { segment, index, input: str, isWordLike: /\w/.test(segment) }; index += segment.length; return s; });
        return { [Symbol.iterator]: () => segs[Symbol.iterator](), containing: i => segs.find(s => i >= s.index && i < s.index + s.segment.length) };
      }
      static supportedLocalesOf(l) { return [].concat(l || []); }
    }
    class DisplayNames {
      constructor(_l, o = {}) { this.o = o; }
      of(code) { return String(code); }
      static supportedLocalesOf(l) { return [].concat(l || []); }
    }
    class Locale {
      constructor(tag) { this.baseName = String(tag); [this.language, this.region] = this.baseName.split("-"); }
      toString() { return this.baseName; }
      maximize() { return this; }
      minimize() { return this; }
    }
    G.Intl = {
      NumberFormat, DateTimeFormat, PluralRules, Collator, RelativeTimeFormat, ListFormat, Segmenter, DisplayNames, Locale,
      getCanonicalLocales: l => [].concat(l || []),
      supportedValuesOf: () => [],
    };
    for (const C of Object.values(G.Intl)) if (typeof C === "function") added.push(C);
    // Native toLocale* ignore options in QuickJS; route them through the shim.
    Number.prototype.toLocaleString = function (l, o) { return new NumberFormat(l, o).format(this); };
    Date.prototype.toLocaleDateString = function (l, o) { return new DateTimeFormat(l, o).format(this); };
    Date.prototype.toLocaleTimeString = function (l, o) { return new DateTimeFormat(l, { hour: "numeric", minute: "2-digit", second: "2-digit", ...o }).format(this); };
    Date.prototype.toLocaleString = function (l, o) { return new DateTimeFormat(l, o || { year: "numeric", month: "numeric", day: "numeric", hour: "numeric", minute: "2-digit", second: "2-digit" }).format(this); };
    added.push(Number.prototype.toLocaleString, Date.prototype.toLocaleDateString, Date.prototype.toLocaleTimeString, Date.prototype.toLocaleString);
  }

  __ws_markNative(...added, G.crypto, selection);
})();
