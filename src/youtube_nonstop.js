/**
 * @file youtube_nonstop.js
 * @description "YouTube không dừng" của Chỏme — tự xử lý hộp "Video đã tạm dừng. Tiếp tục xem?"
 * để nghe nhạc không bị ngắt khi bạn để máy lâu không thao tác. Ý tưởng tham khảo extension
 * "YouTube NonStop" (Nikos Ioannou, giấy phép MIT); mã viết lại cho Chỏme.
 *   1. Khi YouTube tự gọi tạm dừng lúc người dùng đang không thao tác (≥ 5 giây) → hoãn lại.
 *      Nếu người dùng bấm chuột/phím trong 5 giây sau đó thì mới tạm dừng thật.
 *   2. Khi hộp "Tiếp tục xem?" (YouTube) / "Bạn còn đó không?" (YouTube Music) hiện ra lúc
 *      người dùng không thao tác → tự đóng hộp và phát tiếp.
 *   Tạm dừng bằng chuột, bàn phím hoặc phím media (tai nghe) vẫn hoạt động bình thường.
 */
(() => {
  'use strict';
  if (window.__chomeNonStop) return;
  window.__chomeNonStop = true;

  const MUSIC = location.hostname === 'music.youtube.com';
  const DIALOG = MUSIC ? 'YTMUSIC-YOU-THERE-RENDERER' : 'YT-CONFIRM-DIALOG-RENDERER';
  const IDLE_MS = 5000;
  const PAUSE_WINDOW_MS = 5000;

  let lastInteraction = Date.now();
  let pauseRequested = false;
  let pauseTimer = 0;
  let video = null;

  const idle = () => Date.now() - lastInteraction >= IDLE_MS;

  const realPause = () => {
    pauseRequested = false;
    clearTimeout(pauseTimer);
    if (video && video.__chomePause) video.__chomePause();
  };

  // Người dùng thao tác: nếu YouTube vừa xin tạm dừng thì giờ mới dừng thật.
  const onInteraction = () => {
    if (pauseRequested) {
      realPause();
      return;
    }
    lastInteraction = Date.now();
  };
  for (const type of ['pointerdown', 'pointerup', 'keydown', 'keyup']) {
    document.addEventListener(type, onInteraction, true);
  }

  // Phím media "pause" (tai nghe, bàn phím) là thao tác thật → luôn dừng.
  const guardMediaKeys = () => {
    const ms = navigator.mediaSession;
    if (!ms || ms.__chomeGuarded) return;
    ms.__chomeGuarded = true;
    const setHandler = ms.setActionHandler.bind(ms);
    setHandler('pause', realPause);
    ms.setActionHandler = (action, fn) => {
      if (action === 'pause') return; // giữ handler của Chỏme
      setHandler(action, fn);
    };
  };

  // Bọc video.pause(): YouTube tự gọi lúc người dùng không thao tác → hoãn lại.
  const hookVideo = () => {
    const v = document.querySelector('video');
    if (!v || v.__chomePause) return;
    video = v;
    v.__chomePause = v.pause.bind(v);
    v.pause = () => {
      if (!idle()) {
        realPause();
        return;
      }
      pauseRequested = true;
      clearTimeout(pauseTimer);
      pauseTimer = setTimeout(() => { pauseRequested = false; }, PAUSE_WINDOW_MS);
    };
    guardMediaKeys();
  };

  // Hộp "Tiếp tục xem?": đóng và phát tiếp nếu người dùng đang không thao tác.
  document.addEventListener('yt-popup-opened', (event) => {
    const name = event.detail && event.detail.nodeName;
    if (name !== DIALOG || !idle()) return;
    const container = document.querySelector(MUSIC ? 'ytmusic-popup-container' : 'ytd-popup-container');
    const confirm = container && container.querySelector('#confirm-button button, #confirm-button, tp-yt-paper-button#button');
    if (confirm) confirm.click();
    else if (container) container.click();
    if (video) video.play().catch(() => {});
  });

  // Trang YouTube là ứng dụng một trang: <video> có thể được tạo/đổi bất cứ lúc nào.
  // Gộp lại tối đa một lần mỗi khung hình (YouTube thay đổi DOM rất thường xuyên).
  let scheduled = false;
  new MutationObserver(() => {
    if (scheduled || (video && video.isConnected)) return;
    scheduled = true;
    requestAnimationFrame(() => { scheduled = false; hookVideo(); });
  }).observe(document.documentElement, { childList: true, subtree: true });
  hookVideo();
})();
