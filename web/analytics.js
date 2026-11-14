"use strict";
/*
 * Optional visitor analytics for the dashboard.
 *
 * Two providers are supported and either, both or neither can be turned on from
 * analytics-config.js:
 *   goatcounter  a cookie-free counter. Needs no consent banner. Shows pages, referrers,
 *                browsers, systems, screen sizes and countries.
 *   ga4          Google Analytics 4. Adds city-level location, new versus returning
 *                visitors and a live view. Runs without a prompt, and in cookieless mode
 *                for visitors in Europe.
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
    // No prompt anywhere. Outside Europe, Google Analytics runs normally. Inside the EEA,
    // the UK and Switzerland, where cookies need prior consent, it runs in Google's
    // cookieless mode: no cookie is stored and visits are counted without identifiers.
    const EUROPE = ["AT", "BE", "BG", "HR", "CY", "CZ", "DK", "EE", "FI", "FR", "DE", "GR", "HU", "IS", "IE", "IT", "LV", "LI", "LT", "LU", "MT", "NL", "NO", "PL", "PT", "RO", "SK", "SI", "ES", "SE", "CH", "GB"];
    window.gtag("consent", "default", { analytics_storage: "denied", ad_storage: "denied", ad_user_data: "denied", ad_personalization: "denied", region: EUROPE });
    window.gtag("consent", "default", { analytics_storage: "granted", ad_storage: "denied", ad_user_data: "denied", ad_personalization: "denied" });
    window.gtag("js", new Date());
    window.gtag("config", cfg.ga4, { send_page_view: false, anonymize_ip: true, allow_google_signals: false, allow_ad_personalization_signals: false });
    script(`https://www.googletagmanager.com/gtag/js?id=${encodeURIComponent(cfg.ga4)}`);
    attach({
      page: (t) => window.gtag("event", "page_view", { page_title: title(t), page_path: `/${t}`, page_location: `${location.origin}${location.pathname}#${t}` }),
      event: (e) => window.gtag("event", e),
    });
  }
})();
