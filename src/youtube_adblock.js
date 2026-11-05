/**
 * @file youtube_adblock.js
 * @description Chặn quảng cáo YouTube cho Chỏme. Được nhúng vào youtube.com
 * trước mọi script của trang (document-start, ngữ cảnh của trang). Ba lớp:
 *   1. Xóa dữ liệu quảng cáo (adPlacements, adSlots, playerAds…) khỏi phản hồi
 *      của trình phát — quảng cáo không bao giờ được lên lịch.
 *   2. (CSS đi kèm, xem youtube_adblock.css) ẩn khung quảng cáo trên trang.
 *   3. Lưới an toàn: nếu quảng cáo vẫn chạy, tắt tiếng + tua hết + bấm "Bỏ qua".
 */
(() => {
  'use strict';
  if (window.__liteYtAdblock) return;
  window.__liteYtAdblock = true;

  const AD_KEYS = ['adPlacements', 'adSlots', 'playerAds', 'adBreakHeartbeatParams'];

  // ---------- 1. Xóa dữ liệu quảng cáo ----------
  const stripKeys = (o) => {
    if (!o || typeof o !== 'object') return;
    for (const k of AD_KEYS) {
      if (Object.prototype.hasOwnProperty.call(o, k)) delete o[k];
    }
  };

  // Chỉ xét các vị trí YouTube thật sự đặt dữ liệu quảng cáo (không duyệt sâu toàn bộ
  // object) để JSON.parse — vốn được gọi rất nhiều — vẫn nhanh.
  const prune = (value) => {
    if (!value || typeof value !== 'object') return value;
    const items = Array.isArray(value) ? value : [value];
    for (const it of items) {
      if (!it || typeof it !== 'object') continue;
      stripKeys(it);
      stripKeys(it.playerResponse);
      const prefetch = it.endpoint && it.endpoint.watchEndpoint &&
        it.endpoint.watchEndpoint.unserializedPrefetchData;
      if (prefetch) stripKeys(prefetch.playerResponse);
    }
    return value;
  };

  // Giữ hàm bọc trông như hàm gốc (một số kiểm tra chống chặn quảng cáo dùng toString).
  const disguise = (wrapper, original) => {
    try {
      Object.defineProperty(wrapper, 'name', { value: original.name });
      wrapper.toString = Function.prototype.toString.bind(original);
    } catch (_) { /* bỏ qua */ }
    return wrapper;
  };

  const nativeParse = JSON.parse;
  JSON.parse = disguise(function parse(text, reviver) {
    return prune(nativeParse.call(this, text, reviver));
  }, nativeParse);

  const nativeJson = Response.prototype.json;
  Response.prototype.json = disguise(function json() {
    return nativeJson.call(this).then(prune);
  }, nativeJson);

  // ytInitialPlayerResponse được gán bằng script nhúng trong HTML trang xem video.
  for (const name of ['ytInitialPlayerResponse', 'playerResponse']) {
    let stored;
    try {
      Object.defineProperty(window, name, {
        configurable: true,
        get: () => stored,
        set: (v) => { stored = prune(v); },
      });
    } catch (_) { /* bỏ qua */ }
  }

  // ---------- 3. Lưới an toàn ----------
  const SKIP_BUTTONS = '.ytp-skip-ad-button, .ytp-ad-skip-button, .ytp-ad-skip-button-modern';

  const handleAd = () => {
    const player = document.querySelector('#movie_player');
    if (!player || !player.classList.contains('ad-showing')) return;
    const video = player.querySelector('video');
    if (video) {
      video.muted = true;
      if (Number.isFinite(video.duration) && video.duration > 0) {
        video.currentTime = video.duration;
      }
    }
    const skip = document.querySelector(SKIP_BUTTONS);
    if (skip) skip.click();
  };

  // Hộp thoại "Trình chặn quảng cáo vi phạm điều khoản": gỡ đi và cho video chạy tiếp.
  const handleEnforcement = () => {
    const dialog = document.querySelector('ytd-enforcement-message-view-model');
    if (!dialog) return;
    const popup = dialog.closest('tp-yt-paper-dialog');
    (popup || dialog).remove();
    const backdrop = document.querySelector('tp-yt-iron-overlay-backdrop');
    if (backdrop) backdrop.remove();
    const video = document.querySelector('#movie_player video');
    if (video && video.paused) video.play().catch(() => {});
  };

  let scheduled = false;
  const check = () => {
    if (scheduled) return;
    scheduled = true;
    requestAnimationFrame(() => {
      scheduled = false;
      handleAd();
      handleEnforcement();
    });
  };

  const start = () => {
    new MutationObserver(check).observe(document.documentElement, {
      subtree: true,
      childList: true,
      attributes: true,
      attributeFilter: ['class'],
    });
    check();
  };

  if (document.documentElement) start();
  else document.addEventListener('DOMContentLoaded', start, { once: true });
})();
