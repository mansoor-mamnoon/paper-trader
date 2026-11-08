"use strict";
/*
 * Optional visitor analytics for the dashboard.
 *
 * Two providers are supported and either, both or neither can be turned on from
 * analytics-config.js:
 *   goatcounter  a cookie-free counter. Needs no consent banner. Shows pages, referrers,
 *                browsers, systems, screen sizes and countries.
 *   ga4          Google Analytics 4. Adds city-level location, new versus returning
 *                visitors and a live view. Loads only after a visitor agrees.
 *
 * Your own devices are excluded: open the site once on each of them with ?me=1 on the
 * end of the address (use ?me=0 to count that device again). Local previews, visitors who
 * send Do Not Track, and browsers where that switch is on are never counted.
 *
 * Neither tool exposes who a visitor is. They report places, devices and behavior, not
 * names or IP addresses.
 */
(() => {
  const cfg = window.PT_ANALYTICS || {};
  const IGNORE = "pt_ignore";
  const CONSENT = "pt_ga_consent";
  const safe = (fn, fallback) => { try { return fn(); } catch { return fallback; } };

  const params = new URLSearchParams(location.search);
  if (params.has("me")) {
    safe(() => (params.get("me") === "0" ? localStorage.removeItem(IGNORE) : localStorage.setItem(IGNORE, "1")));
  }
  const ignored = safe(() => localStorage.getItem(IGNORE) === "1", false);
  const local = ["localhost", "127.0.0.1", ""].includes(location.hostname);
  const doNotTrack = navigator.doNotTrack === "1" || window.doNotTrack === "1";

  const state = { ignored, local, doNotTrack, providers: [] };
  window.ptAnalytics = state;
  const queue = [];
  const sinks = [];
  window.ptTrack = (tab) => { queue.push({ page: tab }); sinks.forEach((s) => s.page(tab)); };
  window.ptEvent = (name) => { queue.push({ event: name }); sinks.forEach((s) => s.event(name)); };
  const attach = (sink) => {
    sinks.push(sink);
    for (const item of queue) { if (item.page) sink.page(item.page); else sink.event(item.event); }
  };

  if (params.has("me")) {
    const note = document.createElement("div");
    note.className = "toast";
    note.textContent = params.get("me") === "0" ? "This browser is counted again." : "This browser is excluded from visitor counts.";
    document.addEventListener("DOMContentLoaded", () => {
      document.body.append(note);
      setTimeout(() => note.remove(), 5000);
    });
  }
  if (ignored || local || doNotTrack) return;

  const script = (src, attrs = {}) => {
    const s = document.createElement("script");
    s.async = true;
    s.src = src;
    for (const [k, v] of Object.entries(attrs)) s.setAttribute(k, v);
    document.head.append(s);
    return s;
  };
  const title = (tab) => `Paper Trader · ${tab}`;

  if (cfg.goatcounter) {
    state.providers.push("goatcounter");
    window.goatcounter = { no_onload: true, endpoint: `https://${cfg.goatcounter}.goatcounter.com/count` };
    const gc = { pages: [], events: [], ready: false };
    const count = (o) => window.goatcounter && window.goatcounter.count && window.goatcounter.count(o);
    const flush = () => {
      gc.ready = true;
      gc.pages.forEach((t) => count({ path: `/${t}`, title: title(t) }));
      gc.events.forEach((e) => count({ path: e, title: e, event: true }));
    };
    script("https://gc.zgo.at/count.js").addEventListener("load", flush);
    attach({
      page: (t) => (gc.ready ? count({ path: `/${t}`, title: title(t) }) : gc.pages.push(t)),
      event: (e) => (gc.ready ? count({ path: e, title: e, event: true }) : gc.events.push(e)),
    });
  }

  if (cfg.ga4) {
    state.providers.push("ga4");
    window.dataLayer = window.dataLayer || [];
    window.gtag = function gtag() { window.dataLayer.push(arguments); };
    window.gtag("consent", "default", { analytics_storage: "denied", ad_storage: "denied", ad_user_data: "denied", ad_personalization: "denied" });
    window.gtag("js", new Date());
    window.gtag("config", cfg.ga4, { send_page_view: false, anonymize_ip: true });
    script(`https://www.googletagmanager.com/gtag/js?id=${encodeURIComponent(cfg.ga4)}`);
    const grant = () => window.gtag("consent", "update", { analytics_storage: "granted" });
    const choice = safe(() => localStorage.getItem(CONSENT), null);
    if (choice === "yes") grant();
    if (choice === null) {
      document.addEventListener("DOMContentLoaded", () => {
        const bar = document.createElement("div");
        bar.className = "consent";
        bar.setAttribute("role", "dialog");
        bar.setAttribute("aria-label", "Visit statistics");
        const text = document.createElement("span");
        text.textContent = "Count this visit with Google Analytics? It helps me see which tabs people use and roughly where they are. No names or accounts are involved.";
        const yes = document.createElement("button");
        yes.textContent = "Yes, count it";
        const no = document.createElement("button");
        no.textContent = "No thanks";
        no.className = "quiet";
        const close = (value) => { safe(() => localStorage.setItem(CONSENT, value)); if (value === "yes") grant(); bar.remove(); };
        yes.addEventListener("click", () => close("yes"));
        no.addEventListener("click", () => close("no"));
        bar.append(text, yes, no);
        document.body.append(bar);
      });
    }
    attach({
      page: (t) => window.gtag("event", "page_view", { page_title: title(t), page_path: `/${t}`, page_location: `${location.origin}${location.pathname}#${t}` }),
      event: (e) => window.gtag("event", e),
    });
  }
})();
