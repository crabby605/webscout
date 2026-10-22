// Source for vendor/dom-bundle.js, the JS DOM that src/js.rs loads into QuickJS.
// Rebuild (from vendor/): bun install && bun run build
// Web APIs QuickJS lacks; each polyfill only installs if the global is missing.
import "core-js/actual/url";
import "core-js/actual/url-search-params";
import "core-js/actual/atob";
import "core-js/actual/btoa";
import "core-js/actual/structured-clone";
import "core-js/actual/queue-microtask";
import * as linkedom from "linkedom";

globalThis.__linkedom = linkedom;
