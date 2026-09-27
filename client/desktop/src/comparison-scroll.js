(config => {
  'use strict';
  if (window.top !== window || location.origin !== config.origin) return;

  // HTTP carries one latest position per frame without a navigation. If the
  // preview's CSP blocks that endpoint, retain the canceled-navigation bridge
  // below its Chromium/WebKit flood limit (200 navigations / 10 seconds).
  const fallbackInterval = 80;
  const requestTimeout = 250;
  const request = typeof fetch === 'function' ? fetch.bind(window) : null;
  let fast = Boolean(config.relayUrl && request);
  let enabled = config.enabled;
  let generation = 0;
  const pending = new Map();
  let suppressed = new WeakMap();
  let scheduled = false;
  let inFlight = false;
  let lastSent = -Infinity;
  let cachedContainers = [];
  let cacheTime = -Infinity;
  const root = () => document.scrollingElement || document.documentElement;
  const clamp = value => Math.min(1, Math.max(0, value || 0));
  const scrollable = el => el.scrollHeight > el.clientHeight + 1 || el.scrollWidth > el.clientWidth + 1;
  function containers(refresh = false) {
    if (refresh || performance.now() - cacheTime > 500 || cachedContainers.some(el => !el.isConnected)) {
      cachedContainers = Array.from(document.querySelectorAll('*')).filter(el => {
        if (el === root() || !scrollable(el)) return false;
        const style = getComputedStyle(el);
        return /(auto|scroll|overlay)/.test(style.overflow + style.overflowX + style.overflowY);
      });
      cacheTime = performance.now();
    }
    return cachedContainers;
  }

  function describe(el) {
    if (el === root()) return { root: true, key: '', id: '', path: [], index: -1 };
    const key = (el.getAttribute('data-comparison-scroll-key') || '').slice(0, 256);
    const id = (el.id || '').slice(0, 256);
    const path = [];
    for (let node = el; node && node !== document.documentElement; node = node.parentElement) {
      if (!node.parentElement || path.length === 24) break;
      path.unshift(Array.prototype.indexOf.call(node.parentElement.children, node));
    }
    // Retain the order fallback even when a peer lacks this pane's key/id, but
    // reuse its list instead of scanning the whole document each frame.
    let index = containers().indexOf(el);
    if (index < 0) index = containers(true).indexOf(el);
    return { root: false, key, id, path, index };
  }

  function resolve(packet) {
    if (packet.root) return root();
    if (packet.key) {
      const match = document.querySelector(`[data-comparison-scroll-key="${CSS.escape(packet.key)}"]`);
      if (match) return match;
    }
    if (packet.id) {
      const match = document.getElementById(packet.id);
      if (match) return match;
    }
    let match = document.documentElement;
    for (const index of packet.path) match = match && match.children[index];
    if (match && scrollable(match)) return match;
    return containers()[packet.index] || null;
  }

  function remember(el) {
    pending.set(el, true);
    if (pending.size > 8) pending.delete(pending.keys().next().value);
  }

  function schedule() {
    if (!enabled || scheduled || inFlight || !pending.size) return;
    scheduled = true;
    const wait = fast ? 0 : Math.max(0, fallbackInterval - (performance.now() - lastSent));
    // A trailing fallback timer already coalesces events; adding another RAF
    // after it would introduce a gratuitous extra frame of latency.
    if (wait > 0) setTimeout(flush, wait);
    else requestAnimationFrame(flush);
  }

  function flush() {
    scheduled = false;
    if (!enabled || inFlight || pending.size === 0) return;
    const elements = Array.from(pending.keys()).filter(el => el.isConnected);
    const updates = elements.map(el => ({
      ...describe(el),
      x: clamp(el.scrollLeft / Math.max(1, el.scrollWidth - el.clientWidth)),
      y: clamp(el.scrollTop / Math.max(1, el.scrollHeight - el.clientHeight)),
    }));
    pending.clear();
    if (!updates.length) return;
    lastSent = performance.now();
    if (!fast) {
      // Rust intercepts and cancels this URL before it reaches any dev server.
      location.replace(config.channel + '?payload=' + encodeURIComponent(JSON.stringify(updates)));
      return;
    }
    inFlight = true;
    const sentGeneration = generation;
    const controller = new AbortController();
    const timeout = setTimeout(() => controller.abort(), requestTimeout);
    request(config.relayUrl, {
      method: 'POST', body: JSON.stringify(updates), credentials: 'omit',
      cache: 'no-store', redirect: 'error', signal: controller.signal,
    }).then(response => {
      if (!response.ok) throw new Error('Scroll relay unavailable');
    }).catch(() => {
      // CSP, a disconnected relay or a stalled request degrades once per page.
      // Retain the latest positions; never retry a failed fast path each frame.
      fast = false;
      if (enabled && sentGeneration === generation) {
        for (const el of elements) if (el.isConnected && !pending.has(el)) remember(el);
      }
    }).finally(() => {
      clearTimeout(timeout);
      inFlight = false;
      schedule();
    });
  }

  document.addEventListener('scroll', event => {
    if (!enabled) return;
    const el = event.target === document ? root() : event.target;
    if (!(el instanceof Element)) return;
    const ignore = suppressed.get(el);
    if (ignore && (ignore.applying ||
      (Math.abs(el.scrollLeft - ignore.x) < 0.5 && Math.abs(el.scrollTop - ignore.y) < 0.5))) return;
    suppressed.delete(el);
    remember(el);
    schedule();
  }, { capture: true, passive: true });

  // Real input immediately wins over a received position. This makes switching
  // which pane the user scrolls feel immediate, even during momentum scrolling.
  for (const name of ['wheel', 'touchstart', 'pointerdown', 'keydown']) {
    document.addEventListener(name, () => { suppressed = new WeakMap(); }, { capture: true, passive: true });
  }

  Object.defineProperty(window, '__VIBESTUDIO_COMPARE_SCROLL__', {
    configurable: false,
    writable: false,
    value: Object.freeze({
      setEnabled(value) {
        if (enabled !== Boolean(value)) generation += 1;
        enabled = Boolean(value);
        if (!enabled) pending.clear();
      },
      receive(updates) {
        if (!enabled) return;
        for (const packet of updates) {
          const el = resolve(packet);
          // Do not overwrite local input that has not yet left this pane with
          // an older packet arriving from its peer.
          if (!el || pending.has(el)) continue;
          const x = clamp(packet.x) * Math.max(0, el.scrollWidth - el.clientWidth);
          const y = clamp(packet.y) * Math.max(0, el.scrollHeight - el.clientHeight);
          const applied = { x: 0, y: 0, applying: true };
          suppressed.set(el, applied);
          el.scrollTo({ left: x, top: y, behavior: 'instant' });
          // Native zoom can round the requested position by several CSS pixels.
          // Suppress the actual applied position, including delayed events.
          applied.x = el.scrollLeft;
          applied.y = el.scrollTop;
          applied.applying = false;
        }
      },
    }),
  });
})
