/**
 * @file devtools.js
 * @description Chỉnh sửa nhỏ cho DevTools (Web Inspector của WebKit) để giống Chrome hơn.
 * Được chèn vào Main.html của DevTools (ngay sau Main.js) qua G_RESOURCE_OVERLAYS.
 *   - Tab Network / Sources: phản hồi JSON tự hiện dạng cây ("Response (Object Tree)")
 *     thay vì chữ thô — như ô Preview của Chrome. Người dùng vẫn chọn lại "Response"
 *     để xem chữ nếu muốn.
 */
(() => {
  'use strict';
  const C = window.WI && WI.ResourceClusterContentView;
  if (!C || !C.Identifier || !C.prototype._createPathComponent) return;
  const ID = C.Identifier;

  const createPathComponent = C.prototype._createPathComponent;
  C.prototype._createPathComponent = function (options) {
    const component = createPathComponent.call(this, options);
    if (options && options.identifier === ID.ResponseJSON && !this.__chomeTreeShown) {
      this.__chomeTreeShown = true;
      // Đợi WebKit gắn xong lựa chọn mới rồi mới chuyển sang dạng cây — trừ khi người dùng
      // đã chọn một chế độ khác "Response" (chữ thô) cho tài nguyên này.
      setTimeout(() => {
        const setting = this._currentContentViewSetting;
        if (!setting || setting.value === ID.Response) {
          this._showContentViewForIdentifier(ID.ResponseJSON);
        }
      }, 0);
    }
    return component;
  };

  // ---- Cây JSON gọn như Chrome: không có ô chữ cái N/O/S, giữ đúng thứ tự khóa ----
  const JSONView = WI.LocalJSONContentView;
  const OTV = WI.ObjectTreeView;
  if (JSONView && OTV) {
    JSONView.prototype.renderRemoteObject = function (remoteObject) {
      const tree = new OTV(remoteObject, OTV.Mode.Properties, null, true);
      tree.__chomeJson = true;
      if (tree._outline) tree._outline.__chomeJson = true; // các nút con dùng chung outline này
      tree.showOnlyJSON();
      tree.expand();
      this.element.classList.add('chome-json-tree');
      this.element.appendChild(tree.element);
    };

    // WebKit xếp khóa theo bảng chữ cái; Chrome giữ thứ tự trong dữ liệu. Với cây JSON,
    // tạm thay hàm so sánh bằng "giữ nguyên" (sort của JSC ổn định → giữ thứ tự gốc).
    const keepOrder = (original) => function (...args) {
      const isJson = this.__chomeJson || (this.treeOutline && this.treeOutline.__chomeJson);
      if (!isJson) return original.apply(this, args);
      const compare = OTV.comparePropertyDescriptors;
      OTV.comparePropertyDescriptors = () => 0;
      try {
        return original.apply(this, args);
      } finally {
        OTV.comparePropertyDescriptors = compare;
      }
    };
    for (const cls of [OTV, WI.ObjectTreePropertyTreeElement]) {
      if (cls && cls.prototype._updateProperties) {
        cls.prototype._updateProperties = keepOrder(cls.prototype._updateProperties);
      }
    }

    const style = document.createElement('style');
    style.textContent = `
      .chome-json-tree .object-tree-property > .icon { display: none !important; }
      .chome-json-tree { padding: 4px 6px; }
      .chome-json-tree .object-tree, .chome-json-tree .tree-outline { font-size: 12px; }
      .chome-json-tree .property-name { color: #881391; }
      .chome-json-tree .formatted-string { color: #c41a16; }
      .chome-json-tree .formatted-number { color: #1c00cf; }
      .chome-json-tree .formatted-null { color: #80868b; }
      @media (prefers-color-scheme: dark) {
        .chome-json-tree .property-name { color: #d197f8; }
        .chome-json-tree .formatted-string { color: #f28b54; }
        .chome-json-tree .formatted-number { color: #9980ff; }
        .chome-json-tree .formatted-null { color: #9aa0a6; }
      }`;
    document.head.appendChild(style);
  }

  window.__chomeDevTools = true;
})();
