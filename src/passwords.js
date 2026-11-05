/**
 * @file passwords.js
 * @description Lưu mật khẩu & tự điền của Chỏme. Chạy trong thế giới JavaScript riêng của
 * Chỏme (trang web không đọc được mã này và không gọi được các handler).
 *   - Phát hiện đăng nhập: khi gửi form có ô mật khẩu đã nhập → gửi lên trình duyệt để hỏi lưu.
 *   - Tự điền: chỉ ở khung chính, trang an toàn (https / localhost), form đăng nhập có đúng một
 *     ô mật khẩu; chỉ hỏi kho khóa khi trang thật sự có ô mật khẩu, mỗi trang tối đa một lần.
 */
(function () {
  'use strict';

  const save = window.webkit && window.webkit.messageHandlers && window.webkit.messageHandlers.chomePasswordSave;
  const query = window.webkit && window.webkit.messageHandlers && window.webkit.messageHandlers.chomePasswordQuery;
  if (!save || !query || !/^https?:$/.test(location.protocol)) return;

  const USER_TYPES = /^(text|email|tel|username)$/i;
  const sent = new Set();
  let fillAsked = false;

  const inputs = (root) => Array.from((root || document).querySelectorAll('input'));
  const visible = (el) => !!(el.offsetWidth || el.offsetHeight || el.getClientRects().length);
  const passwordInputs = (root) => inputs(root).filter((i) => i.type === 'password' && visible(i));
  const filledPasswords = (root) => passwordInputs(root).filter((i) => i.value);

  // Ô tên đăng nhập: ô chữ/email đứng ngay trước ô mật khẩu (ưu tiên), hoặc ô cuối cùng.
  function usernameInput(form, password, needValue) {
    const candidates = inputs(form).filter((i) => i !== password && USER_TYPES.test(i.type || 'text') && visible(i) && (!needValue || i.value));
    if (!candidates.length) return null;
    const before = candidates.filter((i) => i.compareDocumentPosition(password) & Node.DOCUMENT_POSITION_FOLLOWING);
    return before.length ? before[before.length - 1] : candidates[candidates.length - 1];
  }

  function report(form) {
    const password = filledPasswords(form)[0];
    if (!password) return;
    const user = usernameInput(form, password, true);
    const username = user ? user.value : '';
    const key = location.origin + '\n' + username + '\n' + password.value;
    if (sent.has(key)) return;
    sent.add(key);
    save.postMessage({ type: 'credentials', origin: location.origin, username: username, password: password.value });
  }

  // Tự điền: khung chính, trang an toàn, form đăng nhập (đúng 1 ô mật khẩu, đang trống).
  function maybeFill() {
    if (fillAsked || window.top !== window) return;
    const secure = location.protocol === 'https:' || /^(localhost|127\.0\.0\.1|\[::1\])$/.test(location.hostname);
    if (!secure) return;
    const password = passwordInputs(document)[0];
    if (!password || password.value) return;
    const form = password.form || password.closest('form') || document;
    if (passwordInputs(form).length !== 1) return; // đăng ký / đổi mật khẩu → không tự điền
    fillAsked = true;
    query.postMessage({ type: 'query', origin: location.origin }).then((reply) => {
      if (!reply || !reply.found || !reply.password || password.value) return;
      const setNative = (input, value) => {
        const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set;
        setter.call(input, value);
        input.dispatchEvent(new Event('input', { bubbles: true }));
        input.dispatchEvent(new Event('change', { bubbles: true }));
      };
      const user = usernameInput(form, password, false);
      if (user && reply.username && !user.value) setNative(user, reply.username);
      setNative(password, reply.password);
    }).catch(() => {});
  }

  // Gửi form (cách đăng nhập chuẩn).
  document.addEventListener('submit', (event) => {
    const form = event.target instanceof HTMLFormElement ? event.target : document;
    report(form);
  }, true);

  // Nút gửi: <button> mặc định (type=submit) / input[type=submit] trong form; nút kiểu
  // type=button chỉ tính khi trang không dùng <form> (đăng nhập bằng JS). Nhờ vậy bấm nút
  // "hiện mật khẩu" (thường là type=button trong form) không bị hỏi lưu nhầm.
  document.addEventListener('click', (event) => {
    const target = event.target.closest && event.target.closest('button, input[type=submit], [role=button]');
    if (!target) return;
    const form = target.form || target.closest('form');
    const isSubmit = target.matches('input[type=submit], button:not([type]), button[type=submit]');
    if (form && !isSubmit) return;
    report(form || document);
  }, true);

  // Ô mật khẩu có thể xuất hiện muộn (trang dựng bằng JS) → thử tự điền khi nó xuất hiện.
  maybeFill();
  if (!fillAsked) {
    let timer = 0;
    const observer = new MutationObserver(() => {
      clearTimeout(timer);
      timer = setTimeout(() => {
        maybeFill();
        if (fillAsked) observer.disconnect();
      }, 300);
    });
    observer.observe(document.documentElement, { childList: true, subtree: true });
    setTimeout(() => observer.disconnect(), 30000); // không theo dõi mãi
  }
})();
